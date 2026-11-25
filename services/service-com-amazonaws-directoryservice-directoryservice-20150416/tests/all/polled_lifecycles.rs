// SPDX-License-Identifier: CC0-1.0

//! The create-then-poll lifecycles of this service, driven end to end over HTTP the way a
//! client with a waiter drives them: a write, then the REPORTING operation polled until the
//! state it reports has advanced.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body,
//! dispatched by the multi-service router (the harness is `crate::harness`). Nothing a
//! RESPONSE claims is read from the state struct, so an answer that is not computed from
//! persisted state cannot pass.
//!
//! The rule under test: every transient state this service records is owed to the clock,
//! so each of these sequences CONVERGES — the reporting operation answers each poll, the
//! state it reports advances out of the transient value on its own, and the end of the
//! lifecycle is observable — and it converges within a bound a client's waiter would allow
//! ([`POLL_DEADLINE`]), rather than resting on a transient value no clock ever moves. The
//! sequences are the ones a lifecycle client drives:
//!
//! * `CreateMicrosoftAD` -> `DescribeDirectories` until the directory is in service;
//! * a create -> `DescribeDirectories` polled past the stage the create was accepted in,
//!   over a table of creates spanning every directory type this service creates: the stage
//!   the poll first reads after that one is the stage the build runs in, and only a later
//!   poll reads the one a directory in service carries;
//! * `CreateSnapshot` -> `DescribeSnapshots` until the snapshot is complete -> `DeleteSnapshot`
//!   -> the snapshot is no longer in the inventory;
//! * `AddIpRoutes` -> `ListIpRoutes` until the block is in service -> `RemoveIpRoutes` -> the
//!   block is no longer routed, over a table of blocks of both address families;
//! * `CreateLogSubscription` -> the unfiltered `ListLogSubscriptions` inventory ->
//!   `DeleteLogSubscription`;
//! * `CreateTrust` -> `DescribeTrusts` until the trust is created -> `VerifyTrust` -> until it
//!   is verified -> `DeleteTrust` -> until the trust is no longer there;
//! * the WHOLE life of a Managed Microsoft AD placed in a VPC as one walk — `CreateMicrosoftAD`
//!   -> `DescribeDirectories` until the directory is in service -> the placement reconciled
//!   against the services that OWN its network -> `DeleteDirectory` -> until the account holds
//!   nothing for the id -> the two refusal classes a gone id and a never-issued one are told
//!   apart by.
//!
//! Each poll also pins that the transient state is REACHABLE: the write answers while the
//! state it starts is still the transient one, which is what a client polling immediately
//! after the call observes.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_directoryservice_directoryservice_20150416::state::{
    STAGE_DELETED, STAGE_DELETING,
};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::ad_connector::minimal_connect;
use crate::directory_deletion::{
    delete, does_not_exist, invalid_directory_ids, refused_with, stages_until_gone,
};
use crate::directory_dns_addresses::{assert_address_in, ec2_client, network, AccountSubnet};
use crate::directory_lifecycle::{
    describe_one, descriptions, minimal_create, PASSWORD, REGION, STAGE_ACTIVE, STAGE_CREATING,
    STAGE_REQUESTED,
};
use crate::directory_networking_and_settings::routes_of;
use crate::directory_security_group::{block_of_vpc, group_reported_by_ec2};
use crate::directory_server_interfaces::{
    addresses_of, directory_interfaces, dns_addresses, subnets_of,
};
use crate::harness::{call, ok, poll_until_reported, service, string};
use crate::microsoft_ad::{
    create_microsoft_ad, minimal_microsoft_ad, DEFAULT_EDITION, FRESH_DOMAIN_CONTROLLERS,
    TYPE_MICROSOFT_AD,
};

/// How long one polled state may take to advance before the case fails: far longer than
/// any span this service publishes, and well inside what a client's waiter allows.
const POLL_DEADLINE: Duration = Duration::from_secs(20);

/// How often a poll asks: a client polls at human intervals, and a test that drives the
/// same sequence asks often enough to see each state it passes through.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Poll `$probe` — an expression answering `(Option<T>, String)`, the converged value and
/// what was observed this time round — until it converges, or fail naming the case, what was
/// being waited for and the last observation.
macro_rules! poll_until {
    ($case:expr, $what:expr, $probe:expr) => {{
        let started = Instant::now();
        loop {
            let (converged, observed): (Option<_>, String) = $probe;
            if let Some(value) = converged {
                break value;
            }
            assert!(
                started.elapsed() < POLL_DEADLINE,
                "case {case:?}: expected {what} within {deadline:?}, last observed {observed}",
                case = $case,
                what = $what,
                deadline = POLL_DEADLINE,
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }};
}

/// The `Stage` `DescribeDirectories` reports for one directory.
async fn directory_stage(router: &MultiServiceRouter, directory_id: &str, case: &str) -> String {
    let answer = ok(
        router,
        "DescribeDirectories",
        json!({ "DirectoryIds": [directory_id] }),
        case,
    )
    .await;
    let described = answer["DirectoryDescriptions"]
        .as_array()
        .and_then(|page| page.first().cloned())
        .unwrap_or_else(|| panic!("case {case:?}: expected a description, got {answer}"));
    string(&described, "Stage", case)
}

/// A Managed Microsoft AD directory, polled until it is in service — the fixture every
/// lifecycle below starts from, and the first wait a lifecycle client makes.
async fn directory_in_service(router: &MultiServiceRouter, name: &str, case: &str) -> String {
    let answer = ok(router, "CreateMicrosoftAD", minimal_microsoft_ad(name), case).await;
    let directory_id = string(&answer, "DirectoryId", case);

    // The create answered while the directory was still being built, which is what a client
    // that polls immediately observes: the stage is one of the chain's transient values, not
    // the serving one.
    let first = directory_stage(router, &directory_id, case).await;
    assert!(
        first == "Requested" || first == "Creating",
        "case {case:?}: expected a create to answer before the directory is in service, got \
         stage {first:?}"
    );

    poll_until!(case, "the directory to report itself in service", {
        let stage = directory_stage(router, &directory_id, case).await;
        (
            (stage == "Active").then_some(stage.clone()),
            format!("stage {stage:?}"),
        )
    });
    directory_id
}

/// The snapshots the unfiltered `DescribeSnapshots` inventory reports, by `SnapshotId`.
async fn snapshot_inventory(router: &MultiServiceRouter, case: &str) -> BTreeMap<String, Value> {
    let answer = ok(router, "DescribeSnapshots", json!({}), case).await;
    answer["Snapshots"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected Snapshots, got {answer}"))
        .iter()
        .map(|snapshot| (string(snapshot, "SnapshotId", case), snapshot.clone()))
        .collect()
}

#[tokio::test]
async fn the_snapshot_lifecycle_converges_through_describe_snapshots() {
    let router = service();
    let case = "the snapshot lifecycle";
    let directory_id = directory_in_service(&router, "snap.example.com", case).await;

    let created = ok(
        &router,
        "CreateSnapshot",
        json!({ "DirectoryId": &directory_id, "Name": "nightly" }),
        case,
    )
    .await;
    let snapshot_id = string(&created, "SnapshotId", case);

    // The create answered while the snapshot was still being taken.
    let inventory = snapshot_inventory(&router, case).await;
    assert_eq!(
        inventory
            .get(&snapshot_id)
            .map(|snapshot| snapshot["Status"].clone()),
        Some(json!("Creating")),
        "case {case:?}: expected the fresh snapshot to be reported as still being taken, \
         got {inventory:?}"
    );

    // The inventory is what a client polls, and the status advances on the clock alone.
    poll_until!(case, "the snapshot to report itself complete", {
        let inventory = snapshot_inventory(&router, case).await;
        let status = inventory
            .get(&snapshot_id)
            .map(|snapshot| snapshot["Status"].clone())
            .unwrap_or(Value::Null);
        (
            (status == json!("Completed")).then_some(status.clone()),
            format!("status {status}"),
        )
    });

    ok(
        &router,
        "DeleteSnapshot",
        json!({ "SnapshotId": &snapshot_id }),
        case,
    )
    .await;
    let after = poll_until!(case, "the deleted snapshot to leave the inventory", {
        let inventory = snapshot_inventory(&router, case).await;
        let held = inventory.contains_key(&snapshot_id);
        (
            (!held).then_some(inventory.len()),
            format!("the inventory still holds {snapshot_id}: {held}"),
        )
    });
    assert_eq!(
        after, 0,
        "case {case:?}: expected the account to hold no snapshot after the delete, got \
         {after} of them"
    );
}

#[tokio::test]
async fn the_ip_route_lifecycle_converges_through_list_ip_routes() {
    let router = service();
    let case = "the IP route lifecycle";
    let directory_id = directory_in_service(&router, "routes.example.com", case).await;

    // The rows differ along the address family and the prefix length, so the convergence is
    // not asserted on one shape of block. The member a block is REMOVED by is the one of its
    // own family, as `RemoveIpRoutes` declares them (`CidrIps` / `CidrIpv6s`).
    let rows: [(&str, &str, &str, &str); 3] = [
        ("an IPv4 network", "CidrIp", "CidrIps", "198.51.100.0/24"),
        ("an IPv4 host route", "CidrIp", "CidrIps", "203.0.113.7/32"),
        ("an IPv6 network", "CidrIpv6", "CidrIpv6s", "2001:db8::/32"),
    ];

    for (class, member, removal_member, block) in rows {
        let case = format!("{case}: {class}");
        ok(
            &router,
            "AddIpRoutes",
            json!({ "DirectoryId": &directory_id, "IpRoutes": [{ member: block }] }),
            &case,
        )
        .await;

        // The add answered while the block was still being put in service.
        let routes = routes_of(&router, &directory_id, &case).await;
        assert_eq!(
            routes
                .get(block)
                .map(|route| route["IpRouteStatusMsg"].clone()),
            Some(json!("Adding")),
            "case {case:?}: expected the fresh block to be reported as still being added, \
             got {routes:?}"
        );

        // The listing is what a client polls, and the status advances on the clock alone.
        poll_until!(&case, "the block to report itself in service", {
            let routes = routes_of(&router, &directory_id, &case).await;
            let status = routes
                .get(block)
                .map(|route| route["IpRouteStatusMsg"].clone())
                .unwrap_or(Value::Null);
            (
                (status == json!("Added")).then_some(status.clone()),
                format!("status {status}"),
            )
        });

        ok(
            &router,
            "RemoveIpRoutes",
            json!({ "DirectoryId": &directory_id, removal_member: [block] }),
            &case,
        )
        .await;
        poll_until!(&case, "the removed block to stop being routed", {
            let routes = routes_of(&router, &directory_id, &case).await;
            let held = routes.contains_key(block);
            ((!held).then_some(()), format!("still routed: {held}"))
        });
    }

    // Every block has been through the whole lifecycle, so the directory routes nothing.
    let left = routes_of(&router, &directory_id, case).await;
    assert!(
        left.is_empty(),
        "case {case:?}: expected the directory to route nothing after the removals, got \
         {left:?}"
    );
}

#[tokio::test]
async fn the_log_subscription_lifecycle_is_reported_by_the_unfiltered_inventory() {
    let router = service();
    let case = "the log subscription lifecycle";
    let first = directory_in_service(&router, "logs-one.example.com", case).await;
    let second = directory_in_service(&router, "logs-two.example.com", case).await;

    for (directory_id, group) in [(&first, "/aws/ds/one"), (&second, "/aws/ds/two")] {
        ok(
            &router,
            "CreateLogSubscription",
            json!({ "DirectoryId": directory_id, "LogGroupName": group }),
            case,
        )
        .await;
    }

    // The unfiltered inventory is what the lifecycle client reads: both subscriptions are
    // reported, each against the directory that created it.
    let answer = ok(&router, "ListLogSubscriptions", json!({}), case).await;
    let reported: BTreeMap<String, String> = answer["LogSubscriptions"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected LogSubscriptions, got {answer}"))
        .iter()
        .map(|subscription| {
            (
                string(subscription, "DirectoryId", case),
                string(subscription, "LogGroupName", case),
            )
        })
        .collect();
    assert_eq!(
        reported,
        [
            (first.clone(), "/aws/ds/one".to_string()),
            (second.clone(), "/aws/ds/two".to_string()),
        ]
        .into_iter()
        .collect::<BTreeMap<String, String>>(),
        "case {case:?}: expected the inventory to report both subscriptions, got {reported:?}"
    );

    ok(
        &router,
        "DeleteLogSubscription",
        json!({ "DirectoryId": &first }),
        case,
    )
    .await;
    let left = ok(&router, "ListLogSubscriptions", json!({}), case).await;
    let pairs: Vec<Value> = left["LogSubscriptions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        pairs.len(),
        1,
        "case {case:?}: expected the deleted subscription to leave the inventory, got {left}"
    );
    assert_eq!(
        pairs[0]["DirectoryId"],
        json!(&second),
        "case {case:?}: expected the other directory's subscription to remain, got {left}"
    );
}

#[tokio::test]
async fn the_trust_lifecycle_converges_through_describe_trusts() {
    let router = service();
    let case = "the trust lifecycle";
    let directory_id = directory_in_service(&router, "trusted.example.com", case).await;

    let created = ok(
        &router,
        "CreateTrust",
        json!({
            "DirectoryId": &directory_id,
            "RemoteDomainName": "remote.example.com",
            "TrustPassword": "Tru5t-P@ssword 1",
            "TrustDirection": "Two-Way",
        }),
        case,
    )
    .await;
    let trust_id = string(&created, "TrustId", case);

    /// The `TrustState` `DescribeTrusts` reports for one trust, or `Null` once it reports
    /// none.
    async fn trust_state(router: &MultiServiceRouter, trust_id: &str, case: &str) -> Value {
        let answer = ok(
            router,
            "DescribeTrusts",
            json!({ "TrustIds": [trust_id] }),
            case,
        )
        .await;
        answer["Trusts"]
            .as_array()
            .and_then(|trusts| trusts.first())
            .map(|trust| trust["TrustState"].clone())
            .unwrap_or(Value::Null)
    }

    // The create answered while the Amazon Web Services side was still being established.
    assert_eq!(
        trust_state(&router, &trust_id, case).await,
        json!("Creating"),
        "case {case:?}: expected the create to answer before the trust is established"
    );
    poll_until!(case, "the trust to report itself created", {
        let state = trust_state(&router, &trust_id, case).await;
        (
            (state == json!("Created")).then_some(state.clone()),
            format!("state {state}"),
        )
    });

    ok(
        &router,
        "VerifyTrust",
        json!({ "TrustId": &trust_id }),
        case,
    )
    .await;
    assert_eq!(
        trust_state(&router, &trust_id, case).await,
        json!("Verifying"),
        "case {case:?}: expected the verify to answer while the verification is running"
    );
    poll_until!(case, "the trust to report itself verified", {
        let state = trust_state(&router, &trust_id, case).await;
        (
            (state == json!("Verified")).then_some(state.clone()),
            format!("state {state}"),
        )
    });

    ok(
        &router,
        "DeleteTrust",
        json!({ "TrustId": &trust_id }),
        case,
    )
    .await;
    // The teardown is owed to the clock too: the trust is reported through it and then the
    // relationship is gone, which is what the lifecycle client's last poll reads.
    poll_until!(case, "the deleted trust to leave the service", {
        let (status, parsed) = call(
            &router,
            "DescribeTrusts",
            json!({ "TrustIds": [&trust_id] }),
        )
        .await;
        (
            (status == 400).then_some(()),
            format!("DescribeTrusts answered {status} with {parsed}"),
        )
    });
}

/// The member of a description `path` names: a top-level member, or a member of one of its
/// structures where the path is dotted (`VpcSettings.VpcId`).
fn member<'a>(description: &'a Value, path: &str) -> &'a Value {
    let mut value = description;
    for segment in path.split('.') {
        value = &value[segment];
    }
    value
}

/// One row of the build table: the create that starts a directory build, and what the
/// description of that directory must report WHILE the build is still running.
struct BuildCase {
    case: &'static str,
    operation: &'static str,
    body: Value,
    /// The members of the description, each named by the path [`member`] reads, and the
    /// value it must carry from the build stage onwards.
    expected: Vec<(&'static str, Value)>,
}

#[tokio::test]
async fn a_polled_create_leaves_the_accepted_stage_for_the_build_stage_before_service() {
    let router = service();

    // The rows span every directory type this service creates, both Simple AD sizes and
    // both Managed Microsoft AD editions, so the chain is a rule about a directory build
    // rather than about one create.
    let rows: Vec<BuildCase> = vec![
        BuildCase {
            case: "Simple AD, Small",
            operation: "CreateDirectory",
            body: minimal_create("polled-small.example.com"),
            expected: vec![
                ("Name", json!("polled-small.example.com")),
                ("Type", json!("SimpleAD")),
                ("Size", json!("Small")),
            ],
        },
        BuildCase {
            case: "Simple AD, Large",
            operation: "CreateDirectory",
            body: json!({
                "Name": "polled-large.example.com",
                "Password": PASSWORD,
                "Size": "Large"
            }),
            expected: vec![
                ("Name", json!("polled-large.example.com")),
                ("Type", json!("SimpleAD")),
                ("Size", json!("Large")),
            ],
        },
        BuildCase {
            case: "AD Connector",
            operation: "ConnectDirectory",
            body: minimal_connect("polled-connected.example.com"),
            expected: vec![
                ("Name", json!("polled-connected.example.com")),
                ("Type", json!("ADConnector")),
            ],
        },
        BuildCase {
            case: "Managed Microsoft AD, default edition",
            operation: "CreateMicrosoftAD",
            body: minimal_microsoft_ad("polled-managed.example.com"),
            expected: vec![
                ("Name", json!("polled-managed.example.com")),
                ("Type", json!("MicrosoftAD")),
                ("Edition", json!("Enterprise")),
                // No short name in the request: the first label of the DNS name.
                ("ShortName", json!("POLLED-MANAGED")),
                ("DesiredNumberOfDomainControllers", json!(2)),
                ("VpcSettings.VpcId", json!("vpc-0a1b2c3d")),
                (
                    "VpcSettings.SubnetIds",
                    json!(["subnet-0a1b2c3d", "subnet-1a2b3c4d"]),
                ),
            ],
        },
        BuildCase {
            case: "Managed Microsoft AD, Standard, with every optional member given",
            operation: "CreateMicrosoftAD",
            body: json!({
                "Name": "polled-standard.example.com",
                "Password": PASSWORD,
                "Edition": "Standard",
                "ShortName": "POLLEDSTD",
                "Description": "polled through the build",
                "VpcSettings": {
                    "VpcId": "vpc-11223344",
                    "SubnetIds": ["subnet-11223344", "subnet-55667788"]
                }
            }),
            expected: vec![
                ("Name", json!("polled-standard.example.com")),
                ("Type", json!("MicrosoftAD")),
                ("Edition", json!("Standard")),
                ("ShortName", json!("POLLEDSTD")),
                ("Description", json!("polled through the build")),
                ("VpcSettings.VpcId", json!("vpc-11223344")),
                (
                    "VpcSettings.SubnetIds",
                    json!(["subnet-11223344", "subnet-55667788"]),
                ),
            ],
        },
    ];

    // Each row is polled past the stage its own create was accepted in BEFORE the next row
    // is created, so what each poll reads is the position of that row's own build.
    let mut created: Vec<(&str, String)> = Vec::new();
    for BuildCase {
        case,
        operation,
        body,
        expected,
    } in rows
    {
        let answer = ok(&router, operation, body, case).await;
        let directory_id = string(&answer, "DirectoryId", case);

        // A client's waiter polls past the stage the create was accepted in. The stage it
        // first reads after that one is the stage the build runs in — a directory whose
        // servers are not up yet is not one in service, so the chain cannot skip it.
        let building = poll_until_reported(
            case,
            "the create to leave the stage it was accepted in",
            POLL_DEADLINE,
            || describe_one(&router, &directory_id, case),
            |described: &Value| described["Stage"] != json!(STAGE_REQUESTED),
        )
        .await;
        assert_eq!(
            building["Stage"],
            json!(STAGE_CREATING),
            "case {case:?}: expected the first stage after the accepted one to be \
             {STAGE_CREATING}, got {building}"
        );

        // The whole record is reported from that stage onwards, not only once the build
        // ends: a client that reads the directory while it is being built reads what the
        // create persisted.
        for (path, expected_value) in &expected {
            let actual = member(&building, path);
            assert_eq!(
                actual, expected_value,
                "case {case:?}: expected {path} to be {expected_value} while the directory \
                 is {STAGE_CREATING}, got {actual}"
            );
        }

        created.push((case, directory_id));
    }

    // And every row settles on the stage a directory in service carries, on the clock
    // alone — the polls above only read.
    for (case, directory_id) in &created {
        let settled = poll_until_reported(
            case,
            "the build the create started to end",
            POLL_DEADLINE,
            || describe_one(&router, directory_id, case),
            |described: &Value| described["Stage"] != json!(STAGE_CREATING),
        )
        .await;
        assert_eq!(
            settled["Stage"],
            json!(STAGE_ACTIVE),
            "case {case:?}: expected the stage the chain ends on, got {settled}"
        );
        let again = describe_one(&router, directory_id, case).await;
        assert_eq!(
            again["Stage"], settled["Stage"],
            "case {case:?}: expected a second read to report the same stage, got {again}"
        );
        assert_eq!(
            again["StageLastUpdatedDateTime"], settled["StageLastUpdatedDateTime"],
            "case {case:?}: expected a second read to report the same stage timestamp, got \
             {again}"
        );
    }
}

/// The `Created` value of the modelled `DirectoryStage` enum: the stage between a finished
/// build and service. Named here rather than taken from the service, which rests nothing on
/// it — what the walk below asserts is the ORDER of the chain, not that any one stage of it
/// is observed.
const STAGE_CREATED: &str = "Created";

/// The modelled `DirectoryStage` values a build is reported in, in the order the modelled
/// enum declares them (`smithy_query(query="shape", shape="DirectoryStage")` answers
/// `["Requested", "Creating", "Created", "Active", ...]`): the three stages a directory still
/// being built may be reported in, then the stage one in service carries.
const CREATE_CHAIN: [&str; 4] = [STAGE_REQUESTED, STAGE_CREATING, STAGE_CREATED, STAGE_ACTIVE];

/// The modelled `DirectoryStage` values a teardown is reported in, in that same declared
/// order: the stage the delete leaves the directory in, then the terminal stage the chain
/// settles on before the record leaves the account's records.
const TEARDOWN_CHAIN: [&str; 2] = [STAGE_DELETING, STAGE_DELETED];

/// One row of the lifecycle table: the Managed Microsoft AD a row creates, and where.
struct ChainRow {
    case: &'static str,
    /// The fully qualified name the create asks for. Its first label is also the short name
    /// a request that names none is given.
    name: &'static str,
    edition: Option<&'static str>,
    short_name: Option<&'static str>,
    description: Option<&'static str>,
    network_type: Option<&'static str>,
    /// The subnets the create places the directory's servers in, in the order it names them.
    subnets: Vec<AccountSubnet>,
    /// A well-formed `DirectoryId` (`^d-[0-9a-f]{10}$`) this account is never issued, so a
    /// reporting call filtered by it reaches an id the account has never HELD rather than one
    /// whose directory is gone. One per row, so no case can pass by matching a single value.
    never_issued: &'static str,
}

impl ChainRow {
    /// The `CreateMicrosoftAD` body this row sends: the three required members, plus whichever
    /// optional ones the row gives.
    fn body(&self, vpc_id: &str) -> Value {
        let mut body = json!({
            "Name": self.name,
            "Password": PASSWORD,
            "VpcSettings": {
                "VpcId": vpc_id,
                "SubnetIds": self.subnet_ids(),
            }
        });
        for (member, given) in [
            ("Edition", self.edition),
            ("ShortName", self.short_name),
            ("Description", self.description),
            ("NetworkType", self.network_type),
        ] {
            if let Some(value) = given {
                body[member] = json!(value);
            }
        }
        body
    }

    /// The identifiers of the chosen subnets, in the order the create names them.
    fn subnet_ids(&self) -> Vec<String> {
        self.subnets.iter().map(|subnet| subnet.id.clone()).collect()
    }

    /// The zones the chosen subnets sit in, as the OWNER of subnets reports them: the zones
    /// the directory's placement has to report, and two of them, since the two subnets of a
    /// Managed Microsoft AD are in different Availability Zones.
    fn zones(&self) -> BTreeSet<String> {
        self.subnets
            .iter()
            .map(|subnet| subnet.zone.clone())
            .collect()
    }

    /// The edition the directory is created with: the one the row named, or the default.
    fn edition_reported(&self) -> &'static str {
        self.edition.unwrap_or(DEFAULT_EDITION)
    }

    /// The `DirectorySize` the directory is run at. `CreateMicrosoftAD` takes no `Size`, so
    /// the size is not echoed from the request: it follows from the edition — `Standard` is
    /// the edition for small and midsize organizations, `Enterprise` the one for a large
    /// directory — which is how the two editions map onto the two values the modelled
    /// `DirectorySize` enum declares.
    fn size_reported(&self) -> &'static str {
        if self.edition_reported() == "Standard" {
            "Small"
        } else {
            "Large"
        }
    }

    /// The `ShortName` the directory reports: the one the row named, or — from
    /// `CreateMicrosoftADRequest$ShortName`'s own documentation ("If you don't specify a
    /// NetBIOS name, it will default to the first part of your directory DNS. For example,
    /// `CORP` for the directory DNS `corp.example.com`") — the first label of the DNS name,
    /// written as a NetBIOS name is: upper case.
    fn short_name_reported(&self) -> String {
        self.short_name.map_or_else(
            || {
                self.name
                    .split('.')
                    .next()
                    .unwrap_or_default()
                    .to_uppercase()
            },
            str::to_string,
        )
    }

    /// The `NetworkType` the directory reports: the one the row named, or — from
    /// `DirectoryDescription$NetworkType`'s own default, "IPv4 or IPv6 based on the provided
    /// subnet capabilities" — the families the subnets it was placed in carry, which for the
    /// IPv4-only subnets of this account's network is `IPv4`.
    fn network_type_reported(&self) -> &'static str {
        self.network_type.unwrap_or("IPv4")
    }
}

/// Assert the stages `observed` — the distinct values the reporting read gave for one
/// directory, in the order it first saw each — are a walk THROUGH `chain`: every one of them
/// a stage that chain declares, and each later in it than the one before.
fn assert_walk_of(observed: &[String], chain: &[&str], case: &str) {
    let mut previous: Option<usize> = None;
    for stage in observed {
        let at = chain
            .iter()
            .position(|declared| declared == stage)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected every stage reported to be one of {chain:?}, got \
                     {observed:?}"
                )
            });
        assert!(
            previous.is_none_or(|before| before < at),
            "case {case:?}: expected the stages to walk {chain:?} in that order, got {observed:?}"
        );
        previous = Some(at);
    }
}

/// A timestamp member of a description, in the seconds the wire carries it in.
fn instant(described: &Value, member: &str, case: &str) -> f64 {
    described[member].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: expected a {member} timestamp, got {described}")
    })
}

/// Assert the settled description of `directory_id` reports what the row's request fixed and
/// what the service derives from it — and no copy of the password it was created with.
fn assert_settled_description(described: &Value, row: &ChainRow, directory_id: &str) {
    let case = row.case;
    for (member, expected) in [
        ("DirectoryId", json!(directory_id)),
        ("Name", json!(row.name)),
        ("ShortName", json!(row.short_name_reported())),
        ("Type", json!(TYPE_MICROSOFT_AD)),
        ("Edition", json!(row.edition_reported())),
        ("Size", json!(row.size_reported())),
        ("Stage", json!(STAGE_ACTIVE)),
        ("Description", json!(row.description)),
        ("NetworkType", json!(row.network_type_reported())),
        // Single sign-on is not something a create turns on, so a fresh directory reports the
        // member's modelled default.
        ("SsoEnabled", json!(false)),
        // No alias was created for the directory, and the member's own documentation says what
        // it reports then: "If no alias exists, the alias is the directory identifier".
        ("Alias", json!(directory_id)),
        (
            "DesiredNumberOfDomainControllers",
            json!(FRESH_DOMAIN_CONTROLLERS),
        ),
    ] {
        assert_eq!(
            described[member], expected,
            "case {case:?}: expected {member} to be {expected}, got {described}"
        );
    }

    // "The access URL for the directory, such as `http://<alias>.awsapps.com`. If no alias
    // exists, `<alias>` is the directory identifier": a host whose leading label is the id.
    let access_url = string(described, "AccessUrl", case);
    assert_eq!(
        access_url.split('.').next().unwrap_or_default(),
        directory_id,
        "case {case:?}: expected the access URL's leading label to be the directory id, got \
         {access_url:?}"
    );
    assert!(
        access_url
            .chars()
            .all(|character| character.is_ascii_alphanumeric()
                || character == '.'
                || character == '-'),
        "case {case:?}: expected the access URL to be a host name, got {access_url:?}"
    );

    // The operating system the directory's servers run, which the service knows of its own
    // servers rather than taking from the request.
    let os_version = string(described, "OsVersion", case);
    assert!(
        !os_version.is_empty(),
        "case {case:?}: expected the directory to report the version its servers run, got \
         {described}"
    );

    // A directory created in one Region has that Region as its primary one and is replicated
    // into no other, so the replication report names it alone.
    assert_eq!(
        described["RegionsInfo"]["PrimaryRegion"],
        json!(REGION),
        "case {case:?}: expected the Region the create was made in, got {described}"
    );
    let additional = described["RegionsInfo"]["AdditionalRegions"]
        .as_array()
        .map_or(0, Vec::len);
    assert_eq!(
        additional, 0,
        "case {case:?}: expected no Region beside the primary one, got {described}"
    );

    // A directory on an IPv4-only network serves DNS at no IPv6 address, so the member for
    // them is absent or empty rather than carrying one.
    let ipv6 = described["DnsIpv6Addrs"].as_array().map_or(0, Vec::len);
    assert_eq!(
        ipv6, 0,
        "case {case:?}: expected an IPv4 directory to report no IPv6 DNS address, got \
         {described}"
    );

    // The clock's own two timestamps: both reported, and the stage was last updated no
    // earlier than the directory was created.
    let launched = instant(described, "LaunchTime", case);
    let stage_updated = instant(described, "StageLastUpdatedDateTime", case);
    assert!(
        stage_updated >= launched,
        "case {case:?}: expected the stage to have been updated no earlier than the launch, \
         got launch {launched} and stage update {stage_updated}"
    );

    // The password a create is called with is not part of the directory, so nothing the
    // reporting call answers carries it.
    let rendered = described.to_string();
    assert!(
        !rendered.contains(PASSWORD),
        "case {case:?}: expected the description to carry no copy of the password, got \
         {described}"
    );
}

/// RULE: the whole life of a Managed Microsoft AD this service places in a VPC is ONE walk
/// that a client drives with the reporting operation while the clock advances it: the create
/// is answered while the directory is still being built, so the stages the client reads before
/// it serves are the modelled create chain's, in the order that chain declares them; from the
/// stage it serves in, every member of its description is computed from the create's own
/// request and from what the services that OWN its network hold for it — one interface per
/// chosen subnet, behind the one group provisioned for its controllers; and the teardown a
/// delete starts is reported through the modelled teardown chain and ends with the account
/// holding NOTHING for the id — the reporting call filtered by it is an empty page, the
/// unfiltered inventory does not name it, a call whose subject has to be a live directory is
/// refused for an id that resolves to none while an id the account was never issued is refused
/// as an invalid one, and the owner of network interfaces holds none of its interfaces.
///
/// The rows differ along what the create fixes and what the service derives from it: the
/// edition (and so the size), whether the short name, the description and the network type
/// were given or left to the service, and which pair of the account's subnets the servers are
/// placed in — and in which order the create names them.
#[tokio::test]
async fn the_managed_microsoft_ad_chain_from_create_to_absence_is_one_walk() {
    let network = network().await;
    let router = &network.router;
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));
    let vpc_block = block_of_vpc(&ec2, &network.vpc).await;

    let rows = vec![
        ChainRow {
            case: "a Standard edition with every optional member given",
            name: "chainone.example.com",
            edition: Some("Standard"),
            short_name: Some("CHAINONE"),
            description: Some("the first row of the chain"),
            network_type: Some("IPv4"),
            subnets: vec![network.client[0].clone(), network.client[1].clone()],
            never_issued: "d-0000000000",
        },
        ChainRow {
            case: "an edition, short name, description and network type all left to the service",
            name: "chaintwo.example.com",
            edition: None,
            short_name: None,
            description: None,
            network_type: None,
            subnets: vec![network.client[1].clone(), network.client[2].clone()],
            never_issued: "d-ffffffffff",
        },
        ChainRow {
            case: "an Enterprise edition in a /28 and a /20 named in that order",
            name: "chainthree.example.com",
            edition: Some("Enterprise"),
            short_name: Some("CHAIN-3"),
            description: None,
            network_type: Some("IPv4"),
            subnets: vec![network.client[2].clone(), network.client[3].clone()],
            never_issued: "d-1234abcdef",
        },
    ];

    // Every create is answered BEFORE its directory is in service, which is what a client that
    // polls straight after the call observes: a stage of the chain the build runs through,
    // never the one a directory in service carries.
    let mut walked: Vec<(String, Vec<String>)> = Vec::new();
    for row in &rows {
        let directory_id = create_microsoft_ad(router, row.body(&network.vpc), row.case).await;
        let accepted = directory_stage(router, &directory_id, row.case).await;
        assert!(
            CREATE_CHAIN[..CREATE_CHAIN.len() - 1].contains(&accepted.as_str()),
            "case {case:?}: expected the create to answer while the directory was still being \
             built, got stage {accepted:?}",
            case = row.case
        );
        walked.push((directory_id, vec![accepted]));
    }

    for (row, (directory_id, walk)) in rows.iter().zip(walked.iter_mut()) {
        // The build ends on the clock alone: the reporting operation is polled the way a
        // client's waiter polls it, and every stage it reads on the way is collected.
        poll_until!(row.case, "the directory to report itself in service", {
            let stage = directory_stage(router, directory_id, row.case).await;
            if walk.last() != Some(&stage) {
                walk.push(stage.clone());
            }
            (
                (stage == STAGE_ACTIVE).then_some(stage.clone()),
                format!("stage {stage:?}"),
            )
        });
        assert_walk_of(walk, &CREATE_CHAIN, row.case);
        assert_eq!(
            walk.last().map(String::as_str),
            Some(STAGE_ACTIVE),
            "case {case:?}: expected the walk the create owes to end in service, got {walk:?}",
            case = row.case
        );
    }

    for (row, (directory_id, _)) in rows.iter().zip(walked.iter()) {
        let case = row.case;
        let described = describe_one(router, directory_id, case).await;
        assert_settled_description(&described, row, directory_id);

        // The placement is the one the create named, with the zones the OWNER of subnets
        // reports for those very subnets.
        let placement = &described["VpcSettings"];
        assert_eq!(
            placement["VpcId"],
            json!(network.vpc),
            "case {case:?}: expected the VPC the create named, got {described}"
        );
        assert_eq!(
            placement["SubnetIds"],
            json!(row.subnet_ids()),
            "case {case:?}: expected the subnets the create named, in that order, got \
             {described}"
        );
        let zones: BTreeSet<String> = placement["AvailabilityZones"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected AvailabilityZones, got {described}"))
            .iter()
            .map(|zone| zone.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            zones,
            row.zones(),
            "case {case:?}: expected the zones the chosen subnets sit in, got {described}"
        );
        assert_eq!(
            zones.len(),
            2,
            "case {case:?}: expected a Managed Microsoft AD to span two zones, got {zones:?}"
        );

        // The addresses the directory serves DNS at are the addresses of the interfaces the
        // OWNER of network interfaces holds for it — one per chosen subnet, inside the VPC's
        // own block, each behind the group the placement reports.
        let group_id = string(placement, "SecurityGroupId", case);
        let interfaces = directory_interfaces(&ec2, directory_id, case).await;
        assert_eq!(
            interfaces.len(),
            row.subnets.len(),
            "case {case:?}: expected one interface per chosen subnet, got {interfaces:?}"
        );
        assert_eq!(
            subnets_of(&interfaces),
            row.subnet_ids().into_iter().collect::<BTreeSet<String>>(),
            "case {case:?}: expected the interfaces in the chosen subnets, got {interfaces:?}"
        );
        let addresses = addresses_of(&interfaces);
        assert_eq!(
            dns_addresses(&described, case),
            addresses,
            "case {case:?}: expected the DNS addresses to be the interfaces' own, got \
             {described} against {interfaces:?}"
        );
        for address in &addresses {
            assert_address_in(address, &vpc_block, case);
        }
        for interface in &interfaces {
            assert!(
                interface
                    .groups()
                    .iter()
                    .any(|group| group.group_id() == Some(group_id.as_str())),
                "case {case:?}: expected every interface to sit behind {group_id}, got \
                 {interface:?}"
            );
        }

        // That group is the one the owner of security groups holds for THIS directory's
        // controllers, in the VPC the subnets belong to, and every rule admitting traffic to
        // it is scoped to that VPC's block.
        let group = group_reported_by_ec2(&ec2, &group_id, case).await;
        assert_eq!(
            group.group_name(),
            Some(format!("{directory_id}_controllers").as_str()),
            "case {case:?}: expected the group to be named for the directory's controllers, \
             got {group:?}"
        );
        assert_eq!(
            group.vpc_id(),
            Some(network.vpc.as_str()),
            "case {case:?}: expected the group in the VPC the subnets belong to, got {group:?}"
        );
        assert!(
            !group.ip_permissions().is_empty(),
            "case {case:?}: expected the group to admit the directory's traffic, got {group:?}"
        );
        for permission in group.ip_permissions() {
            for range in permission.ip_ranges() {
                assert_eq!(
                    range.cidr_ip(),
                    Some(vpc_block.as_str()),
                    "case {case:?}: expected every admitted range to be the VPC's own block, \
                     got {permission:?}"
                );
            }
        }
    }

    // The teardown is REPORTED too: the delete answers while the directory is still being torn
    // down, and the chain ends with the record leaving the account's records.
    for (row, (directory_id, _)) in rows.iter().zip(walked.iter()) {
        let case = row.case;
        delete(router, directory_id, case).await;

        // The stages the account's own inventory reports for it on the way out, collected
        // through the reader the teardown suite owns.
        let teardown = stages_until_gone(router, directory_id, case).await;
        assert_walk_of(&teardown, &TEARDOWN_CHAIN, case);
        assert_eq!(
            teardown.first().map(String::as_str),
            Some(STAGE_DELETING),
            "case {case:?}: expected the teardown to be reported from the stage the delete \
             leaves the directory in, got {teardown:?}"
        );

        // What the account keeps is the one fact that it HAD the id: the reporting call
        // filtered by it succeeds with an empty page, while the same call filtered by an id
        // this account was never issued is refused as an invalid one.
        let page = ok(
            router,
            "DescribeDirectories",
            json!({ "DirectoryIds": [directory_id] }),
            case,
        )
        .await;
        assert_eq!(
            descriptions(&page, case),
            Vec::<Value>::new(),
            "case {case:?}: expected an empty page for the deleted id, got {page}"
        );
        refused_with(
            router,
            "DescribeDirectories",
            json!({ "DirectoryIds": [row.never_issued] }),
            &invalid_directory_ids(&[row.never_issued]),
            case,
        )
        .await;

        // And every call whose subject has to be a directory the account HOLDS is refused for
        // the gone id with the other sentence.
        for (operation, body) in [
            ("DeleteDirectory", json!({ "DirectoryId": directory_id })),
            ("GetSnapshotLimits", json!({ "DirectoryId": directory_id })),
        ] {
            refused_with(
                router,
                operation,
                body,
                &does_not_exist(directory_id),
                case,
            )
            .await;
        }

        // The unfiltered inventory does not name it either, and the interfaces its servers
        // answered on are gone from the service that owns them.
        let inventory = ok(router, "DescribeDirectories", json!({}), case).await;
        assert!(
            descriptions(&inventory, case)
                .iter()
                .all(|described| string(described, "DirectoryId", case) != *directory_id),
            "case {case:?}: expected the inventory not to name the deleted directory, got \
             {inventory}"
        );
        assert_eq!(
            directory_interfaces(&ec2, directory_id, case).await.len(),
            0,
            "case {case:?}: expected the directory's interfaces to be released"
        );
    }
}
