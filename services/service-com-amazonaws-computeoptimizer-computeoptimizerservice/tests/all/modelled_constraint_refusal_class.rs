// SPDX-License-Identifier: CC0-1.0

//! The error class this service refuses a request outside an INPUT BOUND with, and the
//! sentence it states — for the bounds the model publishes, and for the one this service
//! publishes itself where the model declares none.
//!
//! The rule: a request this service refuses for a value its model rejects — a number outside
//! a `@range`, a value outside an enum's value set, a required member left out — is answered
//! with the one bad-input error class the service models, `InvalidParameterValueException`
//! (HTTP 400). The model declares no `ValidationException`, so the generic class the request
//! boundary would otherwise name is an error code no client of this API can catch, whichever
//! operation and whichever member the refusal came from.
//!
//! So the table below spans the refusal, not one call: several operations, several violation
//! kinds (above a range, below a range, outside an enum, absent required member) and
//! paging-size bounds of two different widths — each one past a bound the model publishes,
//! with the bound itself accepted. The refusal also NAMES the input parameter at fault, which
//! each row states. Every call goes through the shared wire harness, so what is asserted is
//! the code, the status and the message a client reads.
//!
//! The last suites are the one bound the model declares NOWHERE: how many resource
//! identifiers one request may name in a reader's list. The model puts no `@length` on
//! `InstanceArns`, `AutoScalingGroupArns`, `VolumeArns`, `FunctionArns`, `ServiceArns`,
//! `ResourceArns` or `JobIds`, so the service's own ceiling is what bounds them — ONE
//! ceiling for every reader that takes such a list, since a bound one reader applies and
//! another does not is a bound a client cannot know. The rows there are every reader of this
//! service that takes an identifier list, each with the identifier form its member
//! documents, crossed with list lengths either side of the ceiling.

use serde_json::{json, Value};
use service_com_amazonaws_computeoptimizer_computeoptimizerservice::state::MAX_REQUESTED_IDENTIFIERS;

use crate::harness::{
    auto_scaling_group_arn, call, emulator, error_code, instance_arn, opt_in, ACCOUNT, REGION,
};

/// The ONE bad-input error class this service models: "The value supplied for the input
/// parameter is out of range or not valid" (HTTP 400). Every operation that takes an input
/// lists it, and no operation lists a `ValidationException`.
const MODELLED_BAD_INPUT: &str = "InvalidParameterValueException";

/// A request refused for a modelled constraint is refused with the service's own bad-input
/// class, whatever the operation and whatever the constraint.
#[tokio::test]
async fn a_modelled_constraint_violation_is_refused_with_the_services_own_error_class() {
    let router = emulator();
    opt_in(&router).await;

    // (case, operation, request, the message the refusal states)
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            "a paging size one above IdleMaxResults' published maximum of 100",
            "GetIdleRecommendations",
            json!({"maxResults": 101}),
            "Invalid Max Results.",
        ),
        (
            "a paging size one below IdleMaxResults' published minimum of 0",
            "GetIdleRecommendations",
            json!({"maxResults": -1}),
            "Invalid Max Results.",
        ),
        (
            "a paging size one above MaxResults' published maximum of 1000",
            "GetLambdaFunctionRecommendations",
            json!({"maxResults": 1001}),
            "Invalid Max Results.",
        ),
        (
            "a paging size one below MaxResults' published minimum of 0",
            "GetEnrollmentStatusesForOrganization",
            json!({"maxResults": -1}),
            "Invalid Max Results.",
        ),
        (
            "a filter name outside IdleRecommendationFilterName",
            "GetIdleRecommendations",
            json!({"filters": [{"name": "tag:Owner", "values": ["TeamA"]}]}),
            "Invalid Name.",
        ),
        (
            "a sort order outside Order",
            "GetIdleRecommendations",
            json!({"orderBy": {"dimension": "SavingsValue", "order": "Descending"}}),
            "Invalid Order.",
        ),
        (
            "a resource type outside ResourceType",
            "PutRecommendationPreferences",
            json!({
                "resourceType": "Ec2InstanceFleet",
                "scope": {"name": "AccountId", "value": ACCOUNT},
            }),
            "Invalid Resource Type.",
        ),
        (
            "a file format outside FileFormat",
            "ExportEC2InstanceRecommendations",
            json!({
                "s3DestinationConfig": {"bucket": "a-destination-bucket"},
                "fileFormat": "Parquet",
            }),
            "Invalid File Format.",
        ),
        (
            "an enrollment status outside Status",
            "UpdateEnrollmentStatus",
            json!({"status": "Enrolled"}),
            "Invalid Status.",
        ),
        (
            "the required status member left out",
            "UpdateEnrollmentStatus",
            json!({}),
            "Invalid Status.",
        ),
        (
            "the required resourceType member left out",
            "GetRecommendationPreferences",
            json!({}),
            "Invalid Resource Type.",
        ),
        (
            "a scope name outside ScopeName",
            "PutRecommendationPreferences",
            json!({
                "resourceType": "Ec2Instance",
                "scope": {"name": "InstanceArn", "value": instance_arn("i-0abcdef1234567890")},
            }),
            "Invalid Name.",
        ),
    ];

    for (case, operation, request, wanted_message) in rows {
        let (status, body) = call(&router, operation, request.clone()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {operation} to refuse {request} with 400, got {status}: \
             {body}"
        );
        assert_eq!(
            error_code(&body),
            MODELLED_BAD_INPUT,
            "case {case:?}: expected {MODELLED_BAD_INPUT}, got {}: {body}",
            error_code(&body)
        );
        let message = body["message"].as_str().unwrap_or_default();
        assert_eq!(
            message, wanted_message,
            "case {case:?}: expected the refusal to state {wanted_message:?}, got {message:?}"
        );
    }
}

/// The bound itself is INSIDE the range: a paging size equal to a published maximum, and one
/// equal to a published minimum, are answered rather than refused — so the refusal above is
/// the constraint talking, not a rejection of every paging size.
#[tokio::test]
async fn a_paging_size_on_a_published_bound_is_answered() {
    let router = emulator();
    opt_in(&router).await;

    // (case, operation, maxResults, the list member the answer carries)
    let rows: Vec<(&str, &str, i64, &str)> = vec![
        (
            "IdleMaxResults' maximum",
            "GetIdleRecommendations",
            100,
            "idleRecommendations",
        ),
        (
            "IdleMaxResults' minimum",
            "GetIdleRecommendations",
            0,
            "idleRecommendations",
        ),
        (
            "MaxResults' maximum",
            "GetLambdaFunctionRecommendations",
            1000,
            "lambdaFunctionRecommendations",
        ),
        (
            "MaxResults' minimum",
            "GetLambdaFunctionRecommendations",
            0,
            "lambdaFunctionRecommendations",
        ),
    ];

    for (case, operation, max_results, member) in rows {
        let (status, body) = call(&router, operation, json!({"maxResults": max_results})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {operation} to answer maxResults={max_results}, got \
             {status}: {body}"
        );
        assert!(
            body[member].is_array(),
            "case {case:?}: expected {member} to be a list, got {body}"
        );
    }
}

/// One reader that takes a caller-supplied list of resource identifiers: the operation, the
/// member the list arrives in, the Nth well-formed identifier of the form that member
/// documents, and the status a request naming identifiers this service holds nothing for is
/// answered with once it is past the length bound.
struct IdentifierList {
    /// The operation name, as `X-Amz-Target` carries it.
    operation: &'static str,
    /// The input member the identifier list arrives in.
    member: &'static str,
    /// The `index`th identifier of the form this member takes. Distinct per index, so a list
    /// of n entries names n resources.
    identifier: fn(usize) -> String,
    /// What this reader answers a list of identifiers it holds nothing for WITHIN the bound
    /// with: the readers report an empty selection (200), while the export-job reader states
    /// that a job id it cannot report names no job (404). Either way the request got past the
    /// length bound, which is what the rows assert.
    within_bound: u16,
}

/// Every reader of this service that takes a caller-supplied identifier list, from each
/// operation's input in this service's Smithy model: `InstanceArns`, `AutoScalingGroupArns`,
/// `VolumeArns`, `FunctionArns`, `ServiceArns`, `ResourceArns` (the license, idle and database
/// readers) and `JobIds` — none of which the model declares a length for.
const IDENTIFIER_LISTS: &[IdentifierList] = &[
    IdentifierList {
        operation: "GetEC2InstanceRecommendations",
        member: "instanceArns",
        identifier: nth_instance_arn,
        within_bound: 200,
    },
    IdentifierList {
        operation: "GetAutoScalingGroupRecommendations",
        member: "autoScalingGroupArns",
        identifier: nth_group_arn,
        within_bound: 200,
    },
    IdentifierList {
        operation: "GetEBSVolumeRecommendations",
        member: "volumeArns",
        identifier: nth_volume_arn,
        within_bound: 200,
    },
    IdentifierList {
        operation: "GetLambdaFunctionRecommendations",
        member: "functionArns",
        identifier: nth_function_arn,
        within_bound: 200,
    },
    IdentifierList {
        operation: "GetECSServiceRecommendations",
        member: "serviceArns",
        identifier: nth_ecs_service_arn,
        within_bound: 200,
    },
    IdentifierList {
        operation: "GetLicenseRecommendations",
        member: "resourceArns",
        identifier: nth_instance_arn,
        within_bound: 200,
    },
    IdentifierList {
        operation: "GetIdleRecommendations",
        member: "resourceArns",
        identifier: nth_instance_arn,
        within_bound: 200,
    },
    IdentifierList {
        operation: "GetRDSDatabaseRecommendations",
        member: "resourceArns",
        identifier: nth_database_arn,
        within_bound: 200,
    },
    IdentifierList {
        operation: "DescribeRecommendationExportJobs",
        member: "jobIds",
        identifier: nth_job_id,
        within_bound: 404,
    },
];

/// The `index`th Amazon EC2 instance ARN — the form `instanceArns` takes, and an ARN the
/// license and idle readers' `resourceArns` take as well.
fn nth_instance_arn(index: usize) -> String {
    instance_arn(&format!("i-{index:017x}"))
}

/// The `index`th Auto Scaling group ARN (`autoScalingGroupArns`).
fn nth_group_arn(index: usize) -> String {
    auto_scaling_group_arn(&format!("web-tier-{index}"))
}

/// The `index`th Amazon EBS volume ARN (`volumeArns`).
fn nth_volume_arn(index: usize) -> String {
    format!("arn:aws:ec2:{REGION}:{ACCOUNT}:volume/vol-{index:017x}")
}

/// The `index`th Lambda function ARN (`functionArns`).
fn nth_function_arn(index: usize) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:checkout-{index}")
}

/// The `index`th Amazon ECS service ARN, in the documented form
/// `arn:aws:ecs:region:aws_account_id:service/cluster-name/service-name` (`serviceArns`).
fn nth_ecs_service_arn(index: usize) -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:service/web/checkout-{index}")
}

/// The `index`th Amazon Aurora or RDS database ARN (the database reader's `resourceArns`).
fn nth_database_arn(index: usize) -> String {
    format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:ledger-{index}")
}

/// The `index`th export-job id (`jobIds`).
fn nth_job_id(index: usize) -> String {
    format!("export-job-{index}")
}

/// A list of `count` distinct identifiers for `row`.
fn identifiers(row: &IdentifierList, count: usize) -> Vec<String> {
    (0..count).map(row.identifier).collect()
}

/// The refusal a list longer than the bound is answered with: it names the member at fault
/// and the bound, and says how many the request named. Built from the service's own ceiling,
/// so one reader cannot state a different number from another.
fn too_long_message(member: &str, named: usize) -> String {
    format!(
        "The value supplied for parameter {member} is not valid: at most \
         {MAX_REQUESTED_IDENTIFIERS} resources can be named per request, not {named}."
    )
}

/// A list of identifiers longer than this service answers for is refused — with the same
/// bound, the same error class and the same sentence, by every reader that takes one.
#[tokio::test]
async fn an_identifier_list_past_the_bound_is_refused_by_every_reader() {
    let router = emulator();
    opt_in(&router).await;

    // The rule generalises over HOW FAR past the bound the list is, so the rows span just
    // past it and far past it.
    for count in [
        MAX_REQUESTED_IDENTIFIERS + 1,
        MAX_REQUESTED_IDENTIFIERS + 2,
        10 * MAX_REQUESTED_IDENTIFIERS,
    ] {
        for row in IDENTIFIER_LISTS {
            let case = format!("{}${} naming {count}", row.operation, row.member);
            let request = json!({row.member: identifiers(row, count)});
            let (status, body) = call(&router, row.operation, request).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected 400, got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                MODELLED_BAD_INPUT,
                "case {case:?}: expected {MODELLED_BAD_INPUT}, got {body}"
            );
            let message = body["message"].as_str().unwrap_or_default();
            let expected = too_long_message(row.member, count);
            assert_eq!(
                message, expected,
                "case {case:?}: expected the refusal to state {expected:?}, got {message:?}"
            );
        }
    }
}

/// The bound itself is INSIDE what the service answers: a list of exactly as many identifiers
/// as the ceiling names, and every shorter one, is answered on its merits rather than refused
/// for its length — so the refusal above is the bound talking, not a rejection of naming
/// resources at all.
#[tokio::test]
async fn an_identifier_list_up_to_the_bound_is_not_refused_for_its_length() {
    let router = emulator();
    opt_in(&router).await;

    // The rows span the lengths within the bound: one identifier, one short of the ceiling,
    // and the ceiling itself.
    for count in [
        1,
        MAX_REQUESTED_IDENTIFIERS - 1,
        MAX_REQUESTED_IDENTIFIERS,
    ] {
        for row in IDENTIFIER_LISTS {
            let case = format!("{}${} naming {count}", row.operation, row.member);
            let request = json!({row.member: identifiers(row, count)});
            let (status, body) = call(&router, row.operation, request).await;
            assert_eq!(
                status, row.within_bound,
                "case {case:?}: expected {}, got {status}: {body}",
                row.within_bound
            );
            let message = body["message"].as_str().unwrap_or_default();
            assert_ne!(
                message,
                too_long_message(row.member, count),
                "case {case:?}: a list within the bound must not be refused for its length, \
                 got {body}"
            );
        }
    }
}
