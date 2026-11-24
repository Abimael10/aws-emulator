// SPDX-License-Identifier: CC0-1.0

//! The whole SNAPSHOT/RESTORE round trip of a serverless cache, driven the way a client
//! with a budget drives it: one read per step.
//!
//! The rule under test: each step of a serverless cache's life answers with the transient
//! status the service documents for that phase, and the change the clock owes that step is
//! materialised by whichever later call reads the record — so the FIRST read a client
//! issues once a step's change has fallen due already reports the settled state (the cache
//! `available` at an endpoint it answers on, its snapshot `available` with an ARN, the
//! deleted cache's absence as 404 `ServerlessCacheNotFoundFault`), and a cache created from
//! that snapshot's ARN serves, on the endpoint it publishes, exactly the keyspace the
//! snapshot captured. A flow of N such steps therefore costs N reads, not N poll intervals,
//! which is what makes the round trip observable inside a client's budget at all.
//!
//! The rows span what the rule generalises over: the ENGINE the cache runs and its version
//! line, whether the cache is placed in subnets of the account behind a security group or
//! named no placement at all, and — along the inner table — the SIZE and SHAPE of what was
//! written before the snapshot, plus a key written AFTER it, which the snapshot does not
//! hold. The recorded flow (one `redis` cache in two subnets, one key, one snapshot, one
//! restore) is one row of it.
//!
//! Nothing here sleeps and nothing polls: the change the clock owes is made due through the
//! store ([`crate::harness::release_pending_changes`]) and ONE read follows each step, so
//! what this suite asserts does not depend on how fast the host runs. Every assertion goes
//! over a wire — the control plane through the same router the production emulator mounts,
//! the data plane over the TLS socket the core opened, dialled at the address that control
//! plane published.
//!
//! The companions of this suite are [`crate::first_read_sees_the_settled_state`], which
//! asserts the one-read rule per RESOURCE rather than along one flow, and
//! [`crate::cache_data_consequences`], which asserts what a restore carries over a client's
//! own poll loop.

use crate::cache_cluster_lifecycle::{account_subnet_ids, subnet_pairs};
use crate::cache_data_plane::{command, endpoint_of, open_tls, Resp, Session};
use crate::harness::{
    assert_own_endpoint, assert_refusal, params, release_pending_changes, Emulator, Reply,
};

/// One row of the flow table: the cache to stand up and where it is placed.
#[derive(Debug, Clone, Copy)]
struct Flow {
    /// What this row is, named in every assertion.
    case: &'static str,
    /// The prefix the row's cache, snapshot and restored cache are named with.
    prefix: &'static str,
    /// The engine pairs the create sends: an engine this emulator serves RESP for, and the
    /// version line asked for it.
    engine: &'static [(&'static str, &'static str)],
    /// Whether the row names the account's own subnets and a security group — the placement
    /// an in-VPC client's cache has — or names none and takes the service's default.
    placed: bool,
}

const FLOWS: &[Flow] = &[
    Flow {
        case: "redis on its default version, no placement named",
        prefix: "trip-redis-default",
        engine: &[("Engine", "redis")],
        placed: false,
    },
    Flow {
        case: "redis in the account's subnets behind a security group",
        prefix: "trip-redis-placed",
        engine: &[("Engine", "redis")],
        placed: true,
    },
    Flow {
        case: "valkey 8 in the account's subnets behind a security group",
        prefix: "trip-valkey-placed",
        engine: &[("Engine", "valkey"), ("MajorEngineVersion", "8")],
        placed: true,
    },
];

/// One key written into the source cache before its snapshot is taken.
///
/// The rows differ along what the restore rule generalises over: the SIZE of the value
/// (none, a word, more than one frame's worth of bytes) and the shape of the key name.
struct Written {
    case: &'static str,
    key: &'static str,
    value: String,
}

/// The keys every row of [`FLOWS`] writes before its snapshot.
fn written() -> Vec<Written> {
    vec![
        Written {
            case: "a word",
            key: "session:original",
            value: "serverless-original-value".to_string(),
        },
        Written {
            case: "an empty value",
            key: "empty",
            value: String::new(),
        },
        Written {
            case: "a value longer than one frame",
            key: "bulk",
            // 9 KiB, so the reply is read over more than one socket read.
            value: "0123456789abcdef".repeat(576),
        },
        Written {
            case: "a key name with separators",
            key: "tenant:42/profile",
            value: "{\"user\":\"42\"}".to_string(),
        },
    ]
}

/// The key written AFTER the snapshot was taken, which the snapshot therefore does not
/// hold: what pins a restore to the INSTANT its snapshot captured.
const AFTER_THE_SNAPSHOT: &str = "written-after-the-snapshot";

/// The placement a row sends: the account's first two subnets and one security group of
/// its own, asked of Amazon EC2 — the service that owns both — rather than written here.
async fn placement(emulator: &Emulator, flow: &Flow) -> Vec<(String, String)> {
    if !flow.placed {
        return Vec::new();
    }
    let subnets = account_subnet_ids(emulator).await;
    let group = emulator
        .security_groups(flow.prefix, 1)
        .await
        .pop()
        .expect("EC2 must mint the security group the cache is guarded by");
    let mut pairs = subnet_pairs("SubnetId", &subnets[..2]);
    pairs.push(("SecurityGroupIds.SecurityGroupId.1".to_string(), group));
    pairs
}

/// Create one serverless cache of `flow`, restoring from `restore_from`, and assert the
/// call that starts the work answers with the transient status that phase documents.
async fn create_cache(
    emulator: &Emulator,
    flow: &Flow,
    name: &str,
    place: &[(String, String)],
    restore_from: &[String],
) -> Reply {
    let mut pairs = params(&[("ServerlessCacheName", name)]);
    pairs.extend(params(flow.engine));
    pairs.extend(place.iter().cloned());
    for (index, arn) in restore_from.iter().enumerate() {
        pairs.push((
            format!("SnapshotArnsToRestore.SnapshotArn.{}", index + 1),
            arn.clone(),
        ));
    }
    let created = emulator.call("CreateServerlessCache", &pairs).await;
    assert_eq!(
        created.status, 200,
        "case {:?}: CreateServerlessCache {name} must answer 200, got {} {}",
        flow.case, created.status, created.body
    );
    // "The allowed values are CREATING, AVAILABLE, DELETING, CREATE-FAILED and MODIFYING":
    // a cache that has just been asked for is not ready, so the create's own answer is the
    // transient status and not the settled one.
    assert_eq!(
        created.text("Status"),
        Some("creating"),
        "case {:?}: the create of {name} must answer the cache creating, got {}",
        flow.case,
        created.body
    );
    created
}

/// The ONE `DescribeServerlessCaches` a client issues once the cache's owed change has
/// fallen due, asserted to report the settled cache at an endpoint of this emulator's own.
async fn one_read_to_available(emulator: &Emulator, flow: &Flow, name: &str) -> Reply {
    release_pending_changes(emulator);
    let described = emulator
        .call(
            "DescribeServerlessCaches",
            &params(&[("ServerlessCacheName", name)]),
        )
        .await;
    assert_eq!(
        described.status, 200,
        "case {:?}: the first read of {name} past its owed change must answer 200, got {} {}",
        flow.case, described.status, described.body
    );
    assert_eq!(
        described.text("ServerlessCacheName"),
        Some(name),
        "case {:?}: the read must report the cache the create registered, got {}",
        flow.case,
        described.body
    );
    assert_eq!(
        described.text("Status"),
        Some("available"),
        "case {:?}: the FIRST read of {name} past its owed change must report it available, \
         got {:?} in {}",
        flow.case,
        described.text("Status"),
        described.body
    );
    described
}

/// A TLS RESP session on the endpoint `described` publishes for the role under
/// `address`/`port`, which is the wire a client of a serverless cache speaks.
///
/// The endpoint is asserted to be one of the EMULATOR's own before it is dialled: an
/// endpoint a service advertises is an address the service answers on.
async fn session_on(flow: &Flow, described: &Reply, role: &str, member: &str) -> Session {
    let (address, port) = endpoint_of(
        described,
        &format!("{member}/Address"),
        &format!("{member}/Port"),
        flow.case,
    );
    assert_own_endpoint(flow.case, role, &address, Some(port.as_str()));
    open_tls(flow.case, &address, &port).await
}

/// Write every row of [`written`] on `session`, as a client fills the cache it is about to
/// snapshot.
async fn write_all(flow: &Flow, session: &mut Session, rows: &[Written]) {
    for row in rows {
        let reply = command(flow.case, session, &["SET", row.key, &row.value]).await;
        assert_eq!(
            reply,
            Resp::Status("OK".to_string()),
            "case {:?}/{}: a SET on the published endpoint must be accepted, got {reply:?}",
            flow.case,
            row.case
        );
    }
}

/// Assert `session` answers every row of [`written`], and nothing for `absent`.
async fn assert_reads_back(flow: &Flow, session: &mut Session, rows: &[Written], absent: &[&str]) {
    for row in rows {
        let reply = command(flow.case, session, &["GET", row.key]).await;
        assert_eq!(
            reply,
            Resp::Bulk(row.value.clone()),
            "case {:?}/{}: GET {} must answer the stored value ({} bytes), got {reply:?}",
            flow.case,
            row.case,
            row.key,
            row.value.len()
        );
    }
    for key in absent {
        let reply = command(flow.case, session, &["GET", key]).await;
        assert_eq!(
            reply,
            Resp::Nil,
            "case {:?}: {key} is not in this cache's data set, got {reply:?}",
            flow.case
        );
    }
}

/// Take one snapshot of `cache`, read it back with the ONE read past its owed change, and
/// answer the ARN it publishes.
async fn snapshot_in_one_read(emulator: &Emulator, flow: &Flow, name: &str, cache: &str) -> String {
    let created = emulator
        .call(
            "CreateServerlessCacheSnapshot",
            &params(&[
                ("ServerlessCacheSnapshotName", name),
                ("ServerlessCacheName", cache),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {:?}: CreateServerlessCacheSnapshot must answer 200, got {} {}",
        flow.case, created.status, created.body
    );
    assert_eq!(
        created.text("Status"),
        Some("creating"),
        "case {:?}: the call that takes the snapshot must answer it creating, got {}",
        flow.case,
        created.body
    );
    assert_eq!(
        created.text("ServerlessCacheName"),
        Some(cache),
        "case {:?}: the snapshot must name the cache it was taken of, got {}",
        flow.case,
        created.body
    );

    release_pending_changes(emulator);
    let described = emulator
        .call(
            "DescribeServerlessCacheSnapshots",
            &params(&[("ServerlessCacheSnapshotName", name)]),
        )
        .await;
    assert_eq!(
        described.status, 200,
        "case {:?}: the first read of the snapshot {name} must answer 200, got {} {}",
        flow.case, described.status, described.body
    );
    assert_eq!(
        described.text("Status"),
        Some("available"),
        "case {:?}: the FIRST read of the snapshot {name} past its owed change must report it \
         available, got {:?} in {}",
        flow.case,
        described.text("Status"),
        described.body
    );
    let arn = described
        .text("ARN")
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: the snapshot must publish the ARN a restore names it by, got {}",
                flow.case, described.body
            )
        })
        .to_string();
    // The ARN is the handle a restore is given: it has to be this account's ARN of THIS
    // snapshot, which is what makes the restore below address the snapshot just taken.
    assert!(
        arn.starts_with("arn:") && arn.ends_with(name),
        "case {:?}: the snapshot's ARN must name {name}, got {arn:?}",
        flow.case
    );
    arn
}

/// Delete `name` and assert the ONE read past the deletion's own due instant finds it gone:
/// 404 `ServerlessCacheNotFoundFault`, carrying the sentence that fault documents.
async fn delete_in_one_read(emulator: &Emulator, flow: &Flow, name: &str) {
    let deleted = emulator
        .call(
            "DeleteServerlessCache",
            &params(&[("ServerlessCacheName", name)]),
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {:?}: DeleteServerlessCache must answer 200, got {} {}",
        flow.case, deleted.status, deleted.body
    );
    assert_eq!(
        deleted.text("Status"),
        Some("deleting"),
        "case {:?}: the delete must answer the cache deleting, got {}",
        flow.case,
        deleted.body
    );

    release_pending_changes(emulator);
    let gone = emulator
        .call(
            "DescribeServerlessCaches",
            &params(&[("ServerlessCacheName", name)]),
        )
        .await;
    assert_refusal(
        flow.case,
        &gone,
        404,
        "ServerlessCacheNotFoundFault",
        &format!("Serverless cache with name {name} not found."),
    );
}

#[tokio::test]
async fn a_serverless_snapshot_restore_round_trip_converges_on_one_read_per_step() {
    let emulator = Emulator::new();

    for flow in FLOWS {
        let place = placement(&emulator, flow).await;
        let rows = written();
        let source = format!("{}-source", flow.prefix);
        let snapshot_name = format!("{}-snap", flow.prefix);
        let restored_name = format!("{}-restored", flow.prefix);

        // Step 1 — the cache, and the ONE read that finds it available.
        create_cache(&emulator, flow, &source, &place, &[]).await;
        let described = one_read_to_available(&emulator, flow, &source).await;

        // Step 2 — the data, written and read back over the TLS wire a serverless cache is
        // served on, at the address the control plane just published.
        let mut writer = session_on(flow, &described, "the cache's endpoint", "Endpoint").await;
        write_all(flow, &mut writer, &rows).await;
        assert_reads_back(flow, &mut writer, &rows, &[AFTER_THE_SNAPSHOT]).await;

        // Step 3 — the snapshot of that data set, and the ONE read that finds it available
        // with the ARN a restore addresses it by.
        let arn = snapshot_in_one_read(&emulator, flow, &snapshot_name, &source).await;

        // One more key, written AFTER the snapshot: it is in the cache and not in the
        // snapshot, which is what makes the restore below about the captured instant rather
        // than about the keys.
        let late = command(flow.case, &mut writer, &["SET", AFTER_THE_SNAPSHOT, "late"]).await;
        assert_eq!(
            late,
            Resp::Status("OK".to_string()),
            "case {:?}: the cache must take a key written after its snapshot, got {late:?}",
            flow.case
        );

        // Step 4 — the source cache goes, and the ONE read past the deletion finds the name
        // resolving to nothing at all.
        delete_in_one_read(&emulator, flow, &source).await;

        // Step 5 — the restore: a NEW cache created from nothing but that ARN, available to
        // the one read past its owed change, serving the snapshot's keyspace on the endpoint
        // it publishes — and not the key written after the snapshot was taken.
        create_cache(
            &emulator,
            flow,
            &restored_name,
            &place,
            ::std::slice::from_ref(&arn),
        )
        .await;
        let described = one_read_to_available(&emulator, flow, &restored_name).await;
        let mut restored =
            session_on(flow, &described, "the restored cache's endpoint", "Endpoint").await;
        assert_reads_back(flow, &mut restored, &rows, &[AFTER_THE_SNAPSHOT]).await;

        // The restored data is the whole cache's, not one socket's: the reader endpoint
        // answers the same keyspace, and still takes no write.
        let mut reader = session_on(
            flow,
            &described,
            "the restored cache's reader endpoint",
            "ReaderEndpoint",
        )
        .await;
        assert_reads_back(flow, &mut reader, &rows, &[AFTER_THE_SNAPSHOT]).await;
        let refused = command(flow.case, &mut reader, &["SET", "at-the-reader", "no"]).await;
        assert_eq!(
            refused.error_code(),
            Some("READONLY"),
            "case {:?}: a write on the restored cache's reader endpoint must be refused \
             READONLY, got {refused:?}",
            flow.case
        );

        // And the restored cache is its OWN cache: what a client writes into it now is not
        // in the snapshot it came from, so a second restore of that snapshot starts from the
        // data the snapshot captured.
        let own = format!("{}-own", flow.prefix);
        let written_now = command(flow.case, &mut restored, &["SET", &own, "mine"]).await;
        assert_eq!(
            written_now,
            Resp::Status("OK".to_string()),
            "case {:?}: the restored cache must take a write of its own, got {written_now:?}",
            flow.case
        );
        let second = format!("{}-again", flow.prefix);
        create_cache(
            &emulator,
            flow,
            &second,
            &place,
            ::std::slice::from_ref(&arn),
        )
        .await;
        let described = one_read_to_available(&emulator, flow, &second).await;
        let mut again =
            session_on(flow, &described, "the second restore's endpoint", "Endpoint").await;
        assert_reads_back(flow, &mut again, &rows, &[AFTER_THE_SNAPSHOT, &own]).await;
    }
}
