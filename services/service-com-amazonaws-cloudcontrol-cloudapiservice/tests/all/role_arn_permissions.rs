// SPDX-License-Identifier: CC0-1.0

//! Cloud Control API acting as the IAM ROLE a resource operation request named.
//!
//! The rule every case here holds the service to: **a resource operation is carried out with
//! the permissions of the role the request names — "the ARN of the Identity and Access
//! Management (IAM) role for Cloud Control API to use when performing this resource
//! operation. The role specified must have the permissions required for this operation"
//! (`CreateResource$RoleArn`) — so when the identity-based policies in force on that role do
//! not allow the owning service's call the type's handler has to make, the request reaches a
//! terminal `FAILED` `ProgressEvent` carrying the handler error code for a call that was not
//! authorized, and the owning service's resource is left exactly as it was.**
//! A `SUCCESS` means the call happened, which is why a request the role could not have made
//! must not report one.
//!
//! WHICH code that is, and the sentence beside it, are `denied_call_classification`'s rule;
//! this suite holds the PERMISSION question itself through those same expectations.
//!
//! The rows differ along what the rule generalises over: which resource operation the
//! request is (create, update, delete), which of IAM's rules withholds the grant (an
//! explicit `Deny`, no statement at all, a statement scoped to another resource, a
//! permissions boundary), whether the resource was there before the operation, and which
//! owning service the call would have been made on — `AWS::SSM::Parameter` and
//! `AWS::Logs::LogGroup`, so what is asserted is the permission question rather than
//! anything about one type.

use serde_json::{json, Value};

use super::denied_call_classification::{assert_denied, Withheld};
use super::emulator::{element, progress, text, Emulator, STATUS_SUCCESS};

/// The type whose resources live in the Systems Manager service's own store.
const PARAMETER: &str = "AWS::SSM::Parameter";

/// A type whose resources live in the CloudWatch Logs service's own store, so one case holds
/// the rule on a SECOND owning service.
const LOG_GROUP: &str = "AWS::Logs::LogGroup";

/// The error code Systems Manager answers for a parameter it does not hold.
const PARAMETER_NOT_FOUND: &str = "ParameterNotFound";

/// The language version every policy document below declares.
const POLICY_VERSION: &str = "2012-10-17";

/// A policy document stating one `effect` over `actions` and `resources`.
pub(crate) fn one_statement(effect: &str, actions: Value, resources: Value) -> Value {
    json!({
        "Version": POLICY_VERSION,
        "Statement": [{ "Effect": effect, "Action": actions, "Resource": resources }],
    })
}

/// A policy document that allows `allowed` everywhere and explicitly denies `denied`
/// everywhere — the shape of a service role scoped by an explicit deny.
pub(crate) fn allow_but_deny(allowed: Value, denied: Value) -> Value {
    json!({
        "Version": POLICY_VERSION,
        "Statement": [
            { "Effect": "Allow", "Action": allowed, "Resource": "*" },
            { "Effect": "Deny", "Action": denied, "Resource": "*" },
        ],
    })
}

/// The trust document a role carries, which is what lets this service act as it.
fn trusts_cloud_control() -> Value {
    json!({
        "Version": POLICY_VERSION,
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "cloudcontrolapi.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// One IAM call, required to succeed, answering the XML envelope it reported.
pub(crate) async fn iam_ok(emulator: &Emulator, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = emulator.iam(action, params).await;
    assert_eq!(
        status, 200,
        "iam:{action}: expected HTTP 200, got {status} with body {body}"
    );
    body
}

/// Create the role `name` carrying NO policy at all, answering the ARN IAM assigned it — a
/// role that may make no call of any service, which is the state a freshly created role is
/// in until something is attached to it.
pub(crate) async fn bare_role(emulator: &Emulator, name: &str) -> String {
    let created = iam_ok(
        emulator,
        "CreateRole",
        &[
            ("RoleName", name),
            (
                "AssumeRolePolicyDocument",
                &trusts_cloud_control().to_string(),
            ),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    assert!(
        !arn.is_empty(),
        "role {name:?}: IAM must report the ARN it assigned, got {created}"
    );
    arn
}

/// Create the role `name` carrying `policy` as its one inline policy, answering the ARN IAM
/// assigned it — the value a request's `RoleArn` names.
pub(crate) async fn role_with(emulator: &Emulator, name: &str, policy: &Value) -> String {
    let arn = bare_role(emulator, name).await;
    iam_ok(
        emulator,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "resource-operations"),
            ("PolicyDocument", &policy.to_string()),
        ],
    )
    .await;
    arn
}

/// Start `operation` with `request` and wait for the terminal `ProgressEvent` of the resource
/// operation request it accepted.
pub(crate) async fn terminal_event(
    emulator: &Emulator,
    case: &str,
    operation: &str,
    request: Value,
) -> Value {
    let (status, body) = emulator.cc(operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} expected HTTP 200, got {status} with body {body:?}"
    );
    let accepted = progress(&body);
    emulator.terminal(&text(&accepted, "RequestToken")).await
}

/// The `CreateResource` request for `type_name` and `desired`, carrying `role_arn` when the
/// case names one.
pub(crate) fn create_request(type_name: &str, desired: &Value, role_arn: Option<&str>) -> Value {
    let mut request = json!({
        "TypeName": type_name,
        "DesiredState": desired.to_string(),
    });
    if let Some(role_arn) = role_arn {
        request["RoleArn"] = json!(role_arn);
    }
    request
}

/// The `Parameter` Systems Manager's own read reports for `name`, or `None` when it holds
/// none — the independent read that says whether the write really happened.
async fn native_parameter(emulator: &Emulator, case: &str, name: &str) -> Option<Value> {
    let (status, body) = emulator.ssm("GetParameter", json!({ "Name": name })).await;
    if status == 200 {
        return Some(body["Parameter"].clone());
    }
    assert!(
        body.to_string().contains(PARAMETER_NOT_FOUND),
        "case {case:?}: expected {PARAMETER_NOT_FOUND} for a parameter that is not there, got \
         {status} with body {body:?}"
    );
    None
}

/// One row of the denial table: the case, the policy in force on the role, the operation the
/// request is, whether the parameter is already there, the OWNING service's API action the
/// handler would have been refused on, and which of IAM's rules withheld the grant.
struct Denial {
    case: &'static str,
    policy: Value,
    operation: &'static str,
    existing: bool,
    action: &'static str,
    withheld: Withheld,
}

/// A request naming a role whose policies do not allow the call the handler has to make
/// FAILS, and the parameter in the owning service is left as it was.
///
/// The rows differ along which operation the request is, which of IAM's rules withholds the
/// grant, and whether the resource was there to begin with.
#[tokio::test]
async fn a_call_the_requests_role_may_not_make_fails_it_and_writes_nothing() {
    let emulator = Emulator::with_ssm_and_iam();
    let rows = [
        Denial {
            case: "a create the role is explicitly denied the write by",
            policy: allow_but_deny(json!("ssm:*"), json!("ssm:PutParameter")),
            operation: "CreateResource",
            existing: false,
            action: "ssm:PutParameter",
            withheld: Withheld::ExplicitDeny,
        },
        Denial {
            case: "a create no statement of the role allows",
            policy: one_statement("Allow", json!("s3:*"), json!("*")),
            operation: "CreateResource",
            existing: false,
            action: "ssm:PutParameter",
            withheld: Withheld::NoStatement,
        },
        Denial {
            case: "a create the role is granted the write on another resource only",
            policy: one_statement(
                "Allow",
                json!("ssm:PutParameter"),
                json!(service_com_amazonaws_ssm_amazonssm::state::parameter_arn(
                    super::emulator::SIGNING_REGION,
                    "some-other-parameter"
                )),
            ),
            operation: "CreateResource",
            existing: false,
            action: "ssm:PutParameter",
            withheld: Withheld::NoStatement,
        },
        Denial {
            case: "a patch no statement of the role allows",
            policy: one_statement("Allow", json!("ssm:GetParameter"), json!("*")),
            operation: "UpdateResource",
            existing: true,
            action: "ssm:PutParameter",
            withheld: Withheld::NoStatement,
        },
        Denial {
            case: "a delete the role is explicitly denied the removal by",
            policy: allow_but_deny(json!("ssm:*"), json!("ssm:DeleteParameter")),
            operation: "DeleteResource",
            existing: true,
            action: "ssm:DeleteParameter",
            withheld: Withheld::ExplicitDeny,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let name = format!("denied-{index}");
        let role = role_with(&emulator, &format!("denied-role-{index}"), &row.policy).await;
        // A row about an operation on an EXISTING parameter writes it through the owning
        // service's own API, so the state the case starts from owes this service nothing.
        if row.existing {
            emulator
                .ssm_ok(
                    "PutParameter",
                    json!({ "Name": &name, "Value": "before", "Type": "String" }),
                )
                .await;
        }
        let request = match row.operation {
            "CreateResource" => create_request(
                PARAMETER,
                &json!({ "Name": &name, "Type": "String", "Value": "asked-for" }),
                Some(&role),
            ),
            "UpdateResource" => json!({
                "TypeName": PARAMETER,
                "Identifier": &name,
                "PatchDocument": json!([
                    { "op": "replace", "path": "/Value", "value": "patched" }
                ])
                .to_string(),
                "RoleArn": &role,
            }),
            _ => json!({
                "TypeName": PARAMETER,
                "Identifier": &name,
                "RoleArn": &role,
            }),
        };
        let event = terminal_event(&emulator, case, row.operation, request).await;
        assert_denied(
            case,
            &event,
            &role,
            row.action,
            &service_com_amazonaws_ssm_amazonssm::state::parameter_arn(
                super::emulator::SIGNING_REGION,
                &name,
            ),
            row.withheld,
        );

        // And the owning service holds exactly what it held before: nothing for a create
        // that was refused, and the untouched first version for the other two.
        let held = native_parameter(&emulator, case, &name).await;
        if row.existing {
            let held = held.unwrap_or_else(|| {
                panic!("case {case:?}: the refused operation must leave the parameter in place")
            });
            assert_eq!(
                held["Value"].as_str(),
                Some("before"),
                "case {case:?}: the refused operation must not change the value, got {held:?}"
            );
            assert_eq!(
                held["Version"].as_i64(),
                Some(1),
                "case {case:?}: the refused operation must write no version, got {held:?}"
            );
        } else {
            assert!(
                held.is_none(),
                "case {case:?}: the refused create must write nothing, got {held:?}"
            );
        }
    }
}

/// A request naming a role whose policies DO allow the calls the handler makes is carried
/// out: the whole lifecycle lands in the owning service, which is what a `SUCCESS` means.
#[tokio::test]
async fn a_role_the_policies_allow_carries_the_operation_out() {
    let emulator = Emulator::with_ssm_and_iam();
    let role = role_with(
        &emulator,
        "allowed-role",
        &one_statement("Allow", json!("ssm:*"), json!("*")),
    )
    .await;
    let name = "allowed-parameter";

    let created = terminal_event(
        &emulator,
        "create",
        "CreateResource",
        create_request(
            PARAMETER,
            &json!({ "Name": name, "Type": "String", "Value": "first" }),
            Some(&role),
        ),
    )
    .await;
    assert_eq!(
        text(&created, "OperationStatus"),
        STATUS_SUCCESS,
        "case create: a role granted the write must carry it out, got {created:?}"
    );
    let held = native_parameter(&emulator, "create", name)
        .await
        .unwrap_or_else(|| panic!("case create: Systems Manager must hold the parameter"));
    assert_eq!(
        (held["Value"].as_str(), held["Version"].as_i64()),
        (Some("first"), Some(1)),
        "case create: expected the value the desired state asked for at version 1, got {held:?}"
    );

    let patched = terminal_event(
        &emulator,
        "patch",
        "UpdateResource",
        json!({
            "TypeName": PARAMETER,
            "Identifier": name,
            "PatchDocument": json!([
                { "op": "replace", "path": "/Value", "value": "second" }
            ])
            .to_string(),
            "RoleArn": &role,
        }),
    )
    .await;
    assert_eq!(
        text(&patched, "OperationStatus"),
        STATUS_SUCCESS,
        "case patch: a role granted the write must carry it out, got {patched:?}"
    );
    let held = native_parameter(&emulator, "patch", name)
        .await
        .unwrap_or_else(|| panic!("case patch: Systems Manager must hold the parameter"));
    assert_eq!(
        (held["Value"].as_str(), held["Version"].as_i64()),
        (Some("second"), Some(2)),
        "case patch: each patch of the value is one new version, got {held:?}"
    );

    let removed = terminal_event(
        &emulator,
        "delete",
        "DeleteResource",
        json!({ "TypeName": PARAMETER, "Identifier": name, "RoleArn": &role }),
    )
    .await;
    assert_eq!(
        text(&removed, "OperationStatus"),
        STATUS_SUCCESS,
        "case delete: a role granted the removal must carry it out, got {removed:?}"
    );
    assert!(
        native_parameter(&emulator, "delete", name).await.is_none(),
        "case delete: the parameter must be gone from Systems Manager"
    );
}

/// The permissions boundary in force on the role caps what its own policies grant, so a call
/// the boundary does not allow is withheld however widely the role is granted it.
#[tokio::test]
async fn the_boundary_in_force_on_the_role_caps_what_it_may_do() {
    let emulator = Emulator::with_ssm_and_iam();
    let role = role_with(
        &emulator,
        "capped-role",
        &one_statement("Allow", json!("ssm:*"), json!("*")),
    )
    .await;
    let created = iam_ok(
        &emulator,
        "CreatePolicy",
        &[
            ("PolicyName", "only-object-storage"),
            (
                "PolicyDocument",
                &one_statement("Allow", json!("s3:*"), json!("*")).to_string(),
            ),
        ],
    )
    .await;
    let boundary = element(&created, "Arn");
    assert!(
        !boundary.is_empty(),
        "IAM must report the ARN of the policy it created, got {created}"
    );
    iam_ok(
        &emulator,
        "PutRolePermissionsBoundary",
        &[
            ("RoleName", "capped-role"),
            ("PermissionsBoundary", &boundary),
        ],
    )
    .await;

    let case = "a create only the role's own policies allow";
    let name = "capped-parameter";
    let event = terminal_event(
        &emulator,
        case,
        "CreateResource",
        create_request(
            PARAMETER,
            &json!({ "Name": name, "Type": "String", "Value": "asked-for" }),
            Some(&role),
        ),
    )
    .await;
    assert_denied(
        case,
        &event,
        &role,
        "ssm:PutParameter",
        &service_com_amazonaws_ssm_amazonssm::state::parameter_arn(
            super::emulator::SIGNING_REGION,
            name,
        ),
        Withheld::NoBoundary,
    );
    assert!(
        native_parameter(&emulator, case, name).await.is_none(),
        "case {case:?}: the refused create must write nothing"
    );
}

/// Nothing is withheld on evidence this account does not hold: a request that named no role
/// at all is made with the caller's own credentials, and a `RoleArn` the account holds no
/// role for may belong to another account — so both are carried out.
#[tokio::test]
async fn a_request_the_account_can_show_nothing_about_is_carried_out() {
    let emulator = Emulator::with_ssm_and_iam();
    let unheld = format!(
        "arn:aws:iam::{}:role/held-by-another-account",
        engine::emulator_account_id()
    );
    let rows: [(&str, Option<&str>); 2] = [
        ("a request naming no role", None),
        ("a request naming a role this account does not hold", Some(&unheld)),
    ];
    for (index, (case, role_arn)) in rows.iter().enumerate() {
        let name = format!("unjudged-{index}");
        let event = terminal_event(
            &emulator,
            case,
            "CreateResource",
            create_request(
                PARAMETER,
                &json!({ "Name": &name, "Type": "String", "Value": "asked-for" }),
                *role_arn,
            ),
        )
        .await;
        assert_eq!(
            text(&event, "OperationStatus"),
            STATUS_SUCCESS,
            "case {case:?}: nothing is withheld on evidence the account does not hold, got \
             {event:?}"
        );
        let held = native_parameter(&emulator, case, &name).await;
        assert_eq!(
            held.as_ref().and_then(|held| held["Value"].as_str()),
            Some("asked-for"),
            "case {case:?}: Systems Manager must hold the parameter the create asked for, got \
             {held:?}"
        );
    }
}

/// The same rule on a SECOND owning service: a role that may not make the call
/// `AWS::Logs::LogGroup`'s handler makes fails the request and creates no group, and a role
/// that may make it creates one the owning service's own read reports.
#[tokio::test]
async fn the_rule_holds_for_every_owning_service_the_calls_are_made_on() {
    let emulator = Emulator::with_cloudwatch_logs_and_iam();
    let rows: [(&str, Value, bool); 2] = [
        (
            "denied-group",
            allow_but_deny(json!("logs:*"), json!("logs:CreateLogGroup")),
            false,
        ),
        (
            "allowed-group",
            one_statement("Allow", json!("logs:*"), json!("*")),
            true,
        ),
    ];
    for (name, policy, allowed) in rows {
        let role = role_with(&emulator, &format!("{name}-role"), &policy).await;
        let event = terminal_event(
            &emulator,
            name,
            "CreateResource",
            create_request(LOG_GROUP, &json!({ "LogGroupName": name }), Some(&role)),
        )
        .await;
        let groups = emulator
            .logs_ok(
                "DescribeLogGroups",
                json!({ "logGroupNamePrefix": name }),
            )
            .await;
        let held = groups["logGroups"].as_array().cloned().unwrap_or_default();
        if allowed {
            assert_eq!(
                text(&event, "OperationStatus"),
                STATUS_SUCCESS,
                "case {name:?}: a role granted the call must carry it out, got {event:?}"
            );
            assert_eq!(
                held.len(),
                1,
                "case {name:?}: the owning service must hold the group, got {held:?}"
            );
        } else {
            assert_denied(
                name,
                &event,
                &role,
                "logs:CreateLogGroup",
                &service_com_amazonaws_cloudwatchlogs_logs_20140328::state::log_group_arn(
                    super::emulator::SIGNING_REGION,
                    name,
                    true,
                ),
                Withheld::ExplicitDeny,
            );
            assert!(
                held.is_empty(),
                "case {name:?}: the refused create must write nothing, got {held:?}"
            );
        }
    }
}
