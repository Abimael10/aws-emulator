// SPDX-License-Identifier: CC0-1.0

//! A call the owning service refused for want of a GRANT is one of its downstream errors.
//!
//! The rule every case here holds the service to: **a resource operation's handler makes the
//! owning service's calls with the credentials of the role the request named, so a call that
//! service would refuse for want of a grant is an error that service generated which the
//! handler maps to no verdict of its own — the request terminates `FAILED` under the catch-all
//! the model declares for exactly that, `GeneralServiceException` ("the downstream service
//! generated an error that doesn't map to any other handler error code",
//! `smithy_query(query="doc",shape="GeneralServiceException")`), with the handler's own
//! sentence about the CALL it was making as the `StatusMessage` — the same answer any other
//! failure of that call carries, because the handler relays nothing the downstream service
//! said.** A fault in the resource MODEL is not a downstream error at all and keeps
//! `InvalidRequest`, the code for "invalid input from the user has generated a generic
//! exception", stated in a sentence naming what is at fault.
//!
//! The rows differ along what the rule generalises over: which owning service the refused call
//! would have been made on (`AWS::SSM::Parameter`, `AWS::Logs::LogGroup`), which resource
//! operation the request is (create, delete), which of IAM's rules withheld the grant (an
//! explicit `Deny`, no statement at all) — which the answer does not depend on — and, against
//! the denial, the other two classifications of the same type's create: a rule the owning
//! service holds the model to, and a fault in the model the handler itself judges. The control
//! is the identical denied request once the role IS granted the call.
//!
//! `denied_call_classification` holds the same answer through the permission table;
//! this suite holds the CLASSIFICATION itself — that a denied call is reported exactly as the
//! owning service's other errors are, while a model fault is not.

use serde_json::{json, Value};

use service_com_amazonaws_cloudcontrol_cloudapiservice::ops::types::HANDLER_ERROR_CODE_VALUES;

use super::denied_call_classification::{downstream_sentence, DENIED_CALL_CODE};
use super::emulator::{text, Emulator, STATUS_FAILED, STATUS_SUCCESS};
use super::role_arn_permissions::{
    allow_but_deny, bare_role, create_request, iam_ok, one_statement, role_with, terminal_event,
};

/// The type whose resources live in the AWS Systems Manager service's own store.
const PARAMETER: &str = "AWS::SSM::Parameter";

/// A type whose resources live in the Amazon CloudWatch Logs service's own store, so the rule
/// is held on a SECOND owning service.
const LOG_GROUP: &str = "AWS::Logs::LogGroup";

/// The handler code for a fault in the resource model against what the type's schema
/// publishes, which is NOT a downstream error.
const MODEL_FAULT: &str = "InvalidRequest";

/// The language version every policy document below declares.
const POLICY_VERSION: &str = "2012-10-17";

/// Hold a reported handler error code to the model's own `HandlerErrorCode` enum, so no case
/// can pass on a code a client cannot read.
fn assert_modelled(case: &str, code: &str) {
    assert!(
        HANDLER_ERROR_CODE_VALUES.contains(&code),
        "case {case:?}: {code:?} is not a value the model's HandlerErrorCode enum declares"
    );
}

/// Which of IAM's rules withholds the grant in a row — the INPUT the rule generalises over,
/// since the answer is the same whichever rule it was.
#[derive(Clone, Copy)]
enum Withheld {
    /// An explicit `Deny` of the call in the role's own document.
    ExplicitDeny,
    /// No document of the role states anything that allows the call — IAM's default deny.
    NoStatement,
}

/// One denial row: the call the handler would have been refused on, and how the grant was
/// withheld.
struct Denied {
    /// What the row is about, named in every assertion of it.
    case: &'static str,
    /// The `TypeName` the request addresses.
    type_name: &'static str,
    /// The resource operation the request is.
    operation: &'static str,
    /// The owning service's API action the handler would have been refused on.
    action: &'static str,
    /// Which of IAM's rules withheld the grant.
    withheld: Withheld,
}

/// A call the role may not make is reported as the owning service's own error: the downstream
/// catch-all code and the handler's sentence about the call — on either owning service, on
/// either resource operation, and whichever of IAM's rules withheld the grant.
#[tokio::test]
async fn a_denied_call_is_reported_as_a_downstream_error() {
    let rows = [
        Denied {
            case: "a parameter create no statement of the role allows",
            type_name: PARAMETER,
            operation: "CreateResource",
            action: "ssm:PutParameter",
            withheld: Withheld::NoStatement,
        },
        Denied {
            case: "a parameter delete the role is explicitly denied",
            type_name: PARAMETER,
            operation: "DeleteResource",
            action: "ssm:DeleteParameter",
            withheld: Withheld::ExplicitDeny,
        },
        Denied {
            case: "a log group create the role is explicitly denied",
            type_name: LOG_GROUP,
            operation: "CreateResource",
            action: "logs:CreateLogGroup",
            withheld: Withheld::ExplicitDeny,
        },
        Denied {
            case: "a log group delete no statement of the role allows",
            type_name: LOG_GROUP,
            operation: "DeleteResource",
            action: "logs:DeleteLogGroup",
            withheld: Withheld::NoStatement,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let on_logs = row.type_name == LOG_GROUP;
        let emulator = if on_logs {
            Emulator::with_cloudwatch_logs_and_iam()
        } else {
            Emulator::with_ssm_and_iam()
        };
        let name = format!("/downstream-denied-{index}");
        let name = if on_logs { name.replacen('/', "", 1) } else { name };
        let deletes = row.operation == "DeleteResource";
        let role_name = format!("downstream-denied-role-{index}");
        let role = match row.withheld {
            // An explicit `Deny` of the refused call, over a grant wide enough that only that
            // `Deny` can withhold it.
            Withheld::ExplicitDeny => {
                let service = row.action.split(':').next().unwrap_or_default();
                role_with(
                    &emulator,
                    &role_name,
                    &allow_but_deny(json!(format!("{service}:*")), json!(row.action)),
                )
                .await
            }
            // A role whose documents say nothing about the call at all.
            Withheld::NoStatement => bare_role(&emulator, &role_name).await,
        };

        // A delete needs the resource to be there first, written through the OWNING service's
        // own API so the row starts from a state Cloud Control API never wrote.
        if deletes {
            if on_logs {
                emulator
                    .logs_ok("CreateLogGroup", json!({ "logGroupName": &name }))
                    .await;
            } else {
                emulator
                    .ssm_ok(
                        "PutParameter",
                        json!({ "Name": &name, "Value": "before", "Type": "String" }),
                    )
                    .await;
            }
        }

        let request = if deletes {
            json!({
                "TypeName": row.type_name,
                "Identifier": &name,
                "RoleArn": &role,
            })
        } else if on_logs {
            create_request(row.type_name, &json!({ "LogGroupName": &name }), Some(&role))
        } else {
            create_request(
                row.type_name,
                &json!({ "Name": &name, "Type": "String", "Value": "asked-for" }),
                Some(&role),
            )
        };

        let event = terminal_event(&emulator, case, row.operation, request).await;
        let reported_status = text(&event, "OperationStatus");
        assert_eq!(
            reported_status, STATUS_FAILED,
            "case {case:?}: a call the role may not make must fail the request, got \
             {reported_status:?}"
        );
        let code = text(&event, "ErrorCode");
        assert_modelled(case, &code);
        assert_eq!(
            code, DENIED_CALL_CODE,
            "case {case:?}: expected the {DENIED_CALL_CODE} handler error code, got {code:?} \
             with {event:?}"
        );
        let expected = downstream_sentence(row.action);
        let reported = text(&event, "StatusMessage");
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the status message {expected:?}, got {reported:?}"
        );
    }
}

/// The three classifications of one type's create, side by side: a call the role may not make
/// and a rule the owning service holds the model to are the SAME answer — both are that
/// service's error on the call the handler made — while a fault in the model keeps the code for
/// invalid input and a sentence of its own.
#[tokio::test]
async fn a_denied_call_and_an_owning_rule_are_one_answer_and_a_model_fault_is_not() {
    let emulator = Emulator::with_ssm_and_iam();
    let role = bare_role(&emulator, "classification-role").await;

    // (case, the desired state, the role the request names, the code expected)
    let rows: [(&str, Value, Option<&str>, &str); 3] = [
        (
            "a create the role carries no grant for",
            json!({ "Name": "/classified/denied", "Type": "String", "Value": "v" }),
            Some(role.as_str()),
            DENIED_CALL_CODE,
        ),
        (
            "a name in the hierarchy the owning service keeps for itself",
            json!({ "Name": "/aws/classified", "Type": "String", "Value": "v" }),
            None,
            DENIED_CALL_CODE,
        ),
        (
            "a property the type's schema declares that this handler does not apply",
            json!({
                "Name": "/classified/policed",
                "Type": "String",
                "Value": "v",
                "Policies": "[{\"Type\":\"Expiration\",\"Version\":\"1.0\"}]",
            }),
            None,
            MODEL_FAULT,
        ),
    ];

    let mut sentences: Vec<String> = Vec::new();
    for (case, desired, role_arn, expected_code) in rows {
        let event = terminal_event(
            &emulator,
            case,
            "CreateResource",
            create_request(PARAMETER, &desired, role_arn),
        )
        .await;
        let reported_status = text(&event, "OperationStatus");
        assert_eq!(
            reported_status, STATUS_FAILED,
            "case {case:?}: the create must fail, got {reported_status:?} with {event:?}"
        );
        let code = text(&event, "ErrorCode");
        assert_modelled(case, &code);
        assert_eq!(
            code, expected_code,
            "case {case:?}: expected the {expected_code} handler error code, got {code:?} with \
             {event:?}"
        );
        let sentence = text(&event, "StatusMessage");
        assert!(
            !sentence.is_empty(),
            "case {case:?}: a failed request must state a diagnostic, got {event:?}"
        );
        sentences.push(sentence);
    }

    // The two failures the owning service answered are ONE answer: the same code and the same
    // sentence about the call the handler made, which is the sentence that call's failures
    // carry whatever that service's reason was.
    assert_eq!(
        sentences[0],
        downstream_sentence("ssm:PutParameter"),
        "the denied call must report the handler's sentence about the call it was making"
    );
    assert_eq!(
        sentences[0], sentences[1],
        "a denied call and a rule the owning service holds the model to are one answer, got \
         {sentences:?}"
    );
    assert_ne!(
        sentences[0], sentences[2],
        "a fault in the model must state what is at fault instead, got {sentences:?}"
    );
}

/// The control the classification is only meaningful against: the identical request, once the
/// role is granted the call, is carried out.
#[tokio::test]
async fn the_same_request_succeeds_once_the_role_is_granted_the_call() {
    let emulator = Emulator::with_ssm_and_iam();
    let role_name = "granted-parameter-role";
    let role = role_with(
        &emulator,
        role_name,
        &one_statement("Deny", json!("ssm:PutParameter"), json!("*")),
    )
    .await;
    let name = "/classified/granted";
    let request = create_request(
        PARAMETER,
        &json!({ "Name": name, "Type": "String", "Value": "asked-for" }),
        Some(&role),
    );

    let case = "the create while the role is denied the call";
    let denied = terminal_event(&emulator, case, "CreateResource", request.clone()).await;
    let code = text(&denied, "ErrorCode");
    assert_eq!(
        code, DENIED_CALL_CODE,
        "case {case:?}: expected the {DENIED_CALL_CODE} handler error code, got {code:?} with \
         {denied:?}"
    );
    let (status, _) = emulator.ssm("GetParameter", json!({ "Name": name })).await;
    assert_ne!(
        status, 200,
        "case {case:?}: the refused create must write nothing to the owning service"
    );

    // The grant the role was missing, written through IAM's own API over the `Deny` above —
    // which the row replaces, so the same request is judged by a role that allows the call.
    iam_ok(
        &emulator,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", "resource-operations"),
            (
                "PolicyDocument",
                &json!({
                    "Version": POLICY_VERSION,
                    "Statement": [{ "Effect": "Allow", "Action": "ssm:*", "Resource": "*" }],
                })
                .to_string(),
            ),
        ],
    )
    .await;

    let case = "the identical create after the grant";
    let granted = terminal_event(&emulator, case, "CreateResource", request).await;
    let reported_status = text(&granted, "OperationStatus");
    assert_eq!(
        reported_status, STATUS_SUCCESS,
        "case {case:?}: a role granted the call must carry it out, got {reported_status:?} with \
         {granted:?}"
    );
    let read_back = emulator
        .ssm_ok("GetParameter", json!({ "Name": name }))
        .await;
    let reported_value = text(&read_back["Parameter"], "Value");
    assert_eq!(
        reported_value, "asked-for",
        "case {case:?}: the owning service must hold the parameter the create made, got \
         {read_back:?}"
    );
}
