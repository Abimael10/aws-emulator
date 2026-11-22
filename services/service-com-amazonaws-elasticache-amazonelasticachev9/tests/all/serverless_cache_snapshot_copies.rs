// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `CopyServerlessCacheSnapshot`: "creates a copy of an existing
//! serverless cache's snapshot".
//!
//! Every assertion goes over the wire, through the same router the production emulator
//! mounts, using the shared `harness` request builder and XML reader — and the data set the
//! copy holds is read back over a real socket, from a serverless cache restored from the
//! COPY's ARN, so a copy that holds no data set cannot pass.
//!
//! The rule under test: a copy is a NEW serverless cache snapshot resource whose captured
//! content — the source cache's name, engine and major engine version, the instant the data
//! set was obtained at and the data set itself — is the SOURCE snapshot's, carried over
//! unchanged rather than retaken; only the copy's own identity
//! (`ServerlessCacheSnapshotName`, `ARN`), its fresh `creating` status, its manual type and
//! the members the copy request itself carries (`KmsKeyId`, `Tags`) are the copy's own. A
//! copy is refused, and stores nothing, when the account holds no snapshot under the source
//! name (the modelled not-found fault, HTTP 404), when the source has not settled yet and so
//! has no finished data set to copy (the modelled snapshot-state fault, HTTP 400), when the
//! target name is one the account already holds (the modelled already-exists fault, HTTP
//! 400), when the target name is not a snapshot name at all, and when the tags it carries
//! are not tags a resource may carry.
//!
//! The tests are tables: the rows differ along what the rule generalises over (the engine
//! the source cache runs, the case and the length of both names, which key encrypts the
//! copy, how many tags it carries, and — for the refusals — every way a copy cannot be
//! made), so no single recorded request is what makes them pass.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::cache_data_plane::{command, endpoint_of, open_tls, Resp};
use crate::harness::{
    hold_pending_changes, params, poll_until, release_pending_changes, settle_pending_changes,
    Emulator, RefusalCase, Reply, REGION,
};
use crate::resource_tags::list_tags;

/// A customer managed key, in the ARN form `KmsKeyId` accepts. KMS is not mounted beside
/// this service in these tests, so the identifier a request names is what is stored and
/// reported.
const SOURCE_KEY: &str =
    "arn:aws:kms:us-east-1:000000000000:key/11111111-2222-3333-4444-555555555555";
/// See [`SOURCE_KEY`]: a second key, named by the copy's own request.
const COPY_KEY: &str = "arn:aws:kms:us-east-1:000000000000:key/66666666-7777-8888-9999-aaaaaaaaaaaa";

/// A 63-character target name: `TargetServerlessCacheSnapshotName` "must be between 1 and
/// 255 characters", so a long name is as good a name as a short one.
const LONG_TARGET_NAME: &str = "a-serverless-cache-snapshot-copy-of-sixty-three-characters-long";

/// How ElastiCache words `ServerlessCacheSnapshotNotFoundFault` for a copy whose SOURCE
/// names no snapshot this account holds: the missing resource named as the SOURCE of the
/// copy, with no part of the request in it. Written out here on purpose: a test that built
/// the sentence from the same constant the handler reads would pass whatever that constant
/// said.
const SOURCE_NOT_FOUND: &str = "Source serverless cache snapshot does not exist.";

/// How the operation that acts on ONE named snapshot (`ExportServerlessCacheSnapshot`) words
/// the same fault — the class alone. A copy resolves two names, so it says WHICH one was not
/// resolved: the contrast row below pins that the two surfaces stay observable apart.
const SNAPSHOT_NOT_FOUND: &str = "Serverless cache snapshot with specified name does not exists.";

/// One row of the copy table: the serverless cache to create, the snapshot taken of it, and
/// the copy made from that snapshot.
#[derive(Debug, Clone, Copy)]
struct CopyCase {
    /// What the row is called in a failure message.
    case: &'static str,
    /// The `ServerlessCacheName` the create sends.
    cache_request_name: &'static str,
    /// The engine the source cache runs, which the snapshot's configuration reports back.
    engine: &'static str,
    /// The form pairs sent beside the name and the engine on the cache create.
    cache_extra: &'static [(&'static str, &'static str)],
    /// The `ServerlessCacheSnapshotName` the source snapshot is taken under.
    source_request_name: &'static str,
    /// The form pairs sent beside the two names when the source snapshot is taken.
    source_extra: &'static [(&'static str, &'static str)],
    /// The `TargetServerlessCacheSnapshotName` the copy is asked for, and the identity it is
    /// stored under — the name "is stored as a lowercase string".
    target_request_name: &'static str,
    target_stored_name: &'static str,
    /// The form pairs sent beside the two names on the copy.
    copy_extra: &'static [(&'static str, &'static str)],
    /// The `KmsKeyId` the copy must report: the one its own request named, else the source
    /// snapshot's, else none at all.
    kms_key_id: Option<&'static str>,
    /// The tags the copy must carry, as `(key, value)` pairs.
    tags: &'static [(&'static str, &'static str)],
}

/// The axis: the engine the source cache runs, the case and the length of both names, which
/// key encrypts the copy and how many tags it carries. The copy rule holds over all of them.
const CASES: &[CopyCase] = &[
    CopyCase {
        case: "redis, nothing but the two names",
        cache_request_name: "redis-copy-cache",
        engine: "redis",
        cache_extra: &[],
        source_request_name: "redis-copy-source",
        source_extra: &[],
        target_request_name: "redis-copy-target",
        target_stored_name: "redis-copy-target",
        copy_extra: &[],
        kms_key_id: None,
        tags: &[],
    },
    CopyCase {
        case: "valkey, a source encrypted with its own key, copied under a mixed-case name",
        cache_request_name: "Valkey-Copy-Cache",
        engine: "valkey",
        cache_extra: &[("Description", "the valkey one")],
        source_request_name: "Valkey-Copy-Source",
        source_extra: &[("KmsKeyId", SOURCE_KEY)],
        target_request_name: "Valkey-Copy-TARGET",
        target_stored_name: "valkey-copy-target",
        copy_extra: &[],
        // The copy named no key of its own, so the source snapshot's travels with it.
        kms_key_id: Some(SOURCE_KEY),
        tags: &[],
    },
    CopyCase {
        case: "memcached, a single-character target name, the copy's own key and two tags",
        cache_request_name: "MEMCACHED-COPY-CACHE",
        engine: "memcached",
        cache_extra: &[("SnapshotRetentionLimit", "7"), ("DailySnapshotTime", "03:00")],
        source_request_name: "memcached-copy-source",
        source_extra: &[("KmsKeyId", SOURCE_KEY)],
        target_request_name: "Z",
        target_stored_name: "z",
        copy_extra: &[
            ("KmsKeyId", COPY_KEY),
            ("Tags.Tag.1.Key", "team"),
            ("Tags.Tag.1.Value", "cache"),
            ("Tags.Tag.2.Key", "purpose"),
            ("Tags.Tag.2.Value", "retention"),
        ],
        // The copy named its own key, so that is the one it reports.
        kms_key_id: Some(COPY_KEY),
        tags: &[("team", "cache"), ("purpose", "retention")],
    },
    CopyCase {
        case: "redis, a 63-character target name and one tag",
        cache_request_name: "long-name-copy-cache",
        engine: "redis",
        cache_extra: &[],
        source_request_name: "long-name-copy-source",
        source_extra: &[],
        target_request_name: LONG_TARGET_NAME,
        target_stored_name: LONG_TARGET_NAME,
        copy_extra: &[("Tags.Tag.1.Key", "scope"), ("Tags.Tag.1.Value", "long")],
        kms_key_id: None,
        tags: &[("scope", "long")],
    },
];

/// Every `ServerlessCacheSnapshot` member that describes WHAT the snapshot captured, so a
/// copy has to report exactly what its source reports. `KmsKeyId` is deliberately absent:
/// `CopyServerlessCacheSnapshot$KmsKeyId` is "the identifier of the KMS key used to encrypt
/// the target snapshot", so it is the copy's own and is asserted per row.
const SAME_AS_SOURCE: [&str; 5] = [
    "ServerlessCacheName",
    "Engine",
    "MajorEngineVersion",
    "CreateTime",
    "BytesUsedForCache",
];

/// Create one serverless cache and insist the create itself succeeded.
async fn create_cache(
    emulator: &Emulator,
    case: &str,
    name: &str,
    engine: &str,
    extra: &[(&str, &str)],
) {
    let mut pairs = params(&[("ServerlessCacheName", name), ("Engine", engine)]);
    pairs.extend(params(extra));
    let created = emulator.call("CreateServerlessCache", &pairs).await;
    assert_eq!(
        created.status, 200,
        "case {case}: CreateServerlessCache must answer 200, got {} {}",
        created.status, created.body
    );
}

/// `CreateServerlessCacheSnapshot` of `cache` under `name`, with whatever else `extra` names.
async fn create_snapshot(
    emulator: &Emulator,
    case: &str,
    name: &str,
    cache: &str,
    extra: &[(&str, &str)],
) {
    let mut pairs = params(&[
        ("ServerlessCacheSnapshotName", name),
        ("ServerlessCacheName", cache),
    ]);
    pairs.extend(params(extra));
    let created = emulator
        .call("CreateServerlessCacheSnapshot", &pairs)
        .await;
    assert_eq!(
        created.status, 200,
        "case {case}: CreateServerlessCacheSnapshot must answer 200, got {} {}",
        created.status, created.body
    );
}

/// `CopyServerlessCacheSnapshot` from `source` to `target`, with whatever else `extra` names.
async fn copy_snapshot(
    emulator: &Emulator,
    source: &str,
    target: &str,
    extra: &[(&str, &str)],
) -> Reply {
    let mut pairs = params(&[
        ("SourceServerlessCacheSnapshotName", source),
        ("TargetServerlessCacheSnapshotName", target),
    ]);
    pairs.extend(params(extra));
    emulator
        .call("CopyServerlessCacheSnapshot", &pairs)
        .await
}

/// `DescribeServerlessCacheSnapshots` with the pairs a row sends.
async fn describe_snapshots(emulator: &Emulator, pairs: &[(&str, &str)]) -> Reply {
    emulator
        .call("DescribeServerlessCacheSnapshots", &params(pairs))
        .await
}

/// Read one snapshot back by name, asserting the describe itself succeeded.
async fn describe_snapshot(emulator: &Emulator, case: &str, name: &str) -> Reply {
    let described = describe_snapshots(emulator, &[("ServerlessCacheSnapshotName", name)]).await;
    assert_eq!(
        described.status, 200,
        "case {case}: DescribeServerlessCacheSnapshots must answer 200, got {} {}",
        described.status, described.body
    );
    described
}

/// Every snapshot name the store holds, sorted, as the describe of them all reports it.
async fn stored_names(emulator: &Emulator) -> Vec<String> {
    let all = describe_snapshots(emulator, &[]).await;
    assert_eq!(
        all.status, 200,
        "the describe of every snapshot must answer 200, got {} {}",
        all.status, all.body
    );
    let mut names: Vec<String> = all
        .all("ServerlessCacheSnapshotName")
        .into_iter()
        .map(str::to_string)
        .collect();
    names.sort();
    names
}

/// Assert `reply` is the modelled refusal `code` with HTTP `status`.
fn assert_refused(case: &str, reply: &Reply, status: u16, code: &str) {
    assert_eq!(
        reply.status, status,
        "case {case}: expected HTTP {status}, got {} {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_code(),
        code,
        "case {case}: expected error code {code:?}, got {:?} ({})",
        reply.error_code(),
        reply.body
    );
}

/// Assert everything a copy must report, on whichever response carries it — the copy's own
/// or a later describe's. `source` is the source snapshot as the describe of it reports it,
/// and `status` the one the copy is expected to be in at that point.
fn assert_copy(case: &CopyCase, phase: &str, reply: &Reply, source: &Reply, status: &str) {
    assert_eq!(
        reply.text("ServerlessCacheSnapshotName"),
        Some(case.target_stored_name),
        "case {} ({phase}): expected the stored lowercase target name {:?}, got {:?} ({})",
        case.case,
        case.target_stored_name,
        reply.text("ServerlessCacheSnapshotName"),
        reply.body
    );
    assert_eq!(
        reply.text("Status"),
        Some(status),
        "case {} ({phase}): expected Status {status:?}, got {:?} ({})",
        case.case,
        reply.text("Status"),
        reply.body
    );
    // Whatever the source's type was, a copy a caller asked for by name is that caller's own
    // manual snapshot.
    assert_eq!(
        reply.text("SnapshotType"),
        Some(state::SERVERLESS_SNAPSHOT_TYPE_MANUAL),
        "case {} ({phase}): expected SnapshotType {:?}, got {:?} ({})",
        case.case,
        state::SERVERLESS_SNAPSHOT_TYPE_MANUAL,
        reply.text("SnapshotType"),
        reply.body
    );
    // The ARN names the COPY, in this Region, under the resource type an ElastiCache
    // serverless cache snapshot carries.
    let arn = reply.text("ARN").unwrap_or_else(|| {
        panic!(
            "case {} ({phase}): the copy must report an ARN, got {}",
            case.case, reply.body
        )
    });
    for fragment in [
        "arn:",
        &format!(":{REGION}:"),
        &format!(":serverlesscachesnapshot:{}", case.target_stored_name),
    ] {
        assert!(
            arn.contains(fragment),
            "case {} ({phase}): the ARN {arn:?} must contain {fragment:?}",
            case.case
        );
    }
    assert_eq!(
        reply.text("KmsKeyId"),
        case.kms_key_id,
        "case {} ({phase}): expected KmsKeyId {:?}, got {:?} ({})",
        case.case,
        case.kms_key_id,
        reply.text("KmsKeyId"),
        reply.body
    );
    // A manual snapshot does not expire, so that member is not reported rather than invented.
    assert_eq!(
        reply.text("ExpiryTime"),
        None,
        "case {} ({phase}): ExpiryTime must not be reported, got {:?}",
        case.case,
        reply.text("ExpiryTime")
    );
    // Everything the snapshot CAPTURED is the source's, element for element: copying does
    // not retake the snapshot, so even the instant the data set was obtained at is carried
    // over rather than re-stamped.
    for member in SAME_AS_SOURCE {
        assert_eq!(
            reply.text(member),
            source.text(member),
            "case {} ({phase}): expected {member} {:?} (the source's), got {:?} ({})",
            case.case,
            source.text(member),
            reply.text(member),
            reply.body
        );
    }
}

/// The tags `arn` carries, as `(key, value)` pairs in the order the wire carried them.
async fn tags_of(emulator: &Emulator, case: &str, arn: &str) -> Vec<(String, String)> {
    let listed = list_tags(emulator, arn).await;
    assert_eq!(
        listed.status, 200,
        "case {case}: ListTagsForResource must answer 200, got {} {}",
        listed.status, listed.body
    );
    listed
        .all("Tag/Key")
        .into_iter()
        .map(str::to_string)
        .zip(listed.all("Tag/Value").into_iter().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn a_copy_is_the_source_serverless_snapshot_under_a_new_name() {
    for case in CASES {
        let emulator = Emulator::new();
        create_cache(
            &emulator,
            case.case,
            case.cache_request_name,
            case.engine,
            case.cache_extra,
        )
        .await;
        // A snapshot is taken of a settled cache, and copied from a settled snapshot. The
        // clock owes each of those changes from the call that started it, so the suite
        // carries the owed changes through rather than waiting out their spans.
        settle_pending_changes(&emulator);
        create_snapshot(
            &emulator,
            case.case,
            case.source_request_name,
            case.cache_request_name,
            case.source_extra,
        )
        .await;
        settle_pending_changes(&emulator);

        let source = describe_snapshot(&emulator, case.case, case.source_request_name).await;
        assert_eq!(
            source.text("Status"),
            Some(state::STATUS_AVAILABLE),
            "case {}: the source must be available before it is copied, got {}",
            case.case,
            source.body
        );

        let copied = copy_snapshot(
            &emulator,
            case.source_request_name,
            case.target_request_name,
            case.copy_extra,
        )
        .await;
        assert_eq!(
            copied.status, 200,
            "case {}: CopyServerlessCacheSnapshot must answer 200, got {} {}",
            case.case, copied.status, copied.body
        );
        // A copy is NOT ready: it answers `creating`, and the clock owes it `available`.
        assert_copy(case, "the copy", &copied, &source, state::STATUS_CREATING);

        // The copy is read back through the describe, so what is asserted is the PERSISTED
        // record and not the answer the copy composed.
        let described = poll_until(
            &emulator,
            REGION,
            case.case,
            &format!(
                "the serverless cache snapshot {} settling at available",
                case.target_stored_name
            ),
            "DescribeServerlessCacheSnapshots",
            &params(&[("ServerlessCacheSnapshotName", case.target_request_name)]),
            |reply| reply.text("Status") == Some(state::STATUS_AVAILABLE),
        )
        .await;
        assert_copy(
            case,
            "the describe",
            &described,
            &source,
            state::STATUS_AVAILABLE,
        );

        // The tags are the COPY's own: the ones its request carried, and none of them on the
        // source, which the copy request said nothing about.
        let copy_arn = described
            .text("ARN")
            .expect("the copy reports its ARN")
            .to_string();
        let expected: Vec<(String, String)> = case
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            tags_of(&emulator, case.case, &copy_arn).await,
            expected,
            "case {}: expected the copy to carry the tags its request named",
            case.case
        );
        let source_arn = source
            .text("ARN")
            .expect("the source reports its ARN")
            .to_string();
        assert_eq!(
            tags_of(&emulator, case.case, &source_arn).await,
            Vec::new(),
            "case {}: a copy's tags are not added to the source",
            case.case
        );

        // Both snapshots are stored: a copy adds a record beside its source rather than
        // replacing it, and the source itself did not move — copying READS it.
        let mut expected_names = vec![
            case.source_request_name.to_ascii_lowercase(),
            case.target_stored_name.to_string(),
        ];
        expected_names.sort();
        assert_eq!(
            stored_names(&emulator).await,
            expected_names,
            "case {}: expected both {expected_names:?} to be stored",
            case.case
        );
        let after = describe_snapshot(&emulator, case.case, case.source_request_name).await;
        for member in ["Status", "ARN", "CreateTime", "BytesUsedForCache"] {
            assert_eq!(
                after.text(member),
                source.text(member),
                "case {}: copying must leave the source's {member} alone, got {:?} where it was \
                 {:?}",
                case.case,
                after.text(member),
                source.text(member)
            );
        }
    }
}

#[tokio::test]
async fn a_copy_of_a_copy_still_reports_the_original_point_in_time() {
    let emulator = Emulator::new();
    create_cache(&emulator, "a chain of copies", "chained-cache", "redis", &[]).await;
    settle_pending_changes(&emulator);
    create_snapshot(
        &emulator,
        "a chain of copies",
        "chain-origin",
        "chained-cache",
        &[],
    )
    .await;
    settle_pending_changes(&emulator);
    let origin = describe_snapshot(&emulator, "a chain of copies", "chain-origin").await;

    // Each link copies the previous one, so the second copy's content came through a copy
    // rather than from the snapshot the cache was taken of.
    for (case, source, target) in [
        ("first link", "chain-origin", "chain-copy-one"),
        ("second link", "chain-copy-one", "chain-copy-two"),
    ] {
        let copied = copy_snapshot(&emulator, source, target, &[]).await;
        assert_eq!(
            copied.status, 200,
            "case {case}: CopyServerlessCacheSnapshot must answer 200, got {} {}",
            copied.status, copied.body
        );
        settle_pending_changes(&emulator);
        let described = describe_snapshot(&emulator, case, target).await;
        for member in SAME_AS_SOURCE {
            assert_eq!(
                described.text(member),
                origin.text(member),
                "case {case}: expected {member} {:?} (the origin's), got {:?} ({})",
                origin.text(member),
                described.text(member),
                described.body
            );
        }
    }

    assert_eq!(
        stored_names(&emulator).await,
        vec!["chain-copy-one", "chain-copy-two", "chain-origin"],
        "every link of the chain must be stored"
    );
}

#[tokio::test]
async fn the_target_name_form_is_enforced() {
    let emulator = Emulator::new();
    create_cache(&emulator, "target names", "name-form-cache", "redis", &[]).await;
    settle_pending_changes(&emulator);
    create_snapshot(
        &emulator,
        "target names",
        "name-form-source",
        "name-form-cache",
        &[],
    )
    .await;
    settle_pending_changes(&emulator);

    // The axis: the target name's form. The documented rule is the character set and the
    // 255-character bound a snapshot name of this service shares
    // (`state::is_valid_snapshot_name`), so the row AT the bound is accepted and the row one
    // past it is refused; a hyphen's placement, which no ElastiCache documentation
    // constrains for a snapshot name, is accepted.
    let at_bound = "c".repeat(255);
    let past_bound = "c".repeat(256);
    let rows: [(&str, &str, u16, &str); 9] = [
        ("a plain name", "plain-copy", 200, ""),
        ("mixed case is stored lowercase", "Plain-COPY-2", 200, ""),
        ("digits and hyphens", "20261111-copy", 200, ""),
        ("two consecutive hyphens", "ok--copy", 200, ""),
        ("a trailing hyphen", "ok-copy-", 200, ""),
        ("at the 255-character bound", at_bound.as_str(), 200, ""),
        (
            "a character that is neither alphanumeric nor a hyphen",
            "bad.copy",
            400,
            "InvalidParameterValue",
        ),
        (
            "one character past the 255-character bound",
            past_bound.as_str(),
            400,
            "InvalidParameterValue",
        ),
        ("an underscore", "bad_copy", 400, "InvalidParameterValue"),
    ];

    let mut accepted: Vec<String> = vec!["name-form-source".to_string()];
    for (case, target, status, code) in rows {
        let reply = copy_snapshot(&emulator, "name-form-source", target, &[]).await;
        assert_refused(case, &reply, status, code);
        if status == 200 {
            accepted.push(target.to_ascii_lowercase());
        }
        settle_pending_changes(&emulator);
    }

    accepted.sort();
    assert_eq!(
        stored_names(&emulator).await,
        accepted,
        "only the accepted target names may be stored"
    );
}

#[tokio::test]
async fn a_copy_the_rule_forbids_is_refused_and_stores_nothing() {
    let emulator = Emulator::new();
    create_cache(&emulator, "refusals", "refused-copy-cache", "redis", &[]).await;
    settle_pending_changes(&emulator);
    create_snapshot(
        &emulator,
        "refusals",
        "held-source",
        "refused-copy-cache",
        &[],
    )
    .await;
    settle_pending_changes(&emulator);
    let taken = copy_snapshot(&emulator, "held-source", "taken-target", &[]).await;
    assert_eq!(
        taken.status, 200,
        "the first copy must succeed, got {} {}",
        taken.status, taken.body
    );
    settle_pending_changes(&emulator);

    // Each row: the form pairs that make the request, and the modelled refusal. The axis is
    // the reason the copy cannot be made, and — for the not-found rows — the form of a name
    // the account holds no snapshot under: its length, its case, and a name that resolves to
    // a serverless CACHE this account does hold, since a resource of another kind is still no
    // snapshot.
    let rows: [RefusalCase; 8] = [
        (
            "a source the account holds no snapshot under",
            &[
                ("SourceServerlessCacheSnapshotName", "no-such-source"),
                ("TargetServerlessCacheSnapshotName", "fresh-copy"),
            ],
            404,
            "ServerlessCacheSnapshotNotFoundFault",
        ),
        (
            "the same absent source in upper case",
            &[
                ("SourceServerlessCacheSnapshotName", "NO-SUCH-SOURCE"),
                ("TargetServerlessCacheSnapshotName", "fresh-copy"),
            ],
            404,
            "ServerlessCacheSnapshotNotFoundFault",
        ),
        (
            "a single-character absent source",
            &[
                ("SourceServerlessCacheSnapshotName", "q"),
                ("TargetServerlessCacheSnapshotName", "fresh-copy"),
            ],
            404,
            "ServerlessCacheSnapshotNotFoundFault",
        ),
        (
            "a source naming a serverless cache the account holds",
            &[
                ("SourceServerlessCacheSnapshotName", "refused-copy-cache"),
                ("TargetServerlessCacheSnapshotName", "fresh-copy"),
            ],
            404,
            "ServerlessCacheSnapshotNotFoundFault",
        ),
        (
            "a target name already held",
            &[
                ("SourceServerlessCacheSnapshotName", "held-source"),
                ("TargetServerlessCacheSnapshotName", "taken-target"),
            ],
            400,
            "ServerlessCacheSnapshotAlreadyExistsFault",
        ),
        (
            "a target name already held, in another case",
            &[
                ("SourceServerlessCacheSnapshotName", "held-source"),
                ("TargetServerlessCacheSnapshotName", "TAKEN-TARGET"),
            ],
            400,
            "ServerlessCacheSnapshotAlreadyExistsFault",
        ),
        (
            "a snapshot copied onto its own name",
            &[
                ("SourceServerlessCacheSnapshotName", "held-source"),
                ("TargetServerlessCacheSnapshotName", "held-source"),
            ],
            400,
            "ServerlessCacheSnapshotAlreadyExistsFault",
        ),
        (
            "a tag with no key",
            &[
                ("SourceServerlessCacheSnapshotName", "held-source"),
                ("TargetServerlessCacheSnapshotName", "keyless-tag-copy"),
                ("Tags.Tag.1.Value", "no-key"),
            ],
            400,
            "InvalidParameterValue",
        ),
    ];

    for (case, pairs, status, code) in rows {
        let reply = emulator
            .call("CopyServerlessCacheSnapshot", &params(pairs))
            .await;
        assert_refused(case, &reply, status, code);
        if status == 404 {
            // The not-found sentence is fixed text naming the missing snapshot as the SOURCE
            // of the copy: nothing the request sent appears in it, and it is not the
            // sentence the operation that acts on one named snapshot answers
            // ([`SNAPSHOT_NOT_FOUND`]).
            assert_eq!(
                reply.error_message(),
                SOURCE_NOT_FOUND,
                "case {case}: expected {SOURCE_NOT_FOUND:?}, got {:?}",
                reply.error_message()
            );
        }
    }

    // The contrast, through behaviour rather than a restatement: the operation that acts on
    // ONE named snapshot words the same fault as the class alone, so the copy's sentence is
    // the copy's own and the two surfaces stay observable apart.
    let exported = emulator
        .call(
            "ExportServerlessCacheSnapshot",
            &params(&[
                ("ServerlessCacheSnapshotName", "no-such-source"),
                ("S3BucketName", "elasticache-exports-copy-contrast"),
            ]),
        )
        .await;
    assert_refused(
        "the same absent name at the export",
        &exported,
        404,
        "ServerlessCacheSnapshotNotFoundFault",
    );
    assert_eq!(
        exported.error_message(),
        SNAPSHOT_NOT_FOUND,
        "case the same absent name at the export: expected {SNAPSHOT_NOT_FOUND:?}, got {:?}",
        exported.error_message()
    );

    // More tags than a resource may carry, which the modelled `TagQuotaPerResourceExceeded`
    // refuses: "the maximum number of tags permitted on a resource is 50". Built rather than
    // tabled, because a row of 51 tags is 102 form pairs.
    let mut over_tagged = params(&[
        ("SourceServerlessCacheSnapshotName", "held-source"),
        ("TargetServerlessCacheSnapshotName", "over-tagged-copy"),
    ]);
    for index in 1..=51 {
        over_tagged.push((format!("Tags.Tag.{index}.Key"), format!("key-{index}")));
        over_tagged.push((format!("Tags.Tag.{index}.Value"), format!("value-{index}")));
    }
    let over_tagged = emulator
        .call("CopyServerlessCacheSnapshot", &over_tagged)
        .await;
    assert_refused(
        "51 tags",
        &over_tagged,
        400,
        "TagQuotaPerResourceExceeded",
    );

    // A source that has not settled yet has no finished data set to copy. The clock owes a
    // just-taken snapshot `available` from the call that took it, so this row holds that
    // change out of reach rather than racing the span.
    create_snapshot(
        &emulator,
        "a source still creating",
        "creating-source",
        "refused-copy-cache",
        &[],
    )
    .await;
    hold_pending_changes(&emulator);
    let too_early = copy_snapshot(&emulator, "creating-source", "early-copy", &[]).await;
    assert_refused(
        "a source still creating",
        &too_early,
        400,
        "InvalidServerlessCacheSnapshotStateFault",
    );
    for fragment in ["creating-source", state::STATUS_CREATING] {
        assert!(
            too_early.error_message().contains(fragment),
            "case a source still creating: the state fault must mention {fragment:?}, got {:?}",
            too_early.error_message()
        );
    }
    // ... and once the clock owes the source `available`, the same copy is accepted, so the
    // refusal above is the rule's and not a blanket one.
    release_pending_changes(&emulator);
    let in_time = copy_snapshot(&emulator, "creating-source", "early-copy", &[]).await;
    assert_eq!(
        in_time.status, 200,
        "case a settled source: expected HTTP 200, got {} {}",
        in_time.status, in_time.body
    );

    // Not one refusal added a record: the store holds exactly the snapshots the accepted
    // calls made.
    assert_eq!(
        stored_names(&emulator).await,
        vec![
            "creating-source",
            "early-copy",
            "held-source",
            "taken-target"
        ],
        "a refused copy must store nothing"
    );
}

#[tokio::test]
async fn a_cache_restored_from_a_copy_holds_the_data_set_the_source_captured() {
    let emulator = Emulator::new();
    // The rows a client writes into the source cache before the snapshot is taken: the sizes
    // and key forms the data set generalises over.
    let written: [(&str, &str); 3] = [
        ("greeting", "copied-value"),
        ("empty", ""),
        ("tenant:42/session", "{\"user\":\"42\"}"),
    ];

    create_cache(&emulator, "a copied data set", "data-origin", "redis", &[]).await;
    let available = poll_until(
        &emulator,
        REGION,
        "a copied data set",
        "the serverless cache data-origin settling at available",
        "DescribeServerlessCaches",
        &params(&[("ServerlessCacheName", "data-origin")]),
        |reply| reply.text("Status") == Some(state::STATUS_AVAILABLE),
    )
    .await;
    let (address, port) = endpoint_of(
        &available,
        "Endpoint/Address",
        "Endpoint/Port",
        "a copied data set",
    );
    let mut origin = open_tls("a copied data set", &address, &port).await;
    for (key, value) in written {
        let reply = command("a copied data set", &mut origin, &["SET", key, value]).await;
        assert_eq!(
            reply,
            Resp::Status("OK".to_string()),
            "case a copied data set: SET {key} must be accepted, got {reply:?}"
        );
    }

    create_snapshot(
        &emulator,
        "a copied data set",
        "data-source",
        "data-origin",
        &[],
    )
    .await;
    settle_pending_changes(&emulator);
    let source = describe_snapshot(&emulator, "a copied data set", "data-source").await;
    let captured: u64 = source
        .text("BytesUsedForCache")
        .and_then(|size| size.parse().ok())
        .unwrap_or_else(|| {
            panic!(
                "case a copied data set: the source must report the size of the data set it \
                 captured, got {}",
                source.body
            )
        });
    assert!(
        captured > 0,
        "case a copied data set: a snapshot of a cache holding {} key(s) must report a size \
         above zero, got {captured}",
        written.len()
    );

    let copied = copy_snapshot(&emulator, "data-source", "data-copy", &[]).await;
    assert_eq!(
        copied.status, 200,
        "case a copied data set: CopyServerlessCacheSnapshot must answer 200, got {} {}",
        copied.status, copied.body
    );
    settle_pending_changes(&emulator);
    let copy = describe_snapshot(&emulator, "a copied data set", "data-copy").await;
    assert_eq!(
        copy.text("BytesUsedForCache"),
        source.text("BytesUsedForCache"),
        "case a copied data set: the copy must report the size of the data set it holds, the \
         source's, got {:?} where the source reported {:?}",
        copy.text("BytesUsedForCache"),
        source.text("BytesUsedForCache")
    );
    let copy_arn = copy
        .text("ARN")
        .expect("the copy reports its ARN")
        .to_string();

    // A cache restored from the COPY's ARN serves the keys the SOURCE snapshot captured:
    // that is what makes the copy a copy of the data set and not a note that one was taken.
    let restored = emulator
        .call(
            "CreateServerlessCache",
            &params(&[
                ("ServerlessCacheName", "restored-from-copy"),
                ("Engine", "redis"),
                ("SnapshotArnsToRestore.SnapshotArn.1", &copy_arn),
            ]),
        )
        .await;
    assert_eq!(
        restored.status, 200,
        "case a copied data set: a create restoring from the copy must answer 200, got {} {}",
        restored.status, restored.body
    );
    let ready = poll_until(
        &emulator,
        REGION,
        "a copied data set",
        "the restored cache settling at available",
        "DescribeServerlessCaches",
        &params(&[("ServerlessCacheName", "restored-from-copy")]),
        |reply| reply.text("Status") == Some(state::STATUS_AVAILABLE),
    )
    .await;
    let (address, port) = endpoint_of(
        &ready,
        "Endpoint/Address",
        "Endpoint/Port",
        "a copied data set",
    );
    let mut session = open_tls("a copied data set", &address, &port).await;
    for (key, value) in written {
        let reply = command("a copied data set", &mut session, &["GET", key]).await;
        assert_eq!(
            reply,
            Resp::Bulk(value.to_string()),
            "case a copied data set: GET {key} on the cache restored from the copy must answer \
             the value the source snapshot captured, got {reply:?}"
        );
    }
    // A key no snapshot captured is not served by the restored cache: the data set is the
    // copied one and not a blanket answer.
    let absent = command("a copied data set", &mut session, &["GET", "never-written"]).await;
    assert_eq!(
        absent,
        Resp::Nil,
        "case a copied data set: a key the snapshot never captured must be absent, got {absent:?}"
    );

    // Writing to the source cache after the copy was taken does not reach the copy: the copy
    // holds its own data set.
    let reply = command("a copied data set", &mut origin, &["SET", "after", "late"]).await;
    assert_eq!(
        reply,
        Resp::Status("OK".to_string()),
        "case a copied data set: a later SET on the source cache must be accepted, got {reply:?}"
    );
    let late = command("a copied data set", &mut session, &["GET", "after"]).await;
    assert_eq!(
        late,
        Resp::Nil,
        "case a copied data set: a key written after the snapshot was taken is not in the copy, \
         got {late:?}"
    );
}
