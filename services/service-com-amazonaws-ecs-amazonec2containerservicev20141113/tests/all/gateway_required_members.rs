// SPDX-License-Identifier: CC0-1.0

//! What a GATEWAY operation answers a request that leaves out a member it requires, driven end
//! to end over the shared wire harness in [`crate::ecs_wire`].
//!
//! The rule: each gateway operation publishes its OWN refusal for such a member — the modelled
//! error shape, the HTTP status it is answered with, and a sentence that is entirely fixed text
//! — and not the clause a generic required-member pass renders from the member's path. A
//! client branches on the shape (an authorization refusal is a role or a policy to fix, a
//! server failure is a call to retry, a bad parameter is a request to correct) and surfaces or
//! matches on the sentence, so both are part of the answer.
//!
//! The table spans the class rather than one call: every operation of the surface whose input
//! requires an identifier, each with the identifier left out — and, for the two that require a
//! PAIR, with each of the two left out in turn — plus the three members `CreateGateway`
//! requires, each of which publishes a DIFFERENT shape and status. It closes on an operation
//! off this surface, which keeps the generic clause, since nothing here may widen to a member
//! no AWS sentence covers.
//!
//! Nothing here changes WHICH requests are served, or what a member a request DOES carry earns:
//! the last test creates a gateway that names every required member and reads it back, and a
//! gateway this account holds none of is still reported as missing rather than as an invalid
//! identifier.

use serde_json::{json, Value};

use crate::ecs_wire::{answered, ok, service, string, ACCOUNT, SIGNED_REGION};
use crate::express_gateway_services::infrastructure_role;

/// The sentences this surface publishes, each the whole message it answers.
const NAME_REQUIRED: &str = "GatewayName must be provided.";
const SERVICE_UNAVAILABLE: &str = "Service Unavailable. Please try again later.";
const CROSS_ACCOUNT_PASS_ROLE: &str = "Cross-account pass role is not allowed";
const GATEWAY_IDENTIFIER_INVALID: &str = "The gateway identifier is invalid";
const GATEWAY_ROUTE_ARN_INVALID: &str = "The gateway route ARN is invalid";
const ASSOCIATION_DENIED: &str =
    "You are not authorized to perform this operation on the specified gateway, or it does not \
     exist.";

/// The clause the framework's own walk renders for a required member no AWS sentence covers:
/// the value, the member's path and the requirement it failed.
const GENERIC_CLAUSE: &str = "1 validation error detected: Value null at 'tasks' failed to \
                              satisfy constraint: Member must not be null";

/// One row: the call, and the modelled error, status and sentence it earns.
#[derive(Debug)]
struct Row {
    case: &'static str,
    operation: &'static str,
    request: Value,
    status: u16,
    code: &'static str,
    message: &'static str,
}

/// The rows where a gateway's own identifier, or the cluster of an association, is left out:
/// the refusal is keyed on the MEMBER, so it is the same answer whichever operation required
/// it. `smithy_query(query="operation",operation="UpdateGateway,DescribeGateway,DeleteGateway,\
/// DescribeGatewayResources,AssociateGatewayWithCluster,DisassociateGatewayFromCluster")` — the
/// six operations whose input requires a `gateway` — and the two that also require a `cluster`.
fn identifier_rows() -> Vec<Row> {
    let mut rows = Vec::new();
    // The four that require the gateway alone, and the two that require it beside the cluster
    // of the association — whose rows name the cluster, so the member left out is the only one
    // of the pair the request does not carry.
    for (operation, named) in [
        ("UpdateGateway", json!({})),
        ("DescribeGateway", json!({})),
        ("DeleteGateway", json!({})),
        ("DescribeGatewayResources", json!({})),
        (
            "AssociateGatewayWithCluster",
            json!({ "cluster": "edge-cluster" }),
        ),
        (
            "DisassociateGatewayFromCluster",
            json!({ "cluster": "edge-cluster" }),
        ),
    ] {
        rows.push(Row {
            case: "a request that carries no gateway",
            operation,
            request: named,
            status: 400,
            code: "InvalidParameterException",
            message: GATEWAY_IDENTIFIER_INVALID,
        });
    }
    for operation in [
        "AssociateGatewayWithCluster",
        "DisassociateGatewayFromCluster",
    ] {
        rows.push(Row {
            case: "a request that carries no cluster to associate the gateway with",
            operation,
            request: json!({ "gateway": "edge" }),
            status: 400,
            code: "AccessDeniedException",
            message: ASSOCIATION_DENIED,
        });
    }
    rows
}

/// The rows of the members `CreateGateway` requires, the gateway ROUTE read's ARN, and the
/// operation off this surface that keeps the generic clause.
fn create_and_route_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a create that names no gateway name",
            operation: "CreateGateway",
            request: json!({
                "infrastructureRoleArn": infrastructure_role(),
                "configuration": { "hostSharedAlb": {} },
            }),
            status: 400,
            code: "InvalidParameterException",
            message: NAME_REQUIRED,
        },
        Row {
            case: "a create that hands over no infrastructure role",
            operation: "CreateGateway",
            request: json!({
                "name": "edge",
                "configuration": { "hostSharedAlb": {} },
            }),
            status: 500,
            code: "ServerException",
            message: SERVICE_UNAVAILABLE,
        },
        Row {
            case: "a create that names no configuration",
            operation: "CreateGateway",
            request: json!({
                "name": "edge",
                "infrastructureRoleArn": infrastructure_role(),
            }),
            status: 400,
            code: "AccessDeniedException",
            message: CROSS_ACCOUNT_PASS_ROLE,
        },
        Row {
            case: "a route read with no ARN in the body",
            operation: "DescribeGatewayRoute",
            request: json!({}),
            status: 400,
            code: "InvalidParameterException",
            message: GATEWAY_ROUTE_ARN_INVALID,
        },
        Row {
            case: "an operation off this surface, which publishes no sentence of its own",
            operation: "DescribeTasks",
            request: json!({ "cluster": "edge-cluster" }),
            status: 400,
            code: "InvalidParameterException",
            message: GENERIC_CLAUSE,
        },
    ]
}

/// Drive one row and assert the whole answer: status, modelled shape and sentence.
async fn assert_row(row: &Row) {
    let router = service();
    let (status, body) = answered(&router, row.operation, row.request.clone()).await;
    let case = row.case;
    let operation = row.operation;
    assert_eq!(
        status, row.status,
        "case {case:?} on {operation}: expected HTTP {}, got {status} with {body}",
        row.status
    );
    let reported = body["__type"].as_str().unwrap_or_default();
    assert!(
        reported.contains(row.code),
        "case {case:?} on {operation}: expected a {}, got {body}",
        row.code
    );
    assert_eq!(
        body["message"].as_str().unwrap_or_default(),
        row.message,
        "case {case:?} on {operation}: expected the refusal {:?}, got {body}",
        row.message
    );
}

#[tokio::test]
async fn a_required_gateway_identifier_left_out_is_refused_with_the_surfaces_own_answer() {
    for row in identifier_rows() {
        assert_row(&row).await;
    }
}

#[tokio::test]
async fn each_required_member_of_a_create_publishes_its_own_shape_status_and_sentence() {
    for row in create_and_route_rows() {
        assert_row(&row).await;
    }
}

#[tokio::test]
async fn naming_every_required_member_still_creates_and_reads_the_gateway_back() {
    let case = "every required member named";
    let router = service();
    let name = "edge";

    let created = ok(
        &router,
        "CreateGateway",
        json!({
            "name": name,
            "infrastructureRoleArn": infrastructure_role(),
            "configuration": { "hostSharedAlb": {} },
        }),
        case,
    )
    .await;
    let arn = format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:gateway/{name}");
    assert_eq!(
        string(&created, "gatewayArn", case),
        arn,
        "case {case:?}: expected the gateway's own ARN, got {created}"
    );

    // The stored gateway, read back through the describe whose `gateway` member the rows above
    // refuse when the request leaves it out: a member that DOES name a gateway still resolves
    // it.
    let read = ok(&router, "DescribeGateway", json!({ "gateway": name }), case).await;
    assert_eq!(
        string(&read["gateway"], "gatewayArn", case),
        arn,
        "case {case:?}: expected the stored gateway, got {read}"
    );

    // A gateway this account holds none of is still reported MISSING rather than as an
    // identifier the request left out: the refusals above are about a member that is not there,
    // not about what the account holds.
    let case = "a gateway this account holds none of";
    let (status, body) = answered(
        &router,
        "DescribeGateway",
        json!({ "gateway": "nothing-holds-this" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body}"
    );
    assert!(
        body["__type"]
            .as_str()
            .unwrap_or_default()
            .contains("GatewayNotFoundException"),
        "case {case:?}: expected a GatewayNotFoundException, got {body}"
    );
}
