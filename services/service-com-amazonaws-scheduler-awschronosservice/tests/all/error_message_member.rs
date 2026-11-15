// SPDX-License-Identifier: CC0-1.0

//! WHICH member of an error body this service reports a failure's text in, asserted on the
//! wire.
//!
//! The rule: on this `restJson1` service an error whose class the model declares carries its
//! text under the member the model declares for it — `Message` on every one of this service's
//! error shapes, none of which renames it with a `@jsonName` — whichever layer rendered the
//! body. A refusal the FRAMEWORK renders before any handler runs is built from a synthetic
//! shape whose member is spelled `message`, so without the service's own member rule a caller
//! reads an error's text out of one layer's answers and finds nothing in the other's: the
//! required `Message` member is absent from the body, and every SDK surfaces the exception
//! with no text at all.
//!
//! The rows below therefore span BOTH layers and every constraint class this surface can
//! refuse — `@length`, `@pattern`, `@range` and an absent `@required` member — bound in a path
//! label, in a query parameter, in a body member and in a nested body member, across six
//! operations and four HTTP methods; and, on the other side, the modelled errors a HANDLER
//! serializes (404 `ResourceNotFoundException`, 400 `ValidationException`), which already
//! carry the modelled member and must be answered unchanged. One further test holds the
//! boundary: a failure whose class this model does NOT declare keeps the member the framework
//! wrote its text in.
//!
//! Every request goes through the shared [`crate::wire_harness`], the real restJson1 front
//! door, so the URI/query binding, the boundary validator, the handler and the error
//! serialization are all exercised.

use serde_json::{json, Value};

use crate::wire_harness::{account_id, call, encode_label, error_code, scheduler_router, REGION};

/// The modelled bound this suite's `@length` rows are refused by: `Name` and
/// `ScheduleGroupName` are both `@length` 1..64
/// (`smithy_query(query="operation",operation="GetSchedule")`). A fixed number from the model,
/// not from any request.
const NAME_MAX_LENGTH: usize = 64;

/// The member a restJson1 error of this service carries its text in, as the model declares it.
const MODELLED_MEMBER: &str = "Message";

/// The member the framework's own refusal renderer writes that text in.
const RENDERED_MEMBER: &str = "message";

/// A `CreateSchedule`/`UpdateSchedule` body carrying the members the model requires and
/// nothing else, with `members` merged over it — so a row varies exactly one thing and only
/// the violation it is about fires.
fn schedule_body(members: Value) -> Value {
    let mut body = json!({
        "ScheduleExpression": "rate(1 day)",
        "FlexibleTimeWindow": { "Mode": "OFF" },
        "Target": {
            "Arn": format!("arn:aws:sqs:{REGION}:{}:a-queue", account_id()),
            "RoleArn": format!("arn:aws:iam::{}:role/scheduler-role", account_id()),
        }
    });
    for (key, value) in members.as_object().expect("the members are an object") {
        body[key.as_str()] = value.clone();
    }
    body
}

/// The AWS envelope over one constraint clause.
fn one_violation(value_clause: &str, path: &str, requirement: &str) -> String {
    format!(
        "1 validation error detected: {value_clause} at '{path}' failed to satisfy \
         constraint: {requirement}"
    )
}

/// One request this service answers with an error, and the answer it must give.
struct ErrorCase {
    case: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    status: u16,
    /// The `__type` the body names.
    code: &'static str,
    /// The text the body must carry under [`MODELLED_MEMBER`].
    message: String,
}

/// Every error class this surface answers, over both of the layers that render one.
fn error_cases() -> Vec<ErrorCase> {
    // The value a `@length` row submits: one character past the modelled maximum, built from
    // the allowed character class so the length is the only thing it breaks.
    let over_length = "a".repeat(NAME_MAX_LENGTH + 1);
    let length_requirement =
        format!("Member must have length less than or equal to {NAME_MAX_LENGTH}");
    // The expression the name members publish, without the anchors a message never carries.
    let name_pattern = "Member must satisfy regular expression pattern: [0-9a-zA-Z-_.]+";
    let schedule_arn = format!(
        "arn:aws:scheduler:{REGION}:{}:schedule/default/a-schedule",
        account_id()
    );

    vec![
        // --- the framework's own refusal: @length, in a path label, on three operations ---
        ErrorCase {
            case: "GetSchedule Name past the modelled maximum (a path label on a GET)",
            method: "GET",
            uri: format!("/schedules/{over_length}"),
            body: None,
            status: 400,
            code: "ValidationException",
            message: one_violation(
                &format!("Value '{over_length}'"),
                "name",
                &length_requirement,
            ),
        },
        ErrorCase {
            case: "DeleteSchedule Name past the modelled maximum (a DELETE)",
            method: "DELETE",
            uri: format!("/schedules/{over_length}"),
            body: None,
            status: 400,
            code: "ValidationException",
            message: one_violation(
                &format!("Value '{over_length}'"),
                "name",
                &length_requirement,
            ),
        },
        ErrorCase {
            case: "CreateScheduleGroup Name past the modelled maximum (a POST)",
            method: "POST",
            uri: format!("/schedule-groups/{over_length}"),
            body: None,
            status: 400,
            code: "ValidationException",
            message: one_violation(
                &format!("Value '{over_length}'"),
                "name",
                &length_requirement,
            ),
        },
        // --- @length, bound in a QUERY parameter rather than in the path ---
        ErrorCase {
            case: "GetSchedule GroupName past the modelled maximum (a query parameter)",
            method: "GET",
            uri: format!("/schedules/a-schedule?groupName={over_length}"),
            body: None,
            status: 400,
            code: "ValidationException",
            message: one_violation(
                &format!("Value '{over_length}'"),
                "groupName",
                &length_requirement,
            ),
        },
        // --- @pattern, in a path label, on a PUT and on an ARN member ---
        ErrorCase {
            case: "UpdateSchedule Name outside the modelled pattern (a PUT)",
            method: "PUT",
            uri: "/schedules/not!a!name".to_string(),
            body: Some(schedule_body(json!({}))),
            status: 400,
            code: "ValidationException",
            message: one_violation("Value 'not!a!name'", "name", name_pattern),
        },
        ErrorCase {
            case: "TagResource ResourceArn outside the modelled pattern (a schedule ARN)",
            method: "POST",
            uri: format!("/tags/{}", encode_label(&schedule_arn)),
            body: Some(json!({ "Tags": [{ "Key": "team", "Value": "sre" }] })),
            status: 400,
            code: "ValidationException",
            message: one_violation(
                &format!("Value '{schedule_arn}'"),
                "resourceArn",
                "Member must satisfy regular expression pattern: \
                 arn:aws[a-z-]*:scheduler:[a-z0-9\\-]+:\\d{12}:schedule-group\\/[0-9a-zA-Z-_.]+",
            ),
        },
        // --- @range, in a query parameter and in a NESTED body member ---
        ErrorCase {
            case: "ListSchedules MaxResults above the modelled range (a query parameter)",
            method: "GET",
            uri: "/schedules?MaxResults=101".to_string(),
            body: None,
            status: 400,
            code: "ValidationException",
            message: one_violation(
                "Value '101'",
                "maxResults",
                "Member must have value less than or equal to 100",
            ),
        },
        ErrorCase {
            case: "CreateSchedule FlexibleTimeWindow.MaximumWindowInMinutes above its range",
            method: "POST",
            uri: "/schedules/a-schedule".to_string(),
            body: Some(schedule_body(json!({
                "FlexibleTimeWindow": { "Mode": "FLEXIBLE", "MaximumWindowInMinutes": 1441 }
            }))),
            status: 400,
            code: "ValidationException",
            message: one_violation(
                "Value '1441'",
                "flexibleTimeWindow.maximumWindowInMinutes",
                "Member must have value less than or equal to 1440",
            ),
        },
        // --- an absent @required member, which is found while READING the request ---
        ErrorCase {
            case: "CreateSchedule with no Target at all",
            method: "POST",
            uri: "/schedules/a-schedule".to_string(),
            body: Some(json!({
                "ScheduleExpression": "rate(1 day)",
                "FlexibleTimeWindow": { "Mode": "OFF" }
            })),
            status: 400,
            code: "ValidationException",
            message: one_violation("Value null", "target", "Member must not be null"),
        },
        // --- the other layer: the modelled errors a HANDLER serializes ---
        ErrorCase {
            case: "GetSchedule naming a schedule that does not exist (a handler's 404)",
            method: "GET",
            uri: "/schedules/absent".to_string(),
            body: None,
            status: 404,
            code: "ResourceNotFoundException",
            message: "Schedule absent does not exist.".to_string(),
        },
        ErrorCase {
            case: "GetSchedule naming a group that does not exist (a handler's 404)",
            method: "GET",
            uri: "/schedules/a-schedule?groupName=absent-group".to_string(),
            body: None,
            status: 404,
            code: "ResourceNotFoundException",
            message: "Schedule group absent-group does not exist.".to_string(),
        },
        ErrorCase {
            case: "DeleteScheduleGroup on the reserved group (a handler's 400)",
            method: "DELETE",
            uri: "/schedule-groups/default".to_string(),
            body: None,
            status: 400,
            code: "ValidationException",
            message: "This operation cannot be performed on schedule group default.".to_string(),
        },
    ]
}

#[tokio::test]
async fn every_modelled_error_reports_its_text_in_the_modelled_member() {
    let router = scheduler_router();

    for case in error_cases() {
        let (status, body) = call(&router, case.method, &case.uri, case.body.clone()).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (case.status, case.code),
            "case {:?}: expected {} {}, got {status} {body}",
            case.case,
            case.status,
            case.code
        );
        assert_eq!(
            body.get(MODELLED_MEMBER).and_then(Value::as_str),
            Some(case.message.as_str()),
            "case {:?}: expected {:?} under {MODELLED_MEMBER}, got {body}",
            case.case,
            case.message
        );
        assert!(
            body.get(RENDERED_MEMBER).is_none(),
            "case {:?}: the text must travel under ONE member, got {body}",
            case.case
        );
    }
}

/// The recorded divergence's own class, over a SET of values: a `Name` longer than the
/// modelled maximum is refused with the value as SUBMITTED quoted back, while the member path
/// (`name`) and the bound (64) are fixed text taken from the model.
#[tokio::test]
async fn an_over_length_name_quotes_the_submitted_value_against_the_modelled_bound() {
    let router = scheduler_router();
    // Lengths either side of the modelled maximum, plus three well past it: the bound and the
    // path never move, only the quoted value does.
    let lengths = [
        NAME_MAX_LENGTH,
        NAME_MAX_LENGTH + 1,
        NAME_MAX_LENGTH + 2,
        128,
        512,
    ];

    for length in lengths {
        let name = "n".repeat(length);
        let (status, body) = call(&router, "GET", &format!("/schedules/{name}"), None).await;
        if length <= NAME_MAX_LENGTH {
            // Inside the bound the request is not refused at all: it reaches the handler and
            // is answered for the resource it names.
            assert_eq!(
                (status, error_code(&body).as_str()),
                (404, "ResourceNotFoundException"),
                "case length {length}: a name within the bound must reach the handler, \
                 got {status} {body}"
            );
            continue;
        }
        let expected = one_violation(
            &format!("Value '{name}'"),
            "name",
            &format!("Member must have length less than or equal to {NAME_MAX_LENGTH}"),
        );
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case length {length}: expected 400 ValidationException, got {status} {body}"
        );
        assert_eq!(
            body.get(MODELLED_MEMBER).and_then(Value::as_str),
            Some(expected.as_str()),
            "case length {length}: expected {expected:?} under {MODELLED_MEMBER}, got {body}"
        );
    }
}

/// The boundary of the rule: it is keyed on the error shapes this model DECLARES, so a failure
/// whose class it does not declare keeps the member the framework wrote its text in — the
/// request that names no modelled operation at all.
#[tokio::test]
async fn a_class_this_model_does_not_declare_keeps_the_frameworks_member() {
    let router = scheduler_router();
    let (status, body) = call(&router, "GET", "/not-a-modelled-route", None).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (501, "NotImplemented"),
        "expected the framework's own answer for an unroutable request, got {status} {body}"
    );
    assert!(
        body.get(RENDERED_MEMBER)
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty()),
        "expected the text under {RENDERED_MEMBER}, got {body}"
    );
    assert!(
        body.get(MODELLED_MEMBER).is_none(),
        "a class this model does not declare must gain no modelled member, got {body}"
    );
}
