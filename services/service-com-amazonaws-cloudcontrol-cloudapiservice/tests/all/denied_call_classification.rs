// SPDX-License-Identifier: CC0-1.0

//! WHICH `HandlerErrorCode` a resource operation reports an AUTHORIZATION denial under.
//!
//! The rule every case here holds the service to: **when the work a resource operation owes
//! needs a call the policies in force on the role the request named do not allow, the request
//! terminates `FAILED` with `ErrorCode` `AccessDenied` — the member the model's own
//! `HandlerErrorCode` enum declares for a call that was not authorized
//! (`smithy_query(query="shape",shape="HandlerErrorCode")`), which is a value of that enum
//! distinct from the `GeneralServiceException` reserved for "the downstream service generated
//! an error that doesn't map to any other handler error code" — carrying the sentence the
//! owning service refuses such a call with, and the resource is left exactly as it was.**
//!
//! Which member a failed request carries is what a client classifies the failure by: an
//! authorization denial is permanent until the policies change, so it must not be reported as
//! an unclassifiable downstream fault that a retry layer may treat as transient.
//!
//! The rows differ along what the rule generalises over: which owning service the refused
//! call would have been made on (`AWS::SQS::Queue`, `AWS::SSM::Parameter`,
//! `AWS::Logs::LogGroup`), which resource operation the request is (create, update, delete),
//! which of IAM's rules withheld the grant (an explicit `Deny`, no statement at all, a
//! statement scoped to another resource, a role carrying no policy whatsoever), and whether
//! the resource was there before the operation. The control is the same request once the role
//! IS granted the call, which is carried out.
//!
//! `role_arn_permissions` holds the PERMISSION rule itself — which calls are judged, and what
//! shows nothing about a role — through these same expectations, and
//! `owning_service_failure_classification` holds the codes a refusal the owning service
//! raised about the resource MODEL is reported under.

use serde_json::{json, Value};

use ::service_common::iam_authorization as iam;

use super::emulator::{text, Emulator, SIGNING_REGION, STATUS_FAILED, STATUS_SUCCESS};
use super::role_arn_permissions::{
    allow_but_deny, bare_role, create_request, iam_ok, one_statement, role_with, terminal_event,
};
use super::sqs_queue_read_through::{queue_url, QUEUE};

/// The member of the model's `HandlerErrorCode` enum for a call the resource type's handler
/// was not authorized to make (`smithy_query(query="shape",shape="HandlerErrorCode")`).
pub(crate) const ACCESS_DENIED: &str = "AccessDenied";

/// The name of the SESSION the calls a resource operation makes are taken under.
///
/// A role is never acted AS: a service handed one assumes it and calls as a session of it, so
/// the identity an authorization refusal of such a call is about is
/// `arn:<partition>:sts::<account>:assumed-role/<role name>/<this name>`.
const SESSION_NAME: &str = "AWSCloudControlAPI";

/// Which of IAM's rules withheld the grant, which the refusal names in its reason clause.
#[derive(Clone, Copy)]
pub(crate) enum Withheld {
    /// An explicit `Deny` of the call in one of the role's own documents.
    ExplicitDeny,
    /// No document of the role states anything that allows the call — IAM's default deny.
    NoStatement,
    /// The role's own documents allow the call and the permissions boundary in force on it
    /// does not.
    NoBoundary,
}

impl Withheld {
    /// The reason clause of the refusal, as the workspace's one authorization wording states
    /// each of IAM's rules.
    fn clause(self, action: &str) -> String {
        let action = iam::reported_action(action);
        match self {
            Self::ExplicitDeny => iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
            Self::NoStatement => iam::no_identity_policy_allows(&action),
            Self::NoBoundary => iam::no_permissions_boundary_allows(&action),
        }
    }
}

/// The sentence the owning service refuses an unauthorized call with: the session the call
/// was made as, the action as that service words an action in its own refusals, the resource
/// it was taken on, and which of IAM's rules withheld the grant.
pub(crate) fn denial_sentence(
    role_arn: &str,
    action: &str,
    resource: &str,
    withheld: Withheld,
) -> String {
    iam::not_authorized_by_identity_policy(
        &::service_common::iam_role::assumed_role_session_arn(role_arn, SESSION_NAME),
        &iam::reported_action(action),
        resource,
        &withheld.clause(action),
    )
}

/// Hold a terminal `ProgressEvent` to the verdict the rule states: `FAILED`, the handler
/// error code for a call that was not authorized, and the owning service's own refusal of it.
pub(crate) fn assert_denied(
    case: &str,
    event: &Value,
    role_arn: &str,
    action: &str,
    resource: &str,
    withheld: Withheld,
) {
    assert_eq!(
        text(event, "OperationStatus"),
        STATUS_FAILED,
        "case {case:?}: a call the role may not make must fail the request, got {event:?}"
    );
    assert_eq!(
        text(event, "ErrorCode"),
        ACCESS_DENIED,
        "case {case:?}: expected the {ACCESS_DENIED} handler error code, got {event:?}"
    );
    let expected = denial_sentence(role_arn, action, resource, withheld);
    let reported = text(event, "StatusMessage");
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the status message {expected:?}, got {reported:?}"
    );
}

/// The language version every policy document below declares.
const POLICY_VERSION: &str = "2012-10-17";

/// Which owning service the call a row's handler has to make would have been made on, and so
/// which store the row's resource lives in.
#[derive(Clone, Copy)]
enum Owning {
    /// Amazon SQS, which owns `AWS::SQS::Queue`.
    Sqs,
    /// AWS Systems Manager, which owns `AWS::SSM::Parameter`.
    Ssm,
    /// Amazon CloudWatch Logs, which owns `AWS::Logs::LogGroup`.
    Logs,
}

impl Owning {
    /// One emulator with this service and the IAM service — whose store holds the roles —
    /// mounted beside Cloud Control API on one shared engine.
    fn emulator(self) -> Emulator {
        match self {
            Self::Sqs => Emulator::with_sqs_and_iam(),
            Self::Ssm => Emulator::with_ssm_and_iam(),
            Self::Logs => Emulator::with_cloudwatch_logs_and_iam(),
        }
    }

    /// The `TypeName` of the type this service owns.
    fn type_name(self) -> &'static str {
        match self {
            Self::Sqs => QUEUE,
            Self::Ssm => "AWS::SSM::Parameter",
            Self::Logs => "AWS::Logs::LogGroup",
        }
    }

    /// The desired state a create of `name` carries.
    fn desired(self, name: &str) -> Value {
        match self {
            Self::Sqs => json!({ "QueueName": name }),
            Self::Ssm => json!({ "Name": name, "Type": "String", "Value": "asked-for" }),
            Self::Logs => json!({ "LogGroupName": name }),
        }
    }

    /// The identifier an update or a delete addresses `name` by: for a type whose primary
    /// identifier the owning service ASSIGNS, the address it assigned.
    fn identifier(self, name: &str) -> String {
        match self {
            Self::Sqs => queue_url(name),
            Self::Ssm | Self::Logs => name.to_string(),
        }
    }

    /// The ARN of `name`, as the OWNING service names it — the resource an IAM statement
    /// about it names, and so the resource the refusal is about.
    fn resource_arn(self, name: &str) -> String {
        match self {
            Self::Sqs => super::sqs_queue_read_through::queue_arn(name),
            Self::Ssm => {
                service_com_amazonaws_ssm_amazonssm::state::parameter_arn(SIGNING_REGION, name)
            }
            Self::Logs => service_com_amazonaws_cloudwatchlogs_logs_20140328::state::log_group_arn(
                SIGNING_REGION,
                name,
                true,
            ),
        }
    }

    /// Create `name` through the OWNING service's own API, so a row about an operation on an
    /// EXISTING resource starts from a state this service never wrote.
    async fn create_through_owning_api(self, emulator: &Emulator, name: &str) {
        match self {
            Self::Sqs => {
                emulator
                    .sqs_ok("CreateQueue", json!({ "QueueName": name }))
                    .await;
            }
            Self::Ssm => {
                emulator
                    .ssm_ok(
                        "PutParameter",
                        json!({ "Name": name, "Value": "before", "Type": "String" }),
                    )
                    .await;
            }
            Self::Logs => {
                emulator
                    .logs_ok("CreateLogGroup", json!({ "logGroupName": name }))
                    .await;
            }
        }
    }

    /// Whether the OWNING service's own read reports `name` — the independent read that says
    /// whether a write really happened.
    async fn holds(self, emulator: &Emulator, name: &str) -> bool {
        match self {
            Self::Sqs => {
                let listed = emulator
                    .sqs_ok("ListQueues", json!({ "QueueNamePrefix": name }))
                    .await;
                listed["QueueUrls"]
                    .as_array()
                    .is_some_and(|urls| !urls.is_empty())
            }
            Self::Ssm => {
                let (status, _) = emulator.ssm("GetParameter", json!({ "Name": name })).await;
                status == 200
            }
            Self::Logs => {
                let described = emulator
                    .logs_ok("DescribeLogGroups", json!({ "logGroupNamePrefix": name }))
                    .await;
                described["logGroups"]
                    .as_array()
                    .is_some_and(|groups| !groups.is_empty())
            }
        }
    }
}

/// What the owning service must hold once the refused operation has terminated.
#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    /// Nothing: the refused create wrote no resource.
    Absent,
    /// The resource the row started from, untouched by the refused operation.
    Present,
}

/// One row of the denial table.
struct Denial {
    /// What the row is about, named in every assertion of it.
    case: &'static str,
    /// The service the refused call would have been made on.
    owning: Owning,
    /// The identity policy in force on the role the request names, or `None` for a role
    /// carrying no policy at all.
    policy: Option<Value>,
    /// The resource operation the request is.
    operation: &'static str,
    /// The owning service's API action the handler would have been refused on.
    action: &'static str,
    /// Which of IAM's rules withheld the grant.
    withheld: Withheld,
    /// What the owning service holds afterwards.
    after: After,
}

/// The request a row's operation is, addressed at `name` and naming `role`.
fn request_of(row: &Denial, name: &str, role: &str) -> Value {
    let type_name = row.owning.type_name();
    match row.operation {
        "CreateResource" => create_request(type_name, &row.owning.desired(name), Some(role)),
        "UpdateResource" => json!({
            "TypeName": type_name,
            "Identifier": row.owning.identifier(name),
            "PatchDocument": patch_of(row.owning).to_string(),
            "RoleArn": role,
        }),
        _ => json!({
            "TypeName": type_name,
            "Identifier": row.owning.identifier(name),
            "RoleArn": role,
        }),
    }
}

/// The patch an update row sends: one property of the type, whichever service owns it.
fn patch_of(owning: Owning) -> Value {
    match owning {
        Owning::Sqs => json!([{ "op": "replace", "path": "/VisibilityTimeout", "value": 45 }]),
        Owning::Ssm => json!([{ "op": "replace", "path": "/Value", "value": "patched" }]),
        Owning::Logs => json!([{ "op": "replace", "path": "/RetentionInDays", "value": 7 }]),
    }
}

/// A request whose work needs a call the role it names may not make FAILS with the handler
/// error code for an authorization denial, and the owning service is left as it was.
#[tokio::test]
async fn a_call_the_role_may_not_make_is_reported_as_an_authorization_denial() {
    let rows = [
        Denial {
            case: "a queue create by a role carrying no policy at all",
            owning: Owning::Sqs,
            policy: None,
            operation: "CreateResource",
            action: "sqs:CreateQueue",
            withheld: Withheld::NoStatement,
            after: After::Absent,
        },
        Denial {
            case: "a queue create the role is explicitly denied",
            owning: Owning::Sqs,
            policy: Some(allow_but_deny(json!("sqs:*"), json!("sqs:CreateQueue"))),
            operation: "CreateResource",
            action: "sqs:CreateQueue",
            withheld: Withheld::ExplicitDeny,
            after: After::Absent,
        },
        Denial {
            case: "a queue patch no statement of the role allows",
            owning: Owning::Sqs,
            policy: Some(one_statement("Allow", json!("sqs:CreateQueue"), json!("*"))),
            operation: "UpdateResource",
            action: "sqs:SetQueueAttributes",
            withheld: Withheld::NoStatement,
            after: After::Present,
        },
        Denial {
            case: "a queue delete the role is explicitly denied",
            owning: Owning::Sqs,
            policy: Some(allow_but_deny(json!("sqs:*"), json!("sqs:DeleteQueue"))),
            operation: "DeleteResource",
            action: "sqs:DeleteQueue",
            withheld: Withheld::ExplicitDeny,
            after: After::Present,
        },
        Denial {
            case: "a parameter create the role is granted on another resource only",
            owning: Owning::Ssm,
            policy: Some(one_statement(
                "Allow",
                json!("ssm:PutParameter"),
                json!(service_com_amazonaws_ssm_amazonssm::state::parameter_arn(
                    SIGNING_REGION,
                    "some-other-parameter"
                )),
            )),
            operation: "CreateResource",
            action: "ssm:PutParameter",
            withheld: Withheld::NoStatement,
            after: After::Absent,
        },
        Denial {
            case: "a log group create no statement of the role allows",
            owning: Owning::Logs,
            policy: Some(one_statement("Allow", json!("s3:*"), json!("*"))),
            operation: "CreateResource",
            action: "logs:CreateLogGroup",
            withheld: Withheld::NoStatement,
            after: After::Absent,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let emulator = row.owning.emulator();
        let name = format!("denied-{index}");
        let role_name = format!("denied-role-{index}");
        let role = match &row.policy {
            Some(policy) => role_with(&emulator, &role_name, policy).await,
            None => bare_role(&emulator, &role_name).await,
        };
        if row.after == After::Present {
            row.owning
                .create_through_owning_api(&emulator, &name)
                .await;
        }

        let event = terminal_event(
            &emulator,
            case,
            row.operation,
            request_of(row, &name, &role),
        )
        .await;
        assert_denied(
            case,
            &event,
            &role,
            row.action,
            &row.owning.resource_arn(&name),
            row.withheld,
        );

        let held = row.owning.holds(&emulator, &name).await;
        assert_eq!(
            held,
            row.after == After::Present,
            "case {case:?}: the refused operation must leave the owning service holding \
             exactly what it held before"
        );
    }
}

/// The attribute Amazon SQS reports for the queue `name`, read through that service's own
/// API — what says whether a refused patch changed anything.
async fn native_attribute(emulator: &Emulator, name: &str, attribute: &str) -> String {
    let answered = emulator
        .sqs_ok(
            "GetQueueAttributes",
            json!({ "QueueUrl": queue_url(name), "AttributeNames": [attribute] }),
        )
        .await;
    answered["Attributes"][attribute]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A denial leaves the resource it was about untouched: the refused patch wrote none of the
/// values it asked for, because the question is asked before the provider changes anything.
#[tokio::test]
async fn a_denied_patch_changes_nothing_in_the_owning_service() {
    let emulator = Emulator::with_sqs_and_iam();
    let name = "untouched-queue";
    emulator
        .sqs_ok("CreateQueue", json!({ "QueueName": name }))
        .await;
    // The documented default of the attribute the patch asks to change: `VisibilityTimeout`
    // "Default: 30" (`CreateQueue$Attributes`).
    let before = native_attribute(&emulator, name, "VisibilityTimeout").await;
    assert_eq!(
        before, "30",
        "expected the queue to start at the documented default, got {before:?}"
    );

    let role = role_with(
        &emulator,
        "patch-denied-role",
        &one_statement("Deny", json!("sqs:SetQueueAttributes"), json!("*")),
    )
    .await;
    let case = "a patch the role is explicitly denied";
    let event = terminal_event(
        &emulator,
        case,
        "UpdateResource",
        json!({
            "TypeName": QUEUE,
            "Identifier": queue_url(name),
            "PatchDocument": patch_of(Owning::Sqs).to_string(),
            "RoleArn": &role,
        }),
    )
    .await;
    assert_denied(
        case,
        &event,
        &role,
        "sqs:SetQueueAttributes",
        &super::sqs_queue_read_through::queue_arn(name),
        Withheld::ExplicitDeny,
    );
    let after = native_attribute(&emulator, name, "VisibilityTimeout").await;
    assert_eq!(
        after, before,
        "case {case:?}: the refused patch must not change the attribute, got {after:?}"
    );
}

/// The control the denial is only meaningful against: the SAME request, once the role is
/// granted the call, is carried out — and so is one that names no role at all, which runs
/// with the caller's own credentials.
#[tokio::test]
async fn the_same_request_succeeds_once_the_role_is_granted_the_call() {
    let emulator = Emulator::with_sqs_and_iam();
    let name = "granted-queue";
    let role_name = "granted-role";
    let role = bare_role(&emulator, role_name).await;
    let request = create_request(QUEUE, &Owning::Sqs.desired(name), Some(&role));

    let case = "the create before the grant";
    let denied = terminal_event(&emulator, case, "CreateResource", request.clone()).await;
    assert_denied(
        case,
        &denied,
        &role,
        "sqs:CreateQueue",
        &super::sqs_queue_read_through::queue_arn(name),
        Withheld::NoStatement,
    );
    assert!(
        !Owning::Sqs.holds(&emulator, name).await,
        "case {case:?}: the refused create must write nothing, and must not fall back to the \
         caller's own credentials"
    );

    // The grant the role was missing, written through IAM's own API.
    iam_ok(
        &emulator,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", "queue-operations"),
            (
                "PolicyDocument",
                &json!({
                    "Version": POLICY_VERSION,
                    "Statement": [{ "Effect": "Allow", "Action": "sqs:*", "Resource": "*" }],
                })
                .to_string(),
            ),
        ],
    )
    .await;

    let case = "the identical create after the grant";
    let granted = terminal_event(&emulator, case, "CreateResource", request).await;
    assert_eq!(
        text(&granted, "OperationStatus"),
        STATUS_SUCCESS,
        "case {case:?}: a role granted the call must carry it out, got {granted:?}"
    );
    assert_eq!(
        text(&granted, "Identifier"),
        queue_url(name),
        "case {case:?}: expected the url Amazon SQS assigned the queue, got {granted:?}"
    );
    let resolved = emulator
        .sqs_ok("GetQueueUrl", json!({ "QueueName": name }))
        .await;
    assert_eq!(
        text(&resolved, "QueueUrl"),
        queue_url(name),
        "case {case:?}: the owning service must resolve the queue the create made, got \
         {resolved:?}"
    );

    let case = "a create naming no role";
    let own = terminal_event(
        &emulator,
        case,
        "CreateResource",
        create_request(QUEUE, &Owning::Sqs.desired("own-credentials-queue"), None),
    )
    .await;
    assert_eq!(
        text(&own, "OperationStatus"),
        STATUS_SUCCESS,
        "case {case:?}: a request naming no role is made with the caller's own credentials, \
         got {own:?}"
    );
}
