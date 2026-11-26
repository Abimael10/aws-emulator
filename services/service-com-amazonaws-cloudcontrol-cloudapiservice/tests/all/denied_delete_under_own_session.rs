// SPDX-License-Identifier: CC0-1.0

//! A DELETE the request's role may not make, and the identical delete under the caller's own
//! session.
//!
//! The rule every case here holds the service to: **a resource operation is carried out with
//! the permissions of the role the request names ("the role specified must have the permissions
//! required for this operation", `DeleteResource$RoleArn`), so a delete whose downstream call
//! those permissions withhold terminates `FAILED` carrying the handler's own verdict for an
//! error the downstream service generated and that the handler maps to no code of its own —
//! `ErrorCode` `GeneralServiceException` ("the downstream service generated an error that
//! doesn't map to any other handler error code",
//! `smithy_query(query="shape",shape="HandlerErrorCode")`) and `StatusMessage` the handler's
//! own sentence about the CALL it was making, naming only the owning service's API operation —
//! never the downstream service's own error class or the words it refused the call with; the
//! owning service's resource is left exactly as it was, and the identical request naming NO
//! role is made with the caller's own credentials and carried out.**
//!
//! So a client classifying a failed request reads the handler's code, and a client parsing the
//! message finds the operation name and nothing of the authorization decision behind it — no
//! account id, role name or resource ARN, which are per-run values rather than part of the
//! sentence.
//!
//! The rows differ along what the rule generalises over: which owning service the withheld call
//! would have been made on (`AWS::SSM::Parameter`, `AWS::Logs::LogGroup`, `AWS::SQS::Queue`),
//! which downstream operation the delete path calls, and how the grant is withheld (an explicit
//! `Deny` scoped to the one resource's ARN, an explicit `Deny` over every resource). Every row
//! carries the SAME role through all three steps, so each one is also the asymmetry the rule
//! rests on: the role may create, and the create it carries out is the control the refused
//! delete is read against.
//!
//! `denied_call_classification` and `denied_call_downstream_code` hold the classification over
//! creates, patches and deletes; this suite holds the SEQUENCE a client drives — refused under
//! the role, carried out under the caller's own session — end to end on three owning services.

use serde_json::{json, Value};

use service_com_amazonaws_cloudcontrol_cloudapiservice::ops::types::HANDLER_ERROR_CODE_VALUES;

use super::denied_call_classification::{assert_denied, DENIED_CALL_CODE};
use super::emulator::{text, Emulator, SIGNING_REGION, STATUS_SUCCESS};
use super::role_arn_permissions::{allow_but_deny, create_request, role_with, terminal_event};

/// The language version the one policy document written here declares.
const POLICY_VERSION: &str = "2012-10-17";

/// The error code AWS Systems Manager answers for a parameter it does not hold.
const PARAMETER_NOT_FOUND: &str = "ParameterNotFound";

/// The value every row's create asks the owning service to hold, so the read after the refused
/// delete says whether anything about the resource moved.
const CREATED_VALUE: &str = "created-by-the-service-role";

/// The parameter the Systems Manager row creates, named once: its own ARN is what that row's
/// `Deny` is scoped to.
const PARAMETER_NAME: &str = "/denied-delete/parameter";

/// Wording a DOWNSTREAM authorization refusal carries that the handler's own sentence must not:
/// the session the call was made as, the action it asked for and which of IAM's rules withheld
/// the grant are the downstream service's diagnosis, which the handler relays none of.
const DOWNSTREAM_DIAGNOSIS: &[&str] = &[
    "is not authorized to perform",
    "assumed-role",
    "identity-based policy",
];

/// Which owning service a row's resource lives in — and so which store the refused call would
/// have been made on, and which API says whether it happened.
#[derive(Clone, Copy)]
enum Owning {
    /// AWS Systems Manager, which owns `AWS::SSM::Parameter`.
    Ssm,
    /// Amazon CloudWatch Logs, which owns `AWS::Logs::LogGroup`.
    Logs,
    /// Amazon SQS, which owns `AWS::SQS::Queue`.
    Sqs,
}

impl Owning {
    /// One emulator with this service and the IAM service — whose store holds the roles and the
    /// policies in force on them — mounted beside Cloud Control API on one shared engine.
    fn emulator(self) -> Emulator {
        match self {
            Self::Ssm => Emulator::with_ssm_and_iam(),
            Self::Logs => Emulator::with_cloudwatch_logs_and_iam(),
            Self::Sqs => Emulator::with_sqs_and_iam(),
        }
    }

    /// The `TypeName` of the type this service owns.
    fn type_name(self) -> &'static str {
        match self {
            Self::Ssm => "AWS::SSM::Parameter",
            Self::Logs => "AWS::Logs::LogGroup",
            Self::Sqs => "AWS::SQS::Queue",
        }
    }

    /// The desired state the row's create carries for the resource `name`.
    fn desired(self, name: &str) -> Value {
        match self {
            Self::Ssm => json!({ "Name": name, "Type": "String", "Value": CREATED_VALUE }),
            Self::Logs => json!({ "LogGroupName": name }),
            Self::Sqs => json!({ "QueueName": name }),
        }
    }

    /// Hold the OWNING service's own read to a resource `name` it holds — the independent read
    /// that says the create landed and the refused delete removed nothing.
    async fn assert_held(self, emulator: &Emulator, case: &str, name: &str) {
        match self {
            Self::Ssm => {
                let read = emulator
                    .ssm_ok("GetParameter", json!({ "Name": name }))
                    .await;
                let parameter = &read["Parameter"];
                assert_eq!(
                    (parameter["Value"].as_str(), parameter["Version"].as_i64()),
                    (Some(CREATED_VALUE), Some(1)),
                    "case {case:?}: expected the parameter the create asked for, untouched at \
                     its first version, got {read:?}"
                );
            }
            Self::Logs => {
                let described = emulator
                    .logs_ok("DescribeLogGroups", json!({ "logGroupNamePrefix": name }))
                    .await;
                let groups = described["logGroups"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                assert_eq!(
                    groups.len(),
                    1,
                    "case {case:?}: expected the owning service to hold the one log group, got \
                     {described:?}"
                );
            }
            Self::Sqs => {
                let resolved = emulator
                    .sqs_ok("GetQueueUrl", json!({ "QueueName": name }))
                    .await;
                assert!(
                    !text(&resolved, "QueueUrl").is_empty(),
                    "case {case:?}: expected the owning service to resolve the queue, got \
                     {resolved:?}"
                );
            }
        }
    }

    /// Hold that service's own read to holding NO resource `name` — and, where it words a
    /// refusal for one it does not hold, to answering that refusal.
    async fn assert_absent(self, emulator: &Emulator, case: &str, name: &str) {
        match self {
            Self::Ssm => {
                let (status, body) = emulator.ssm("GetParameter", json!({ "Name": name })).await;
                assert_ne!(
                    status, 200,
                    "case {case:?}: the parameter must be gone from the owning service, got \
                     {body:?}"
                );
                assert!(
                    body.to_string().contains(PARAMETER_NOT_FOUND),
                    "case {case:?}: expected {PARAMETER_NOT_FOUND} for a parameter the owning \
                     service does not hold, got {status} with {body:?}"
                );
            }
            Self::Logs => {
                let described = emulator
                    .logs_ok("DescribeLogGroups", json!({ "logGroupNamePrefix": name }))
                    .await;
                let groups = described["logGroups"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                assert!(
                    groups.is_empty(),
                    "case {case:?}: the log group must be gone from the owning service, got \
                     {described:?}"
                );
            }
            Self::Sqs => {
                let listed = emulator
                    .sqs_ok("ListQueues", json!({ "QueueNamePrefix": name }))
                    .await;
                let urls = listed["QueueUrls"].as_array().cloned().unwrap_or_default();
                assert!(
                    urls.is_empty(),
                    "case {case:?}: the queue must be gone from the owning service, got \
                     {listed:?}"
                );
            }
        }
    }
}

/// One row: a role that may create the resource and may not remove it, on one owning service.
struct Row {
    /// What the row is about, named in every assertion of it.
    case: &'static str,
    /// The service the withheld call would have been made on.
    owning: Owning,
    /// The resource the row creates and then tries to delete.
    name: &'static str,
    /// The identity document in force on the role: a grant wide enough that only its own `Deny`
    /// can withhold the removal.
    policy: Value,
    /// The owning service's API action the type's delete path calls, which the `Deny` withholds.
    denied_action: &'static str,
}

/// A delete the role may not make fails under the handler's own catch-all, changes nothing in
/// the owning service, and is then carried out by the identical request naming no role.
#[tokio::test]
async fn a_denied_delete_is_the_handlers_catch_all_and_the_own_session_delete_succeeds() {
    let rows = [
        Row {
            case: "a parameter delete denied on that one parameter's ARN",
            owning: Owning::Ssm,
            name: PARAMETER_NAME,
            // The shape a service role scoped by an explicit deny takes: every call of the
            // owning service allowed, and the removal denied on the one resource's own ARN —
            // which the owning service builds from the Region and the name.
            policy: json!({
                "Version": POLICY_VERSION,
                "Statement": [
                    { "Effect": "Allow", "Action": "ssm:*", "Resource": "*" },
                    {
                        "Effect": "Deny",
                        "Action": "ssm:DeleteParameter",
                        "Resource": service_com_amazonaws_ssm_amazonssm::state::parameter_arn(
                            SIGNING_REGION,
                            PARAMETER_NAME,
                        ),
                    },
                ],
            }),
            denied_action: "ssm:DeleteParameter",
        },
        Row {
            case: "a log group delete denied on every resource",
            owning: Owning::Logs,
            name: "denied-delete-group",
            policy: allow_but_deny(json!("logs:*"), json!("logs:DeleteLogGroup")),
            denied_action: "logs:DeleteLogGroup",
        },
        Row {
            case: "a queue delete denied on every resource",
            owning: Owning::Sqs,
            name: "denied-delete-queue",
            policy: allow_but_deny(json!("sqs:*"), json!("sqs:DeleteQueue")),
            denied_action: "sqs:DeleteQueue",
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let emulator = row.owning.emulator();
        let type_name = row.owning.type_name();
        let role = role_with(&emulator, &format!("denied-delete-role-{index}"), &row.policy).await;

        // The control the refusal is read against: the SAME role may create, and the create it
        // carries out is what leaves a resource for the delete to be refused on.
        let created = terminal_event(
            &emulator,
            case,
            "CreateResource",
            create_request(type_name, &row.owning.desired(row.name), Some(&role)),
        )
        .await;
        let reported_status = text(&created, "OperationStatus");
        assert_eq!(
            reported_status, STATUS_SUCCESS,
            "case {case:?}: the role may make the create's call, so it must be carried out, got \
             {reported_status:?} with {created:?}"
        );
        let identifier = text(&created, "Identifier");
        assert!(
            !identifier.is_empty(),
            "case {case:?}: the create must report the identifier the resource answers to, got \
             {created:?}"
        );
        row.owning.assert_held(&emulator, case, row.name).await;

        // The delete under that role: the call its policies withhold, so the request fails with
        // the handler's own verdict and the resource stays.
        let denied = terminal_event(
            &emulator,
            case,
            "DeleteResource",
            json!({
                "TypeName": type_name,
                "Identifier": &identifier,
                "RoleArn": &role,
            }),
        )
        .await;
        assert_denied(case, &denied, row.denied_action);
        let code = text(&denied, "ErrorCode");
        assert!(
            HANDLER_ERROR_CODE_VALUES.contains(&code.as_str()),
            "case {case:?}: {code:?} is not a value the model's HandlerErrorCode enum declares"
        );
        assert_eq!(
            code, DENIED_CALL_CODE,
            "case {case:?}: the delete path reports the handler's catch-all rather than the \
             downstream service's own error class, got {code:?} with {denied:?}"
        );
        // And the sentence beside it is the handler's own: none of the downstream service's
        // diagnosis, and none of the per-run values that diagnosis names.
        let reported = text(&denied, "StatusMessage");
        for fragment in DOWNSTREAM_DIAGNOSIS {
            assert!(
                !reported.contains(fragment),
                "case {case:?}: the handler relays nothing the downstream service said, yet the \
                 message carries {fragment:?}: {reported:?}"
            );
        }
        assert!(
            !reported.contains(&role),
            "case {case:?}: the role the request named is a per-run value the sentence must not \
             carry, got {reported:?}"
        );
        row.owning.assert_held(&emulator, case, row.name).await;

        // The identical delete naming NO role is made with the caller's own credentials, which
        // this service judges nothing about: it is carried out, and the resource is gone.
        let own_session = terminal_event(
            &emulator,
            case,
            "DeleteResource",
            json!({ "TypeName": type_name, "Identifier": &identifier }),
        )
        .await;
        let reported_status = text(&own_session, "OperationStatus");
        assert_eq!(
            reported_status, STATUS_SUCCESS,
            "case {case:?}: the identical delete naming no role runs with the caller's own \
             credentials, got {reported_status:?} with {own_session:?}"
        );
        row.owning.assert_absent(&emulator, case, row.name).await;
    }
}
