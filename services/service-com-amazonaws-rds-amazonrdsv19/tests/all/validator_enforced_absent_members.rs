// SPDX-License-Identifier: CC0-1.0

//! Which statement this Query front door answers an ABSENT required parameter with.
//!
//! The rule: every member this model marks `@required` also carries `@clientOptional`, so a
//! request missing one reaches the service, and what it is answered with follows the LAYER
//! that reports it — under one and the same `InvalidParameterValue` code at HTTP 400:
//!
//! * the request validator in front of the cluster create reports it as the constraint the
//!   request broke, in the framework's clause for an absent member —
//!   `1 validation error detected: Value null at '<member>' failed to satisfy constraint:
//!   Member must not be null`, one clause per absent member and the count in front of them;
//! * every other operation of this door reports it by naming the parameter the caller has to
//!   supply — `Please specify a value for parameter '<member>'.` — which is the sibling suite
//!   `all/missing_parameter_refusal_sentences.rs`, and is asserted here too as the CONTROL
//!   that the two statements are told apart by the input and not by the kind of request.
//!
//! The rows span what the rule generalises over: each of the two members the cluster create's
//! input requires (a plainly spelled one and one whose spelling opens with the API's
//! acronym), both of them absent at once, and — as controls — a report, a create and a
//! modification of other inputs, plus a cluster create carrying both members, which is
//! SERVED and reads back.
//!
//! Every call goes over the wire through the harness `db_instances.rs` keeps, so each
//! assertion reads the XML the emulator itself answered with.

use crate::db_instances::{call, emulator, expect_error, expect_ok, text, texts};

/// The Query API common error this front door answers every parameter it was not given
/// acceptably with, whichever layer reported it.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The clause the request validator states an absent member with, written out here rather
/// than read off the crate under test: `count` is the number of absent members and `clauses`
/// are their own clauses, joined as the envelope joins them.
fn validator_message(clauses: &[String]) -> String {
    match clauses {
        [one] => format!("1 validation error detected: {one}"),
        many => format!(
            "{} validation errors detected: {}",
            many.len(),
            many.join("; ")
        ),
    }
}

/// One absent member's clause, as the framework words that class.
fn absent_member_clause(member: &str) -> String {
    format!("Value null at '{member}' failed to satisfy constraint: Member must not be null")
}

/// A cluster create carrying exactly the parameters given.
async fn create_cluster(
    router: &smithy_server_runtime::MultiServiceRouter,
    params: &[(&str, &str)],
) -> (u16, String) {
    call(router, "CreateDBCluster", params).await
}

/// One request that leaves a member of the cluster create's input out: the case, the
/// parameters it does carry, and the members the report must name, in the order the input
/// declares them.
struct AbsentMemberRow {
    case: &'static str,
    params: Vec<(&'static str, &'static str)>,
    members: Vec<&'static str>,
}

#[tokio::test]
async fn the_cluster_creates_absent_members_are_reported_by_the_request_validator() {
    let router = emulator();

    let rows = vec![
        AbsentMemberRow {
            case: "a cluster create with no engine",
            params: vec![("DBClusterIdentifier", "emu-validator-no-engine")],
            members: vec!["engine"],
        },
        AbsentMemberRow {
            case: "a cluster create with no identifier",
            params: vec![("Engine", "aurora-postgresql")],
            members: vec!["dBClusterIdentifier"],
        },
        AbsentMemberRow {
            case: "a cluster create carrying neither",
            params: vec![("DatabaseName", "emuvalidator")],
            members: vec!["dBClusterIdentifier", "engine"],
        },
        AbsentMemberRow {
            case: "a cluster create with nothing at all",
            params: Vec::new(),
            members: vec!["dBClusterIdentifier", "engine"],
        },
    ];

    for AbsentMemberRow {
        case,
        params,
        members,
    } in &rows
    {
        let body = expect_error(
            case,
            create_cluster(&router, params).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let clauses: Vec<String> = members.iter().map(|member| absent_member_clause(member)).collect();
        let expected = validator_message(&clauses);
        let message = text(&body, "Message");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        // The refusal is typed as the caller's own to correct on this protocol, and the
        // envelope names the request it answers.
        let fault_type = text(&body, "Type");
        assert_eq!(
            fault_type, "Sender",
            "case {case:?}: expected a Sender fault, got {fault_type:?} in {body}"
        );
        assert!(
            !text(&body, "RequestId").is_empty(),
            "case {case:?}: expected the error envelope to carry a RequestId, got {body}"
        );
    }

    // Every refusal above was a refusal: the account holds no cluster.
    let page = expect_ok(
        "the clusters the account holds after the refusals",
        call(&router, "DescribeDBClusters", &[("", ""); 0]).await,
    );
    assert!(
        texts(&page, "DBClusterIdentifier").is_empty(),
        "a refused create must store nothing, got {page}"
    );

    // And the same create carrying both members is SERVED and reads back, so the rows above
    // are about the absent member and not about the call.
    expect_ok(
        "a cluster create carrying both members",
        create_cluster(
            &router,
            &[
                ("DBClusterIdentifier", "emu-validator-served"),
                ("Engine", "aurora-postgresql"),
            ],
        )
        .await,
    );
    let read_back = expect_ok(
        "the created cluster",
        call(
            &router,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", "emu-validator-served")],
        )
        .await,
    );
    assert_eq!(
        text(&read_back, "DBClusterIdentifier"),
        "emu-validator-served",
        "expected the created cluster to be reported, got {read_back}"
    );
}

/// One CONTROL request, of an input whose absent parameter the operation itself reports: the
/// case, the action, the parameters it does carry, and the parameter it has to supply.
struct NamedParameterRow {
    case: &'static str,
    action: &'static str,
    params: Vec<(&'static str, &'static str)>,
    parameter: &'static str,
}

#[tokio::test]
async fn every_other_input_reports_its_absent_parameter_by_naming_it() {
    let router = emulator();

    // The CONTROL rows: a report, a create and a modification of other inputs — the absent
    // parameter is NAMED, with nothing else of the request in the sentence.
    let rows = vec![
        NamedParameterRow {
            case: "an option report with nothing at all",
            action: "DescribeOrderableDBInstanceOptions",
            params: Vec::new(),
            parameter: "engine",
        },
        NamedParameterRow {
            case: "a subnet group create with no name",
            action: "CreateDBSubnetGroup",
            params: vec![
                ("DBSubnetGroupDescription", "emulator subnets"),
                ("SubnetIds.SubnetIdentifier.1", "subnet-0123456789abcdef0"),
            ],
            parameter: "dBSubnetGroupName",
        },
        NamedParameterRow {
            case: "an instance modification carrying only a setting",
            action: "ModifyDBInstance",
            params: vec![("BackupRetentionPeriod", "7")],
            parameter: "dBInstanceIdentifier",
        },
    ];

    for NamedParameterRow {
        case,
        action,
        params,
        parameter,
    } in &rows
    {
        let body = expect_error(
            case,
            call(&router, action, params).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&body, "Message");
        let expected = format!("Please specify a value for parameter '{parameter}'.");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}
