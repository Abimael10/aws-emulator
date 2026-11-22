// SPDX-License-Identifier: CC0-1.0

//! The body of the modelled `ValidationException` wherever this service refuses a
//! request because of the request's OWN parameters.
//!
//! RULE under test: such a refusal's `Message` is the fixed opening "The following
//! parameter(s) are not valid: " followed by one clause per fault, comma-separated —
//! `"<parameter path>" is required` for a parameter the request has to carry and
//! omits, and the fixed clause "Unsupported request parameter(s) were found." when the
//! request carries a parameter it may not carry here (which names none of them). The
//! only variable part is the quoted parameter path, which is the path from the
//! operation's own input, and nothing else of the request — no value, no enum member,
//! no count — appears in the sentence.
//!
//! The rows span what the rule generalises over: every operation that cross-checks a
//! nested member set against the request's own discriminator (`CreateJob`'s
//! `Type`/`Details`, `SendDataSetNotification`'s `Type`/`Details`, an event action's
//! `Event` and `Action`), every value of the model's `Type` enum, and each
//! combination of faults a request can carry — a required parameter missing, an
//! unsupported one present, and both at once. Rows of the other clause kinds — a value
//! outside a parameter's modelled value set, a parameter the model constrains to an AWS
//! account id carrying something else, and an end a data grant may not name — show the
//! opening is the same sentence for every parameter fault, and that none of them is
//! delegated to the framework's own generic constraint prose.
//!
//! Every test drives the service through `MultiServiceRouter::dispatch` with a real
//! restJson1 request, so the route, the status, the error code and the serialized body
//! are all exercised.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::data_grants::{
    create_data_grant, get_data_grant, instant_from_now, midnight_in_days, DAY_SECONDS,
};
use crate::data_sets::{call, create_data_set, harness, seconds};
use crate::event_actions::entitle_data_set;
use crate::jobs::{create_job, data_set_with_revision, job_cases, LAKE_FORMATION_TYPE};

/// What AWS opens a refusal about the request's own parameters with. Stated here
/// independently of the constant the crate composes it from, so a change to that
/// constant is a failure rather than a tautology.
const PREFIX: &str = "The following parameter(s) are not valid: ";

/// The whole clause AWS states a request's unsupported parameters with: it names none
/// of them.
const UNSUPPORTED: &str = "Unsupported request parameter(s) were found.";

/// The clause for a parameter the request has to carry and does not.
fn required(path: &str) -> String {
    format!("{path:?} is required")
}

/// The whole `Message` the rule requires for a request whose faults are `clauses`.
fn expected_message(clauses: &[String]) -> String {
    format!("{PREFIX}{}", clauses.join(", "))
}

/// One request at fault in its own parameters, and the `Message` its refusal must
/// carry.
struct Case {
    label: String,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    expected: String,
}

/// An instant for the `date-time` member a notification detail carries.
const AN_INSTANT: &str = "2024-01-01T00:00:00Z";

/// A well-formed `Action` for an event action, so a row about `Event` is refused for
/// its event rather than for its action.
fn an_action() -> Value {
    json!({ "ExportRevisionToS3": { "RevisionDestination": { "Bucket": "export-bucket" } } })
}

/// Assert one row: the status, the error code and the whole `Message`.
async fn assert_refused(router: &MultiServiceRouter, case: &Case) {
    let label = &case.label;
    let answer = call(router, case.method, &case.uri, case.body.clone()).await;
    assert_eq!(
        answer.status, 400,
        "case {label}: expected 400, got {} body {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error.as_deref(),
        Some("ValidationException"),
        "case {label}: expected ValidationException, got {:?}",
        answer.error
    );
    let message = answer.string("Message");
    assert_eq!(
        message, case.expected,
        "case {label}: expected {:?}, got {message:?}",
        case.expected
    );
}

/// Every way a `CreateJob` request's `Details` can disagree with its `Type`, for every
/// `Type` the model admits.
fn create_job_cases(data_set_id: &str, revision_id: &str) -> Vec<Case> {
    let cases = job_cases(data_set_id, revision_id);
    let mut rows: Vec<Case> = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        // The NEXT row's member, so every member of the catalog stands in as the
        // unsupported one for some type.
        let other = &cases[(index + 1) % cases.len()];
        let path = format!("Details.{}", case.member);
        rows.push(Case {
            label: format!("CreateJob {} with no Details member", case.job_type),
            method: "POST",
            uri: "/v1/jobs".to_string(),
            body: Some(json!({ "Type": case.job_type, "Details": {} })),
            expected: expected_message(&[required(&path)]),
        });
        rows.push(Case {
            label: format!(
                "CreateJob {} carrying {} instead",
                case.job_type, other.member
            ),
            method: "POST",
            uri: "/v1/jobs".to_string(),
            body: Some(json!({
                "Type": case.job_type,
                "Details": { other.member: other.detail.clone() },
            })),
            expected: expected_message(&[required(&path), UNSUPPORTED.to_string()]),
        });
        rows.push(Case {
            label: format!(
                "CreateJob {} carrying its own member and {}",
                case.job_type, other.member
            ),
            method: "POST",
            uri: "/v1/jobs".to_string(),
            body: Some(json!({
                "Type": case.job_type,
                "Details": {
                    case.member: case.detail.clone(),
                    other.member: other.detail.clone(),
                },
            })),
            expected: expected_message(&[UNSUPPORTED.to_string()]),
        });
    }

    // The one `Type` whose own details no request can carry (its `CatalogId` pattern
    // admits no value) is still cross-checked against its `Type`: the disagreement is
    // settled before any of that member's own constraints matter.
    let lake_formation = "Details.ImportAssetsFromLakeFormationTagPolicy";
    rows.push(Case {
        label: format!("CreateJob {LAKE_FORMATION_TYPE} with no Details member"),
        method: "POST",
        uri: "/v1/jobs".to_string(),
        body: Some(json!({ "Type": LAKE_FORMATION_TYPE, "Details": {} })),
        expected: expected_message(&[required(lake_formation)]),
    });
    rows.push(Case {
        label: format!(
            "CreateJob {LAKE_FORMATION_TYPE} carrying {} instead",
            cases[0].member
        ),
        method: "POST",
        uri: "/v1/jobs".to_string(),
        body: Some(json!({
            "Type": LAKE_FORMATION_TYPE,
            "Details": { cases[0].member: cases[0].detail.clone() },
        })),
        expected: expected_message(&[required(lake_formation), UNSUPPORTED.to_string()]),
    });
    rows
}

/// Every way a `SendDataSetNotification` request's `Details` can disagree with its
/// `Type`: the detail member a `DEPRECATION` or `SCHEMA_CHANGE` notification must
/// carry is absent, a member another type names is present, or both.
fn notification_cases(data_set_id: &str) -> Vec<Case> {
    let uri = format!("/v1/data-sets/{data_set_id}/notification");
    let data_update = json!({ "DataUpdate": { "DataUpdatedAt": AN_INSTANT } });
    let deprecation = json!({ "Deprecation": { "DeprecationAt": AN_INSTANT } });
    let schema_change = json!({ "SchemaChange": { "SchemaChangeAt": AN_INSTANT } });
    vec![
        Case {
            label: "SendDataSetNotification DEPRECATION with no Details at all".to_string(),
            method: "POST",
            uri: uri.clone(),
            body: Some(json!({ "Type": "DEPRECATION" })),
            expected: expected_message(&[required("Details.Deprecation")]),
        },
        Case {
            label: "SendDataSetNotification SCHEMA_CHANGE with an empty Details".to_string(),
            method: "POST",
            uri: uri.clone(),
            body: Some(json!({ "Type": "SCHEMA_CHANGE", "Details": {} })),
            expected: expected_message(&[required("Details.SchemaChange")]),
        },
        Case {
            label: "SendDataSetNotification DEPRECATION carrying DataUpdate".to_string(),
            method: "POST",
            uri: uri.clone(),
            body: Some(json!({ "Type": "DEPRECATION", "Details": data_update.clone() })),
            expected: expected_message(&[
                required("Details.Deprecation"),
                UNSUPPORTED.to_string(),
            ]),
        },
        Case {
            label: "SendDataSetNotification SCHEMA_CHANGE carrying Deprecation".to_string(),
            method: "POST",
            uri: uri.clone(),
            body: Some(json!({ "Type": "SCHEMA_CHANGE", "Details": deprecation.clone() })),
            expected: expected_message(&[
                required("Details.SchemaChange"),
                UNSUPPORTED.to_string(),
            ]),
        },
        Case {
            label: "SendDataSetNotification DATA_UPDATE carrying Deprecation".to_string(),
            method: "POST",
            uri: uri.clone(),
            body: Some(json!({ "Type": "DATA_UPDATE", "Details": deprecation })),
            expected: expected_message(&[UNSUPPORTED.to_string()]),
        },
        Case {
            label: "SendDataSetNotification DATA_DELAY carrying SchemaChange".to_string(),
            method: "POST",
            uri: uri.clone(),
            body: Some(json!({ "Type": "DATA_DELAY", "Details": schema_change })),
            expected: expected_message(&[UNSUPPORTED.to_string()]),
        },
        Case {
            label: "SendDataSetNotification DATA_UPDATE carrying DataUpdate and Deprecation"
                .to_string(),
            method: "POST",
            uri,
            body: Some(json!({
                "Type": "DATA_UPDATE",
                "Details": {
                    "DataUpdate": { "DataUpdatedAt": AN_INSTANT },
                    "Deprecation": { "DeprecationAt": AN_INSTANT },
                },
            })),
            expected: expected_message(&[UNSUPPORTED.to_string()]),
        },
    ]
}

/// `CreateEventAction`'s own member sets: the `Event` that starts the action and the
/// `Action` it then performs each carry exactly one member the request must name.
fn event_action_cases(data_set_id: &str) -> Vec<Case> {
    let published = json!({ "RevisionPublished": { "DataSetId": data_set_id } });
    vec![
        Case {
            label: "CreateEventAction whose Event names no member".to_string(),
            method: "POST",
            uri: "/v1/event-actions".to_string(),
            body: Some(json!({ "Event": {}, "Action": an_action() })),
            expected: expected_message(&[required("Event.RevisionPublished")]),
        },
        Case {
            label: "CreateEventAction whose Action names no member".to_string(),
            method: "POST",
            uri: "/v1/event-actions".to_string(),
            body: Some(json!({ "Event": published, "Action": {} })),
            expected: expected_message(&[required("Action.ExportRevisionToS3")]),
        },
        Case {
            label: "CreateEventAction whose Event and Action both name no member".to_string(),
            method: "POST",
            uri: "/v1/event-actions".to_string(),
            body: Some(json!({ "Event": {}, "Action": {} })),
            // Two faults in one request, so one clause each: the rule states every
            // fault of the request, not the first one found.
            expected: expected_message(&[
                required("Event.RevisionPublished"),
                required("Action.ExportRevisionToS3"),
            ]),
        },
    ]
}

/// RULE: every refusal about the request's own parameters carries the fixed opening
/// and one clause per fault — on `CreateJob` for every `Type` the model admits, on
/// `SendDataSetNotification`, and on an event action's `Event` and `Action`.
#[tokio::test]
async fn a_parameter_refusal_states_each_fault_after_the_fixed_opening() {
    let (router, _engine) = harness();
    let (data_set_id, revision_id) = data_set_with_revision(&router, "parameter-faults").await;

    let mut rows = create_job_cases(&data_set_id, &revision_id);
    rows.extend(notification_cases(&data_set_id));
    rows.extend(event_action_cases(&data_set_id));

    for case in &rows {
        assert_refused(&router, case).await;
    }

    // Each combination of faults the rule covers is actually exercised above: a
    // required parameter alone, an unsupported one alone, and both in one sentence.
    for (shape, carries_required, carries_unsupported) in [
        ("a required parameter alone", true, false),
        ("an unsupported parameter alone", false, true),
        ("a required parameter and an unsupported one", true, true),
    ] {
        let seen = rows
            .iter()
            .filter(|case| {
                case.expected.contains(" is required") == carries_required
                    && case.expected.contains(UNSUPPORTED) == carries_unsupported
            })
            .count();
        assert!(
            seen > 0,
            "case coverage of {shape}: expected at least one row among {}, got {seen}",
            rows.len()
        );
    }
}

/// RULE: the same opening and the same clause state a fault of the request body of an
/// UPDATE, so the sentence is the operation's parameters' and not the create's.
#[tokio::test]
async fn an_update_states_its_own_parameter_fault_the_same_way() {
    let (router, engine) = harness();
    let data_set_id = create_data_set(&router, "updatable-action", "for an update", "S3_SNAPSHOT", None)
        .await
        .string("Id");
    entitle_data_set(&engine, &data_set_id);

    let created = call(
        &router,
        "POST",
        "/v1/event-actions",
        Some(json!({
            "Event": { "RevisionPublished": { "DataSetId": data_set_id } },
            "Action": an_action(),
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case setup: expected 201 from CreateEventAction, got {} body {:?}",
        created.status, created.body
    );
    let event_action_id = created.string("Id");

    assert_refused(
        &router,
        &Case {
            label: "UpdateEventAction whose Action names no member".to_string(),
            method: "PATCH",
            uri: format!("/v1/event-actions/{event_action_id}"),
            body: Some(json!({ "Action": {} })),
            expected: expected_message(&[required("Action.ExportRevisionToS3")]),
        },
    )
    .await;

    // The stored action is untouched by the refusal, so the sentence is about the
    // request rather than about the record.
    let read = call(
        &router,
        "GET",
        &format!("/v1/event-actions/{event_action_id}"),
        None,
    )
    .await;
    assert_eq!(
        read.member("Action"),
        &an_action(),
        "case the stored action after a refused update: expected it unchanged, got {:?}",
        read.member("Action")
    );
}

/// RULE: the opening is the same for a parameter fault of another kind — a value
/// outside the set the model admits for the parameter — so it is the sentence every
/// parameter refusal of this service opens with, not one operation's wording.
#[tokio::test]
async fn a_value_outside_a_parameters_modelled_set_opens_the_same_way() {
    let (router, _engine) = harness();

    // Source: the model's `Origin` enum, in the model's own order
    // (`smithy_query(query="shape", shape="Origin")`).
    let answer = call(&router, "GET", "/v1/data-sets?origin=NOT_AN_ORIGIN", None).await;
    assert_eq!(
        answer.status, 400,
        "case origin outside the modelled set: expected 400, got {} body {:?}",
        answer.status, answer.body
    );
    let message = answer.string("Message");
    assert_eq!(
        message,
        expected_message(&["\"origin\" must be one of [OWNED, ENTITLED].".to_string()]),
        "case origin outside the modelled set: expected the fixed opening and the value-set \
         clause, got {message:?}"
    );
}

/// RULE (negative control): a request whose parameters are exactly the admitted set is
/// not refused for its parameters — whatever else the service then says about it — so
/// the sentence above belongs to the fault and not to the operation.
#[tokio::test]
async fn a_request_whose_parameters_are_admitted_is_not_refused_for_them() {
    let (router, _engine) = harness();
    let (data_set_id, revision_id) = data_set_with_revision(&router, "admitted-parameters").await;

    for case in job_cases(&data_set_id, &revision_id) {
        let created = create_job(
            &router,
            case.job_type,
            json!({ case.member: case.detail.clone() }),
            None,
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {} with exactly its own Details member: expected 201, got {} body {:?}",
            case.job_type, created.status, created.body
        );
    }

    // A notification whose details ARE the ones its type names reaches the service's
    // next answer about it, which says nothing about the request's parameters.
    let notified = call(
        &router,
        "POST",
        &format!("/v1/data-sets/{data_set_id}/notification"),
        Some(json!({
            "Type": "DEPRECATION",
            "Details": { "Deprecation": { "DeprecationAt": AN_INSTANT } },
        })),
    )
    .await;
    let message = notified
        .member("Message")
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !message.starts_with(PREFIX),
        "case a notification carrying the details its type names: expected no parameter \
         refusal, got {message:?}"
    );
}

// ---------------------------------------------------------------------------
// `CreateDataGrant`'s own parameter faults: the parameter that names an AWS account,
// and the end the grant may name.
// ---------------------------------------------------------------------------

/// How many whole UTC days after the current date the earliest end a data grant may
/// name falls on. The model bounds `EndsAt` not at all
/// (`smithy_query(query="operation", operation="CreateDataGrant")` — an optional
/// `Timestamp`), so the bound is this service's own recorded behaviour.
const END_LEAD_DAYS: i64 = 2;

/// The clause stated for a parameter the model constrains to an AWS account id
/// (`smithy_query(query="shape", shape="ReceiverPrincipal")` -> `@pattern ^\d{12}$`)
/// that carries something else: the parameter named as the MODEL spells the member, and
/// the value as the request sent it.
fn not_an_account_id(parameter: &str, value: &str) -> String {
    format!("\"{parameter}\" with value \"{value}\" fails to match the AWS Account Id pattern.")
}

/// The clause stated for a grant whose end falls before the bound: the parameter under
/// the name the service states this bound under (`endDate`, not the wire member
/// `EndsAt`), and the bound as the instant that opens `date`.
fn end_before_bound(date: &str) -> String {
    format!("\"endDate\" must be greater than or equal to \"{date}T00:00:00.000Z\".")
}

/// The calendar date (`YYYY-MM-DD`) of the UTC midnight that opens the day `days` days
/// after today — read off the clock, so no row carries any one run's date.
fn utc_date_in(days: i64) -> String {
    let midnight = midnight_in_days(days);
    midnight.split('T').next().unwrap_or_default().to_string()
}

/// What the framework's own constraint walk states instead, and so what a refusal this
/// service words itself carries none of — including the lowerCamel member path that
/// walk names a member by.
const GENERIC_CONSTRAINT_PROSE: &[&str] = &[
    "validation error detected",
    "failed to satisfy constraint",
    "Member must satisfy regular expression pattern",
    r"^\d{12}$",
    "receiverPrincipal",
];

/// A `CreateDataGrant` body: the members every row carries, plus whatever the row's
/// request shape adds or overrides.
fn grant_body(
    label: &str,
    shape: &Value,
    receiver: &str,
    ends_at: &str,
    data_set_id: &str,
) -> Value {
    let mut body = json!({
        "Name": format!("grant for {label}"),
        "GrantDistributionScope": "NONE",
        "ReceiverPrincipal": receiver,
        "SourceDataSetId": data_set_id,
        "EndsAt": ends_at,
    });
    for (member, value) in shape.as_object().into_iter().flatten() {
        body[member] = value.clone();
    }
    body
}

/// Assert one refused `CreateDataGrant`: the status, the error code, that the `Message`
/// is one of the sentences the rule admits, and that it carries none of the framework's
/// generic constraint prose.
async fn assert_grant_refusal(
    router: &MultiServiceRouter,
    label: &str,
    body: Value,
    admitted: &[String],
) {
    let refused = create_data_grant(router, body).await;
    assert_eq!(
        refused.status, 400,
        "case {label}: expected 400, got {} body {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error.as_deref(),
        Some("ValidationException"),
        "case {label}: expected ValidationException, got {:?}",
        refused.error
    );
    let message = refused.string("Message");
    assert!(
        admitted.contains(&message),
        "case {label}: expected {:?}, got {message:?}",
        admitted[0]
    );
    for prose in GENERIC_CONSTRAINT_PROSE {
        assert!(
            !message.contains(prose),
            "case {label}: expected none of the framework's generic constraint prose, got \
             {prose:?} in {message:?}"
        );
    }
}

/// RULE: `CreateDataGrant` states a fault of its own parameters with the same fixed
/// opening every parameter refusal of this service carries — one clause for a receiver
/// that is not an AWS account id, naming the member as the model spells it and quoting
/// the value sent, and one for an end before the UTC midnight two days after the
/// current date, naming that bound as an instant — and creates nothing by such a call.
///
/// The rows span what the rule generalises over: the form the offending value takes,
/// how far before the bound the end falls, and which other members the request carries
/// (only the required ones, a description and tags, a grant distributed to the
/// organization) — the sentence belongs to the parameter at fault, not to one request's
/// shape.
#[tokio::test]
async fn a_data_grants_own_parameter_faults_open_the_same_way() {
    let (router, _engine) = harness();
    let data_set_id = create_data_set(
        &router,
        "grant-parameter-faults",
        "for a grant",
        "S3_SNAPSHOT",
        None,
    )
    .await
    .string("Id");

    let shapes: &[(&str, Value)] = &[
        ("carrying only the required members", json!({})),
        (
            "carrying a description and tags",
            json!({ "Description": "a shared set", "Tags": { "team": "data" } }),
        ),
        (
            "distributed to the organization",
            json!({ "GrantDistributionScope": "AWS_ORGANIZATION" }),
        ),
    ];

    // An end well after the bound, so a row about the receiver is refused for its
    // receiver and an admitted row stays admitted however long the suite runs.
    let admitted_end = midnight_in_days(END_LEAD_DAYS + 7);

    // Source: the `ReceiverPrincipal` pattern above — an account id is exactly twelve
    // ASCII digits, and none of these is.
    let receivers: &[(&str, &str)] = &[
        ("a value that is no number at all", "not-an-account-id"),
        ("one digit short", "11112222333"),
        ("one digit too many", "1111222233334"),
        ("an empty receiver", ""),
        ("an ARN naming an account", "arn:aws:iam::111122223333:root"),
        ("digits written with separators", "1111-2222-3333"),
    ];

    for (shape, extra) in shapes {
        for (fault, receiver) in receivers {
            let label = format!("a grant {shape} for {fault}");
            let body = grant_body(&label, extra, receiver, &admitted_end, &data_set_id);
            let admitted = [expected_message(&[not_an_account_id(
                "ReceiverPrincipal",
                receiver,
            )])];
            assert_grant_refusal(&router, &label, body, &admitted).await;
        }
    }

    let ends: &[(&str, String)] = &[
        (
            "at the midnight one day before the bound",
            midnight_in_days(END_LEAD_DAYS - 1),
        ),
        ("at the midnight that opened today", midnight_in_days(0)),
        ("at the instant of the call", instant_from_now(0.0)),
        ("a day before the call", instant_from_now(-DAY_SECONDS)),
        ("at the epoch", "1970-01-01T00:00:00Z".to_string()),
    ];

    for (shape, extra) in shapes {
        for (fault, ends_at) in ends {
            let label = format!("a grant {shape} ending {fault}");
            let body = grant_body(&label, extra, "111122223333", ends_at, &data_set_id);
            // The bound is read off the clock on every call, so both the date of this
            // call's UTC day and the date of the next one are admitted: the service's
            // own reading is one of them on every run, including one that straddles a
            // UTC midnight.
            let admitted = [
                expected_message(&[end_before_bound(&utc_date_in(END_LEAD_DAYS))]),
                expected_message(&[end_before_bound(&utc_date_in(END_LEAD_DAYS + 1))]),
            ];
            assert_grant_refusal(&router, &label, body, &admitted).await;
        }
    }

    // None of those refusals created anything: the list reports no grant at all.
    let listed = call(&router, "GET", "/v1/data-grants", None).await;
    assert_eq!(
        listed.status, 200,
        "case ListDataGrants after the refusals: expected 200, got {} body {:?}",
        listed.status, listed.body
    );
    let summaries = listed
        .member("DataGrantSummaries")
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        summaries.is_empty(),
        "case ListDataGrants after the refusals: expected no grant, got {summaries:?}"
    );

    // A request whose receiver IS an account id and whose end IS at or after the bound
    // is not refused for its parameters, whatever shape it has: the sentences above
    // belong to the faults and not to the operation.
    let mut created = 0usize;
    for (shape, extra) in shapes {
        let label = format!("an admitted grant {shape}");
        let body = grant_body(&label, extra, "111122223333", &admitted_end, &data_set_id);
        let answer = create_data_grant(&router, body).await;
        assert_eq!(
            answer.status, 201,
            "case {label}: expected 201, got {} body {:?}",
            answer.status, answer.body
        );
        // Round trip: the grant the call reports is the one a later read sees, carrying
        // the end the request named.
        let fetched = get_data_grant(&router, &answer.string("Id")).await;
        assert_eq!(
            seconds(fetched.member("EndsAt"), &label),
            seconds(&json!(admitted_end), &label),
            "case {label}: expected GetDataGrant to report the end the request named, got \
             {:?}",
            fetched.member("EndsAt")
        );
        created += 1;
    }

    let listed = call(&router, "GET", "/v1/data-grants", None).await;
    let summaries = listed
        .member("DataGrantSummaries")
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        summaries.len(),
        created,
        "case ListDataGrants after the admitted creates: expected exactly the {created} \
         grants they created, got {summaries:?}"
    );
}
