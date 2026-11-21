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
//!
//! The ceiling is a property of the REQUEST, not of what its entries turn out to name, so
//! those suites also cross every reader with the FORMS its entries can take — well-formed
//! identifiers, one identifier repeated, values of no identifier's form, identifiers of
//! another account — and with an account that never enrolled: a list past the ceiling is
//! refused for its length in every one of those, because the length is decided before
//! anything is parsed, resolved or selected per identifier and before the enrollment gate.
//! Within the ceiling the entries are what answer, so the bound preempts that work without
//! replacing it.

use serde_json::{json, Value};
use service_com_amazonaws_computeoptimizer_computeoptimizerservice::state::MAX_REQUESTED_IDENTIFIERS;

use crate::harness::{
    auto_scaling_group_arn, call, emulator, error_code, instance_arn, opt_in, ACCOUNT, REGION,
};

/// The ONE bad-input error class this service models: "The value supplied for the input
/// parameter is out of range or not valid" (HTTP 400). Every operation that takes an input
/// lists it, and no operation lists a `ValidationException`.
const MODELLED_BAD_INPUT: &str = "InvalidParameterValueException";

/// The refusal every reader of this service answers a request from an account that is not
/// enrolled with (HTTP 403) — the gate a bad input is refused AHEAD of.
const OPT_IN_REQUIRED: &str = "OptInRequiredException";

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
    /// The `index`th value this member's documented form does NOT admit, or `None` where no
    /// string is malformed as one of its identifiers (an export-job id is an opaque string).
    /// Within the bound such a list is refused on its ENTRIES; past the bound the length is
    /// what answers, whichever entries the list carries.
    malformed: Option<fn(usize) -> String>,
    /// The `index`th identifier of this member's form owned by ANOTHER account, or `None`
    /// where the identifiers this member takes carry no account (`jobIds`).
    other_account: Option<fn(usize) -> String>,
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
        malformed: Some(nth_not_an_identifier),
        other_account: Some(nth_other_account_instance_arn),
    },
    IdentifierList {
        operation: "GetAutoScalingGroupRecommendations",
        member: "autoScalingGroupArns",
        identifier: nth_group_arn,
        within_bound: 200,
        malformed: Some(nth_not_an_identifier),
        other_account: Some(nth_other_account_group_arn),
    },
    IdentifierList {
        operation: "GetEBSVolumeRecommendations",
        member: "volumeArns",
        identifier: nth_volume_arn,
        within_bound: 200,
        malformed: Some(nth_not_an_identifier),
        other_account: Some(nth_other_account_volume_arn),
    },
    IdentifierList {
        operation: "GetLambdaFunctionRecommendations",
        member: "functionArns",
        identifier: nth_function_arn,
        within_bound: 200,
        malformed: Some(nth_not_an_identifier),
        other_account: Some(nth_other_account_function_arn),
    },
    IdentifierList {
        operation: "GetECSServiceRecommendations",
        member: "serviceArns",
        identifier: nth_ecs_service_arn,
        within_bound: 200,
        malformed: Some(nth_not_an_identifier),
        other_account: Some(nth_other_account_ecs_service_arn),
    },
    IdentifierList {
        operation: "GetLicenseRecommendations",
        member: "resourceArns",
        identifier: nth_instance_arn,
        within_bound: 200,
        malformed: Some(nth_not_an_identifier),
        other_account: Some(nth_other_account_instance_arn),
    },
    IdentifierList {
        operation: "GetIdleRecommendations",
        member: "resourceArns",
        identifier: nth_instance_arn,
        within_bound: 200,
        malformed: Some(nth_not_an_identifier),
        other_account: Some(nth_other_account_instance_arn),
    },
    IdentifierList {
        operation: "GetRDSDatabaseRecommendations",
        member: "resourceArns",
        identifier: nth_database_arn,
        within_bound: 200,
        malformed: Some(nth_not_an_identifier),
        other_account: Some(nth_other_account_database_arn),
    },
    IdentifierList {
        operation: "DescribeRecommendationExportJobs",
        member: "jobIds",
        identifier: nth_job_id,
        within_bound: 404,
        malformed: None,
        other_account: None,
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

/// A well-formed 12-digit account id that is NOT the one this service answers for: the
/// documentation's own example account. An identifier of this account names a resource no
/// request to this endpoint is answered about, so a list of them is work this service would
/// never do — which is why the length bound has to answer such a list first.
const OTHER_ACCOUNT: &str = "123456789012";

/// The `index`th value no member's documented form admits: a bare word is no Amazon Resource
/// Name, so every reader whose list takes ARNs refuses it on its own merits — within the
/// bound.
fn nth_not_an_identifier(index: usize) -> String {
    format!("not-an-arn-{index}")
}

/// The `index`th Amazon EC2 instance ARN of [`OTHER_ACCOUNT`].
fn nth_other_account_instance_arn(index: usize) -> String {
    format!("arn:aws:ec2:{REGION}:{OTHER_ACCOUNT}:instance/i-{index:017x}")
}

/// The `index`th Auto Scaling group ARN of [`OTHER_ACCOUNT`].
fn nth_other_account_group_arn(index: usize) -> String {
    format!(
        "arn:aws:autoscaling:{REGION}:{OTHER_ACCOUNT}:autoScalingGroup:\
         12345678-1234-1234-1234-123456789012:autoScalingGroupName/web-tier-{index}"
    )
}

/// The `index`th Amazon EBS volume ARN of [`OTHER_ACCOUNT`].
fn nth_other_account_volume_arn(index: usize) -> String {
    format!("arn:aws:ec2:{REGION}:{OTHER_ACCOUNT}:volume/vol-{index:017x}")
}

/// The `index`th Lambda function ARN of [`OTHER_ACCOUNT`].
fn nth_other_account_function_arn(index: usize) -> String {
    format!("arn:aws:lambda:{REGION}:{OTHER_ACCOUNT}:function:checkout-{index}")
}

/// The `index`th Amazon ECS service ARN of [`OTHER_ACCOUNT`].
fn nth_other_account_ecs_service_arn(index: usize) -> String {
    format!("arn:aws:ecs:{REGION}:{OTHER_ACCOUNT}:service/web/checkout-{index}")
}

/// The `index`th Amazon Aurora or RDS database ARN of [`OTHER_ACCOUNT`].
fn nth_other_account_database_arn(index: usize) -> String {
    format!("arn:aws:rds:{REGION}:{OTHER_ACCOUNT}:db:ledger-{index}")
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

/// The entry forms a list of `count` identifiers can arrive in for one reader: the case name
/// and the list itself. The length of a list is a property of the REQUEST, so the forms span
/// what the entries can be — identifiers of the member's own form, one identifier repeated (a
/// list naming ONE resource is still a list of `count` entries), values of no identifier's
/// form at all, and identifiers of an account this service answers nothing about.
fn entry_forms(row: &IdentifierList, count: usize) -> Vec<(&'static str, Vec<String>)> {
    let mut forms: Vec<(&'static str, Vec<String>)> = vec![
        ("identifiers of this member's form", identifiers(row, count)),
        (
            "one identifier repeated",
            vec![(row.identifier)(0); count],
        ),
    ];
    if let Some(malformed) = row.malformed {
        forms.push((
            "values of no identifier's form",
            (0..count).map(malformed).collect(),
        ));
    }
    if let Some(other_account) = row.other_account {
        forms.push((
            "identifiers of another account",
            (0..count).map(other_account).collect(),
        ));
    }
    forms
}

/// The length bound is answered FIRST: a list longer than the ceiling is refused for its
/// length whatever its entries are — well-formed, one identifier repeated, values of no
/// identifier's form, or identifiers of another account — because the length is decided
/// before anything is parsed, resolved or selected per identifier. A reader that parsed the
/// entries first would answer a parse or permission refusal here, and one of them would
/// answer a 200 for the entries it tolerates.
#[tokio::test]
async fn a_list_past_the_bound_is_refused_for_its_length_whatever_its_entries_are() {
    assert_ne!(
        OTHER_ACCOUNT, ACCOUNT,
        "the other-account rows must not name the account this service answers for ({ACCOUNT})"
    );
    let router = emulator();
    opt_in(&router).await;

    let count = MAX_REQUESTED_IDENTIFIERS + 1;
    for row in IDENTIFIER_LISTS {
        for (form, entries) in entry_forms(row, count) {
            let case = format!("{}${} naming {count} {form}", row.operation, row.member);
            let request = json!({row.member: entries});
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

/// Within the bound the ENTRIES are what answer: a list of as many values as the ceiling
/// admits, none of them of the form the member documents, is refused for what it names rather
/// than for how much it names — so the length bound above preempts the per-identifier work
/// without replacing it.
#[tokio::test]
async fn a_within_bound_list_is_still_judged_on_its_entries() {
    let router = emulator();
    opt_in(&router).await;

    let count = MAX_REQUESTED_IDENTIFIERS;
    for row in IDENTIFIER_LISTS {
        let Some(malformed) = row.malformed else {
            continue;
        };
        let case = format!(
            "{}${} naming {count} values of no identifier's form",
            row.operation, row.member
        );
        let entries: Vec<String> = (0..count).map(malformed).collect();
        let (status, body) = call(&router, row.operation, json!({row.member: entries})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the entries to be refused with 400, got {status}: {body}"
        );
        let message = body["message"].as_str().unwrap_or_default();
        assert_ne!(
            message,
            too_long_message(row.member, count),
            "case {case:?}: a list within the bound must not be refused for its length, got \
             {body}"
        );
    }
}

/// The length bound is a property of the request, so it is answered before the account's
/// enrollment is consulted: on an account that never opted in, a list past the ceiling is
/// still refused for its length rather than with the opt-in refusal that reader answers every
/// other request with.
#[tokio::test]
async fn a_list_past_the_bound_is_refused_before_the_opt_in_gate() {
    let router = emulator();

    // The gate is CLOSED on this emulator: nothing opted the account in, so a request that
    // carries no over-long list is answered with the opt-in refusal.
    let (status, body) = call(&router, "GetEC2InstanceRecommendations", json!({})).await;
    assert_eq!(
        status, 403,
        "expected the opt-in gate to refuse a request from an account that never opted in, \
         got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        OPT_IN_REQUIRED,
        "expected {OPT_IN_REQUIRED}, got {body}"
    );

    let count = MAX_REQUESTED_IDENTIFIERS + 1;
    for row in IDENTIFIER_LISTS {
        let case = format!(
            "{}${} naming {count} identifiers without opting in",
            row.operation, row.member
        );
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
