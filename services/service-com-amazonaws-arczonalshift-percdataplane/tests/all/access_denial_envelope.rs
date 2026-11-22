// SPDX-License-Identifier: CC0-1.0

//! The document an ACCESS DENIAL carries, over every operation of this service that can
//! be driven to one.
//!
//! RULE: ARC decides the caller's authorization on a call's target in the layer in front
//! of the operation and answers the denial from there, so a 403 `AccessDeniedException`
//! is serialized in THAT layer's envelope and not in the operation's: the class is named
//! only in `x-amzn-errortype` — the header restJson1 resolves an error shape from — and
//! the JSON document carries the denial text alone, under the member `Message`, with no
//! `__type` discriminator. A denial that states no reason therefore carries no member at
//! all, and its document is the empty JSON object.
//!
//! The rows below span the whole class rather than one recorded call: every operation
//! that declares `AccessDeniedException` and can be denied over the wire, each of the
//! five HTTP methods this service binds, and every document the rule produces — the one
//! that states the non-disclosing reason about a RESOURCE a traffic-shift mutation named
//! (`crate::state::target_unavailable`), the one that names its subject for a call on a
//! resource's practice run configuration (`crate::state::not_a_managed_resource`), and the
//! empty one for a denial taken on an identifier no target was resolved from
//! (`crate::state::identifier_not_attributable`). Which of them a given call gets is
//! `access_denied_precedence.rs`'s, `refusal_disclosure.rs`'s and
//! `unmanaged_configuration_target.rs`'s subject; here it is one row's expected document.
//!
//! The BOUNDARY is asserted too: an error the OPERATION answers — a modelled refusal and
//! a framework-level one — keeps the protocol's own envelope, `__type` and the modelled
//! lowercase `message` included, so the substitution is this one error class and not the
//! service's error serialization as a whole.

use serde_json::{json, Value};

use super::unmanaged_configuration_target::not_a_managed_resource_message;
use super::zonal_shift_lifecycle::{emulator, encoded, error_code, refused, seeded, start_body};
use service_com_amazonaws_arczonalshift_percdataplane::state::TARGET_UNAVAILABLE_MESSAGE;

/// An ARN-shaped `resourceIdentifier` ARC holds no managed resource for: a resource an
/// IAM policy could have granted an ARC action on, which is simply not registered.
const UNREGISTERED_ARN: &str = "arn:aws:eks:us-east-1:111111111111:cluster/absent";

/// An identifier that is not shaped like a resource ARN at all (`ResourceArn`'s
/// `@pattern`, `^arn:.*$`), inside `ResourceIdentifier`'s `@length` 8..1024.
const NON_ARN_IDENTIFIER: &str = "not-an-arn-at-all";

/// A `zonalShiftId` inside `ZonalShiftId`'s `@length` 6..36 and `@pattern`
/// `^[A-Za-z0-9-]+$` that names no stored shift.
const UNKNOWN_SHIFT_ID: &str = "zs-0000009999";

/// An alarm ARN inside `MetricIdentifier`'s `@length` 8..1024, so a practice-run
/// configuration row carries a well-formed outcome alarm.
const ALARM_ARN: &str = "arn:aws:cloudwatch:us-east-1:111111111111:alarm:outcome";

/// A `CreatePracticeRunConfiguration` body for `resource`: the two members the model
/// makes `@required`, with one outcome alarm (the list's `@length` is 1..10).
fn practice_run_configuration(resource: &str) -> String {
    json!({
        "resourceIdentifier": resource,
        "outcomeAlarms": [{ "type": "CLOUDWATCH", "alarmIdentifier": ALARM_ARN }],
    })
    .to_string()
}

/// One denial under test: how it is driven, and what ARC states in it.
struct Row {
    /// The operation and the kind of target that is denied.
    case: &'static str,
    method: &'static str,
    uri: String,
    body: String,
    /// The text the denial states, or `None` for a denial that states no reason.
    reason: Option<String>,
}

/// Every operation of this service that can be driven to a denial over the wire, with the
/// document ARC answers it with. The routes are the model's
/// (`smithy_query(query="digest")`, reproduced in the crate's `OPERATIONS.md`).
fn rows() -> Vec<Row> {
    let configuration_uri = format!("/configuration/{}", encoded(UNREGISTERED_ARN));
    vec![
        Row {
            case: "StartZonalShift on a resource ARC holds nothing for",
            method: "POST",
            uri: "/zonalshifts".to_string(),
            body: start_body(UNREGISTERED_ARN, "use1-az1", "1h", "a denied shift"),
            reason: Some(TARGET_UNAVAILABLE_MESSAGE.to_string()),
        },
        Row {
            case: "CancelZonalShift on an id ARC holds no shift for",
            method: "DELETE",
            uri: format!("/zonalshifts/{UNKNOWN_SHIFT_ID}"),
            body: String::new(),
            reason: None,
        },
        Row {
            case: "UpdateZonalShift on an id ARC holds no shift for",
            method: "PATCH",
            uri: format!("/zonalshifts/{UNKNOWN_SHIFT_ID}"),
            body: r#"{"comment":"a new comment"}"#.to_string(),
            reason: None,
        },
        Row {
            case: "CancelPracticeRun on an id ARC holds no shift for",
            method: "DELETE",
            uri: format!("/practiceruns/{UNKNOWN_SHIFT_ID}"),
            body: String::new(),
            reason: None,
        },
        Row {
            case: "GetManagedResource on an identifier that is not a resource ARN",
            method: "GET",
            uri: format!("/managedresources/{}", encoded(NON_ARN_IDENTIFIER)),
            body: String::new(),
            reason: None,
        },
        Row {
            case: "StartPracticeRun on a resource ARC holds nothing for",
            method: "POST",
            uri: "/practiceruns".to_string(),
            body: json!({
                "resourceIdentifier": UNREGISTERED_ARN,
                "awayFrom": "use1-az1",
                "comment": "a denied practice run",
            })
            .to_string(),
            reason: Some(TARGET_UNAVAILABLE_MESSAGE.to_string()),
        },
        Row {
            case: "CreatePracticeRunConfiguration for a resource ARC holds nothing for",
            method: "POST",
            uri: "/configuration".to_string(),
            body: practice_run_configuration(UNREGISTERED_ARN),
            reason: Some(not_a_managed_resource_message(UNREGISTERED_ARN)),
        },
        Row {
            case: "UpdatePracticeRunConfiguration for a resource ARC holds nothing for",
            method: "PATCH",
            uri: configuration_uri.clone(),
            body: json!({ "blockedDates": ["2026-01-01"] }).to_string(),
            reason: Some(not_a_managed_resource_message(UNREGISTERED_ARN)),
        },
        Row {
            case: "DeletePracticeRunConfiguration for a resource ARC holds nothing for",
            method: "DELETE",
            uri: configuration_uri,
            body: String::new(),
            reason: Some(not_a_managed_resource_message(UNREGISTERED_ARN)),
        },
        Row {
            case: "UpdateZonalAutoshiftConfiguration for a resource ARC holds nothing for",
            method: "PUT",
            uri: format!("/managedresources/{}", encoded(UNREGISTERED_ARN)),
            body: json!({ "zonalAutoshiftStatus": "ENABLED" }).to_string(),
            reason: Some(TARGET_UNAVAILABLE_MESSAGE.to_string()),
        },
    ]
}

/// The member names a JSON object carries, in order, for stating what a document holds.
fn members(document: &Value) -> Vec<&str> {
    document
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn every_denial_states_its_class_in_the_error_type_header_alone() {
    let (router, _engine) = emulator();
    for row in rows() {
        let (status, class, body) = refused(&router, row.method, &row.uri, &row.body).await;
        assert_eq!(
            status, 403,
            "case {:?}: a denial is answered 403, got {status} {body}",
            row.case
        );
        assert_eq!(
            class, "AccessDeniedException",
            "case {:?}: expected the class in x-amzn-errortype, got {class:?} with {body}",
            row.case
        );
        assert!(
            body["__type"].is_null(),
            "case {:?}: the denial document carries no class discriminator, got {body}",
            row.case
        );
    }
}

#[tokio::test]
async fn every_denial_states_its_text_under_the_member_the_denial_envelope_names() {
    let (router, _engine) = emulator();
    for row in rows() {
        let (_status, _class, body) = refused(&router, row.method, &row.uri, &row.body).await;
        assert!(
            body.is_object(),
            "case {:?}: a denial answers a JSON document, got {body}",
            row.case
        );
        match &row.reason {
            Some(expected) => {
                assert_eq!(
                    body["Message"].as_str(),
                    Some(expected.as_str()),
                    "case {:?}: expected the denial text at Message, got {body}",
                    row.case
                );
                assert_eq!(
                    members(&body),
                    vec!["Message"],
                    "case {:?}: the denial document carries the text and nothing else, \
                     got {body}",
                    row.case
                );
            }
            None => assert_eq!(
                members(&body),
                Vec::<&str>::new(),
                "case {:?}: a denial that states no reason carries an empty document, \
                 got {body}",
                row.case
            ),
        }
    }
}

#[tokio::test]
async fn an_error_the_operation_answers_keeps_the_protocols_own_envelope() {
    // The boundary of the rule: only the denial is answered from the authorization layer.
    // Every other error is the operation's, and keeps the discriminator and the modelled
    // lowercase `message` — a framework-level refusal (a value outside a modelled
    // constraint) and a modelled one the handler returns.
    let (router, engine) = emulator();
    let (resource, zones) = seeded(&engine)[0].clone();

    // A `zonalShiftId` outside `ZonalShiftId`'s `@pattern`, refused by the constraint walk
    // before the handler runs.
    let (status, class, body) = refused(&router, "DELETE", "/zonalshifts/bad_id_1", "").await;
    assert_eq!(
        status, 400,
        "a value outside a modelled constraint is refused 400, got {status} {body}"
    );
    assert_eq!(
        (class.as_str(), error_code(&body).as_str()),
        ("ValidationException", "ValidationException"),
        "the refusal names its class in both the header and the document, got {body}"
    );
    assert!(
        body["message"].as_str().is_some_and(|m| !m.is_empty()),
        "the refusal states its message under the modelled member, got {body}"
    );

    // A modelled refusal the handler returns: a second customer-initiated shift for a
    // resource that already has one.
    let (status, started) = super::zonal_shift_lifecycle::call(
        &router,
        "POST",
        "/zonalshifts",
        &start_body(&resource, &zones[0], "1h", "the first shift"),
    )
    .await;
    assert_eq!(status, 201, "the first shift starts, got {status} {started}");
    let (status, class, body) = refused(
        &router,
        "POST",
        "/zonalshifts",
        &start_body(&resource, &zones[0], "1h", "the second shift"),
    )
    .await;
    assert_eq!(
        status, 409,
        "a second shift for the same resource conflicts, got {status} {body}"
    );
    assert_eq!(
        (class.as_str(), error_code(&body).as_str()),
        ("ConflictException", "ConflictException"),
        "the conflict names its class in both the header and the document, got {body}"
    );
    assert!(
        body["message"].as_str().is_some_and(|m| !m.is_empty()),
        "the conflict states its message under the modelled member, got {body}"
    );
}

#[tokio::test]
async fn an_authorized_call_is_served_rather_than_denied() {
    // The denial is computed from the store, not answered unconditionally: the same
    // operations on a target the store DOES hold are served, and their answers carry no
    // denial document at all.
    let (router, engine) = emulator();
    let (resource, zones) = seeded(&engine)[0].clone();
    let (status, class, started) = refused(
        &router,
        "POST",
        "/zonalshifts",
        &start_body(&resource, &zones[0], "1h", "an authorized shift"),
    )
    .await;
    assert_eq!(
        (status, class.as_str()),
        (201, ""),
        "an authorized start is served and names no error class, got {started}"
    );
    assert!(
        started["Message"].is_null(),
        "a served answer carries no denial text, got {started}"
    );

    let id = started["zonalShiftId"]
        .as_str()
        .expect("zonalShiftId is required")
        .to_string();
    let (status, class, read) = refused(
        &router,
        "GET",
        &format!("/managedresources/{}", encoded(&resource)),
        "",
    )
    .await;
    assert_eq!(
        (status, class.as_str()),
        (200, ""),
        "the resource the shift names reads back, got {read}"
    );
    assert!(
        read["zonalShifts"]
            .as_array()
            .is_some_and(|shifts| shifts
                .iter()
                .any(|shift| shift["zonalShiftId"].as_str() == Some(id.as_str()))),
        "the started shift {id} is reported for the resource, got {read}"
    );
}
