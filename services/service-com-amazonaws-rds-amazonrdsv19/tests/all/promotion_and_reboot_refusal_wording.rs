// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SENTENCES three refusals of this front door carry:
//! `PromoteReadReplica`'s `DBInstanceNotFoundFault`, `RebootDBCluster`'s
//! `DBClusterNotFoundFault`, and the `DBClusterNotFoundFault` the two operations that promote
//! a global database's primary answer a target that does not exist.
//!
//! The rule under test: a refusal's message is the sentence Amazon RDS words for THAT
//! operation's error class — the fixed text is the requirement and only the identifiers it
//! interpolates come from the request — so one error shape reached through two front doors
//! carries two sentences, and a sentence with a variable word takes that word from the
//! request too:
//!
//! * the promotion answers `DB Instance not found: <DBInstanceIdentifier>`, where the
//!   identifier is the one the request carried (only the lookup folds it, since the member
//!   "is stored as a lowercase string");
//! * the reboot answers `The source cluster could not be found or cannot be accessed:
//!   <DBClusterIdentifier>`, which does not distinguish a cluster that is absent from one
//!   that cannot be reached — unlike the report of the same cluster, which states it is
//!   absent;
//! * a promotion of a global database's primary answers `A DB cluster with ARN <target>
//!   doesn't exist. Choose another <kind> target and try again.`, where `<kind>` is the kind
//!   of promotion the REQUEST asked for: `switchover` for `SwitchoverGlobalCluster`, and for
//!   `FailoverGlobalCluster` whichever of the two its flag members ask for — `Switchover`
//!   says outright, and where the request omits it "allowing data loss triggers a global
//!   failover operation" while "if you don't specify AllowDataLoss, the global database
//!   cluster operation defaults to a switchover".
//!
//! The status and the code of each are the modelled pair
//! (`smithy_query(query="operation", operation="PromoteReadReplica,RebootDBCluster,FailoverGlobalCluster")`):
//! 404 with `DBInstanceNotFound`, `DBClusterNotFoundFault` and `DBClusterNotFoundFault`.
//!
//! Each table spans what its rule generalises over: the shape of an identifier that names
//! nothing (plain, mixed case, the shortest the member admits, one at the published bound, an
//! ARN form, and one naming a resource of another Region) and — for the promotion of a global
//! database — every combination of the two members that say which kind of promotion is asked
//! for, against both operations and both forms of the target reference. The recorded request
//! is ONE row of each.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so each assertion
//! reads the XML the emulator itself answered with. Nothing waits for a transition: every row
//! is a refusal decided on the request and the stored records as they already are.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, call_in, emulator, expect_error, expect_ok, text, REGION};

/// The canonical account id every generated emulator presents, for the ARNs a promotion
/// target is named by.
const ACCOUNT: &str = "000000000000";

/// A second Region, so a resource that exists but is out of the signed Region's scope is a
/// row of the tables below.
const OTHER_REGION: &str = "eu-central-1";

/// `DBInstanceNotFoundFault`'s `awsQueryError` code.
const DB_INSTANCE_NOT_FOUND: &str = "DBInstanceNotFound";

/// `DBClusterNotFoundFault`'s `awsQueryError` code.
const DB_CLUSTER_NOT_FOUND: &str = "DBClusterNotFoundFault";

/// The sentence `PromoteReadReplica` answers an identifier that names no DB instance with,
/// spelled out here so the test pins the wording rather than reading it from the code under
/// test: fixed text, then the identifier the request carried.
fn promotion_not_found_sentence(named: &str) -> String {
    format!("DB Instance not found: {named}")
}

/// The sentence `RebootDBCluster` answers a reference that names no DB cluster with.
fn reboot_not_found_sentence(named: &str) -> String {
    format!("The source cluster could not be found or cannot be accessed: {named}")
}

/// The sentence a promotion of a global database's primary answers a target that is no DB
/// cluster with, for the kind of promotion the request asked for.
fn promotion_target_missing_sentence(target: &str, kind: &str) -> String {
    format!(
        "A DB cluster with ARN {target} doesn't exist. Choose another {kind} target and try \
         again."
    )
}

/// The ARN of the DB cluster `identifier` in `region`.
fn cluster_arn(region: &str, identifier: &str) -> String {
    format!("arn:aws:rds:{region}:{ACCOUNT}:cluster:{identifier}")
}

/// An identifier of the longest length the DB cluster member publishes ("Must contain from 1
/// to 63 letters, numbers, or hyphens"), so the table reaches the published bound.
fn longest_identifier() -> String {
    format!("e{}", "0".repeat(62))
}

/// `CreateDBInstance` in `region`, so a row can name an instance that exists OUTSIDE the
/// Region the refusal is signed for.
async fn create_instance(router: &MultiServiceRouter, region: &str, identifier: &str) {
    expect_ok(
        identifier,
        call_in(
            router,
            region,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", identifier),
                ("Engine", "postgres"),
                ("DBInstanceClass", "db.m5.large"),
            ],
        )
        .await,
    );
}

/// `CreateDBCluster` of an Aurora cluster in `region`.
async fn create_cluster(router: &MultiServiceRouter, region: &str, identifier: &str) {
    expect_ok(
        identifier,
        call_in(
            router,
            region,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", identifier),
                ("Engine", "aurora-mysql"),
            ],
        )
        .await,
    );
}

#[tokio::test]
async fn the_promotions_instance_not_found_sentence_is_the_one_this_operation_words() {
    let router = emulator();
    // An instance held in the signed Region throughout, so every 404 below is a decision
    // about the identifier the request named and never "this service holds no instance".
    create_instance(&router, REGION, "promote-wording-held").await;
    // One that exists only in ANOTHER Region: RDS is regional, so it names nothing here.
    create_instance(&router, OTHER_REGION, "promote-wording-elsewhere").await;

    let rows = [
        ("a plain identifier nothing created", "promote-wording-absent".to_string()),
        ("a mixed-case spelling of the same", "Promote-Wording-Absent".to_string()),
        ("the shortest identifier the member admits", "p".to_string()),
        ("one at the published length bound", longest_identifier()),
        ("a near miss of the stored instance", "promote-wording-hel".to_string()),
        (
            "an instance of another Region",
            "promote-wording-elsewhere".to_string(),
        ),
    ];

    for (case, named) in &rows {
        let body = expect_error(
            case,
            call(&router, "PromoteReadReplica", &[("DBInstanceIdentifier", named)]).await,
            404,
            DB_INSTANCE_NOT_FOUND,
        );
        let message = text(&body, "Message");
        let expected = promotion_not_found_sentence(named);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // The REPORT of the same absent instance models the same fault and words it its own
        // way, so the two front doors must not answer one undifferentiated string. What the
        // report's sentence is belongs to the report, which is why only the difference is
        // asserted here.
        let reported = text(
            &expect_error(
                case,
                call(
                    &router,
                    "DescribeDBInstances",
                    &[("DBInstanceIdentifier", named)],
                )
                .await,
                404,
                DB_INSTANCE_NOT_FOUND,
            ),
            "Message",
        );
        assert_ne!(
            message, reported,
            "case {case:?}: each front door words the one fault its own way, both answered \
             {message:?}"
        );
    }

    // The instance the store DOES hold is not reported absent: it is refused for the state it
    // is in, which is what makes the rows above about the identifier.
    let held = expect_error(
        "the stored instance",
        call(
            &router,
            "PromoteReadReplica",
            &[("DBInstanceIdentifier", "promote-wording-held")],
        )
        .await,
        400,
        "InvalidDBInstanceState",
    );
    let message = text(&held, "Message");
    assert_ne!(
        message,
        promotion_not_found_sentence("promote-wording-held"),
        "case \"the stored instance\": an instance that exists must not be reported absent, \
         got {message:?}"
    );
}

#[tokio::test]
async fn the_reboots_cluster_not_found_sentence_names_a_source_it_could_not_find_or_access() {
    let router = emulator();
    // A cluster held in the signed Region throughout, so every 404 below is about the
    // reference the request carried.
    create_cluster(&router, REGION, "reboot-wording-held").await;
    create_cluster(&router, OTHER_REGION, "reboot-wording-elsewhere").await;

    // `(case, the reference the request carries, the identifier it NAMES)` — an ARN form is
    // reduced to the identifier it names before the lookup, so that is what the sentence
    // quotes.
    let rows = [
        (
            "a plain identifier nothing created",
            "reboot-wording-absent".to_string(),
            "reboot-wording-absent".to_string(),
        ),
        (
            "a mixed-case spelling of the same",
            "Reboot-Wording-Absent".to_string(),
            "Reboot-Wording-Absent".to_string(),
        ),
        (
            "the shortest identifier the member admits",
            "c".to_string(),
            "c".to_string(),
        ),
        (
            "one at the published length bound",
            longest_identifier(),
            longest_identifier(),
        ),
        (
            "the ARN form of an absent cluster",
            cluster_arn(REGION, "reboot-wording-arn"),
            "reboot-wording-arn".to_string(),
        ),
        (
            "a cluster of another Region",
            "reboot-wording-elsewhere".to_string(),
            "reboot-wording-elsewhere".to_string(),
        ),
    ];

    for (case, reference, named) in &rows {
        let body = expect_error(
            case,
            call(
                &router,
                "RebootDBCluster",
                &[("DBClusterIdentifier", reference)],
            )
            .await,
            404,
            DB_CLUSTER_NOT_FOUND,
        );
        let message = text(&body, "Message");
        let expected = reboot_not_found_sentence(named);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // The report of the same cluster states that it is ABSENT; the reboot does not
        // distinguish absent from unreachable, so the two sentences differ.
        let reported = text(
            &expect_error(
                case,
                call(
                    &router,
                    "DescribeDBClusters",
                    &[("DBClusterIdentifier", reference)],
                )
                .await,
                404,
                DB_CLUSTER_NOT_FOUND,
            ),
            "Message",
        );
        assert_ne!(
            message, reported,
            "case {case:?}: each front door words the one fault its own way, both answered \
             {message:?}"
        );
    }

    // The cluster the store holds is not reported absent: the reboot refuses it for what it
    // is (an Aurora cluster is rebooted through its member instances), which is what makes
    // the rows above about the reference.
    let held = expect_error(
        "the stored cluster",
        call(
            &router,
            "RebootDBCluster",
            &[("DBClusterIdentifier", "reboot-wording-held")],
        )
        .await,
        400,
        "InvalidParameterValue",
    );
    let message = text(&held, "Message");
    assert_ne!(
        message,
        reboot_not_found_sentence("reboot-wording-held"),
        "case \"the stored cluster\": a cluster that exists must not be reported absent, got \
         {message:?}"
    );
}

/// One promotion of a global database's primary whose target names no DB cluster: the
/// operation, the members that say which kind of promotion is asked for, and the word the
/// refusal must name.
struct TargetMissingCase {
    case: &'static str,
    action: &'static str,
    /// `Switchover` and `AllowDataLoss` as the request carries them, if at all.
    flags: &'static [(&'static str, &'static str)],
    /// The kind of target the caller is told to choose another of.
    kind: &'static str,
}

/// Both operations against every combination of the two members that decide what the request
/// asks for: `Switchover` asks to switch over, `AllowDataLoss` "triggers a global failover
/// operation", and the two "can't be specified together".
const TARGET_MISSING_CASES: &[TargetMissingCase] = &[
    TargetMissingCase {
        case: "a failover that names neither member",
        action: "FailoverGlobalCluster",
        flags: &[],
        kind: "switchover",
    },
    TargetMissingCase {
        case: "a failover asked to switch over",
        action: "FailoverGlobalCluster",
        flags: &[("Switchover", "true")],
        kind: "switchover",
    },
    TargetMissingCase {
        case: "a failover asked not to switch over",
        action: "FailoverGlobalCluster",
        flags: &[("Switchover", "false")],
        kind: "failover",
    },
    TargetMissingCase {
        case: "a failover allowing data loss",
        action: "FailoverGlobalCluster",
        flags: &[("AllowDataLoss", "true")],
        kind: "failover",
    },
    TargetMissingCase {
        case: "a failover not allowing data loss",
        action: "FailoverGlobalCluster",
        flags: &[("AllowDataLoss", "false")],
        kind: "switchover",
    },
    TargetMissingCase {
        case: "the switchover operation",
        action: "SwitchoverGlobalCluster",
        flags: &[],
        kind: "switchover",
    },
];

#[tokio::test]
async fn a_promotion_names_the_kind_of_target_the_request_asked_for() {
    let router = emulator();
    // A global database that exists and is available, so each row below is a decision about
    // the TARGET and never about the global database. It is created available and owes no
    // transition, so nothing is waited for.
    expect_ok(
        "a global database",
        call(
            &router,
            "CreateGlobalCluster",
            &[
                ("GlobalClusterIdentifier", "emu-gcw-global"),
                ("Engine", "aurora-mysql"),
            ],
        )
        .await,
    );

    // The forms the member publishes for the target: "Use the Amazon Resource Name (ARN) for
    // the identifier so that Aurora can locate the cluster in its Amazon Web Services
    // Region", and the bare identifier the same member also admits. Each names no cluster.
    let targets = [
        ("an ARN of the signed Region", cluster_arn(REGION, "emu-gcw-absent")),
        ("an ARN of another Region", cluster_arn(OTHER_REGION, "emu-gcw-absent")),
        ("a bare identifier", "emu-gcw-absent".to_string()),
    ];

    for row in TARGET_MISSING_CASES {
        for (shape, target) in &targets {
            let case = format!("{}, {shape}", row.case);
            let mut params: Vec<(&str, String)> = vec![
                ("GlobalClusterIdentifier", "emu-gcw-global".to_string()),
                ("TargetDbClusterIdentifier", target.clone()),
            ];
            for (member, value) in row.flags {
                params.push((member, (*value).to_string()));
            }
            let body = expect_error(
                &case,
                call(&router, row.action, &params).await,
                404,
                DB_CLUSTER_NOT_FOUND,
            );
            let message = text(&body, "Message");
            let expected = promotion_target_missing_sentence(target, row.kind);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }
}
