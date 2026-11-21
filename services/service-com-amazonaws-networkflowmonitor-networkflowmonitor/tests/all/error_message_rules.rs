// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS a refusal — the two message rules whose text is built
//! from the request or from the model, and is therefore a requirement rather than
//! one recording's prose.
//!
//! RULE 1 (a resource that is not there): a `ResourceNotFoundException` for a
//! resource this service mints an ARN for identifies that resource by its FULL ARN,
//! built from the request (the Region of the request's SigV4 credential scope, the
//! emulator account, the resource type and the identifier the request named), inside
//! the fixed wording this service uses for that resource TYPE. Nothing else about
//! the answer changes: the status stays 404 and the code stays
//! `ResourceNotFoundException`.
//!
//! RULE 2 (a value outside an enum): a boundary constraint refusal states the
//! admissible set of an enumerated member in ASCENDING ORDER of its values, not in
//! the order the model declares the enum in. Every other part of the clause — the
//! envelope, the member path, the echoed value, and the requirement of every other
//! constraint class — is the framework's and is unchanged.
//!
//! RULE 3 (how every client-error refusal CLOSES): a refusal this service's handlers
//! answer states its sentence and NOTHING after it — the envelope a client reads around a
//! refused call (the service that answered, the HTTP status, the request id and the attempts
//! that call took) is the SDK's rendering of facts its own call knows, and reaches no wire.
//! The id of the request is reported in the header this protocol names a request in, fresh
//! per call.
//!
//! RULE 3 is about the BODY, not only its text: the body of a refusal carries the sentence in
//! `message` and the error class in `__type`, and nothing else, so two refusals of ONE
//! condition carry the same body byte for byte while each names its own call in the header.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalizes over:
//! rule 1 over the resource type, the operation that reports it, the identifier's
//! form and length, and the Region the request is signed for; rule 2 over the enum
//! (its value count, and whether sorted order differs from declared order), the
//! member, and whether the member is bound to the query string, to the body, or to a
//! structure nested inside a list in the body; rule 3 over the error class and its
//! status (400, 402, 403, 404, 409), the operation that answers it, and whether the
//! sentence it closes carries anything built from the request.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::harness::{client_token, targets, Emulator, ACCOUNT_ID, REGION, SIGNING_NAME};

/// How a missing MONITOR is worded: the fixed prefix `Monitor [`, the monitor's ARN,
/// and `] doesn't exist`. The ARN namespace comes from the harness constant the other
/// suites build an expected ARN from, so this expectation stays tied to the one
/// definition of it.
fn monitor_not_found(region: &str, monitor_name: &str) -> String {
    format!(
        "Monitor [arn:aws:{SIGNING_NAME}:{region}:{ACCOUNT_ID}:monitor/{monitor_name}] \
         doesn't exist"
    )
}

/// How a missing SCOPE is worded — a DIFFERENT fixed prefix from a monitor's, around
/// the same ARN form.
fn scope_not_found(region: &str, scope_id: &str) -> String {
    format!(
        "There's no scope with ARN: \
         [arn:aws:{SIGNING_NAME}:{region}:{ACCOUNT_ID}:scope/{scope_id}]"
    )
}

/// The whole message one constraint refusal carries: the framework's envelope over
/// one clause naming the echoed value, the member path and the requirement.
fn one_validation_error(value: &str, member: &str, requirement: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{member}' failed to satisfy \
         constraint: {requirement}"
    )
}

/// The requirement an enum value set states, over the values as they are to be
/// LISTED (the rule under test decides the order; this only formats them).
fn enum_value_set(values: &[&str]) -> String {
    format!("Member must satisfy enum value set: [{}]", values.join(", "))
}

/// One row of the enum-ordering table: the enum as the MODEL declares it, the order
/// the message must list it in, and the request that reaches that member.
struct EnumRow {
    /// What this row varies, named in every assertion.
    case: &'static str,
    /// The member path the clause names.
    member: &'static str,
    /// The value set in the model's declaration order.
    declared: &'static [&'static str],
    /// The value set in the order the message must list.
    listed: &'static [&'static str],
    /// The request: its method, its URI (which carries a query-string member), and
    /// its body, absent for a GET.
    method: &'static str,
    uri: String,
    body: Option<Value>,
}

/// A `ResourceNotFoundException` answer: 404, that code, and nothing else.
///
/// The SENTENCE is what rule 1 is about, so that is what is compared; the envelope that
/// closes it is rule 3's, and is asserted whole by rule 3's own suites below.
fn assert_not_found(case: &str, answer: &crate::harness::Answer, expected_message: &str) {
    assert_eq!(
        answer.status, 404,
        "case {case:?}: expected HTTP 404, got {} with body {}",
        answer.status, answer.body
    );
    assert!(
        answer.error_type.contains("ResourceNotFoundException"),
        "case {case:?}: expected ResourceNotFoundException, got {:?}",
        answer.error_type
    );
    assert_eq!(
        answer.sentence(),
        expected_message,
        "case {case:?}: expected message {expected_message:?}, got {:?}",
        answer.sentence()
    );
}

/// A `ValidationException` answer: 400, that code, and the message the rule states.
fn assert_validation(case: &str, answer: &crate::harness::Answer, expected_message: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400, got {} with body {}",
        answer.status, answer.body
    );
    assert!(
        answer.error_type.contains("ValidationException"),
        "case {case:?}: expected ValidationException, got {:?}",
        answer.error_type
    );
    assert_eq!(
        answer.text("message"),
        expected_message,
        "case {case:?}: expected message {expected_message:?}, got {:?}",
        answer.text("message")
    );
}

/// RULE 1, for a monitor: every operation that addresses a monitor by name reports a
/// name the store holds nothing under as the monitor's ARN in this service's
/// missing-monitor wording.
///
/// The rows differ along the identifier's form and length — a plain name, a name
/// carrying every character class `ResourceName`'s pattern `^[a-zA-Z0-9_.-]+$`
/// admits, and a name at that member's `@length` maximum (255) — and along the Region
/// the request is signed for, which is the Region the reported ARN must carry.
#[tokio::test]
async fn a_missing_monitor_is_reported_by_its_arn() {
    // (case, the Region the request is signed for, the monitorName asked for)
    let rows: [(&str, &str, String); 4] = [
        (
            "a plain name never created",
            REGION,
            "sim2real-probe-missing-monitor".to_string(),
        ),
        (
            "a name using every character class the pattern admits",
            REGION,
            "Monitor_9.probe-A".to_string(),
        ),
        (
            // `ResourceName`'s own `@length` maximum, from the model.
            "a name at the member's length maximum",
            REGION,
            "m".repeat(255),
        ),
        (
            "a request signed for another Region",
            "eu-central-1",
            "missing-monitor-elsewhere".to_string(),
        ),
    ];

    for (case, region, name) in rows {
        let emulator = Emulator::new_in(region);
        let expected = monitor_not_found(region, &name);

        assert_not_found(
            &format!("{case} (GetMonitor)"),
            &emulator.get_monitor(&name).await,
            &expected,
        );
        assert_not_found(
            &format!("{case} (DeleteMonitor)"),
            &emulator.delete_monitor(&name).await,
            &expected,
        );
        assert_not_found(
            &format!("{case} (UpdateMonitor)"),
            &emulator
                .update_monitor(&name, json!({ "clientToken": client_token(7) }))
                .await,
            &expected,
        );
    }
}

/// RULE 1, for a scope: the scope readers and the scope delete report a well-formed
/// `scopeId` the store holds nothing under as the scope's ARN, in the service's
/// missing-SCOPE wording — a different prefix from a monitor's, which is why the
/// renderer is per resource type.
///
/// The rows differ along the id (every one well formed: the shape this service mints,
/// so the refusal is the genuine not-found path) and the Region the request is signed
/// for.
#[tokio::test]
async fn a_missing_scope_is_reported_by_its_arn() {
    // (case, the Region the request is signed for, the scopeId asked for)
    let rows: [(&str, &str, &str); 3] = [
        (
            "an id of the shape this service mints, never created",
            REGION,
            "00000000-0000-4000-8000-000000000001",
        ),
        (
            "a second such id",
            REGION,
            "00000000-0000-4000-8000-00000000beef",
        ),
        (
            "a request signed for another Region",
            "ap-southeast-2",
            "00000000-0000-4000-8000-000000000042",
        ),
    ];

    for (case, region, scope_id) in rows {
        let emulator = Emulator::new_in(region);
        let expected = scope_not_found(region, scope_id);

        assert_not_found(
            &format!("{case} (GetScope)"),
            &emulator.get_scope(scope_id).await,
            &expected,
        );
        assert_not_found(
            &format!("{case} (DeleteScope)"),
            &emulator.delete_scope(scope_id).await,
            &expected,
        );
    }
}

/// RULE 1's invariant, round-tripped through the store: the ARN a refusal reports for
/// a resource that is gone is EXACTLY the ARN that resource reported while it existed.
///
/// A create mints the ARN, the reader reports it, the delete removes the resource, and
/// the reader then refuses with that same ARN — so the message a teardown flow logs
/// identifies the very resource the flow created.
#[tokio::test]
async fn the_reported_arn_is_the_arn_the_resource_carried() {
    let emulator = Emulator::new();

    let created_scope = emulator
        .create_scope(json!({
            "targets": targets(&[("123456789012", "us-east-1")]),
            "clientToken": client_token(1),
        }))
        .await;
    assert_eq!(
        created_scope.status, 200,
        "the scope must be created: got {} with body {}",
        created_scope.status, created_scope.body
    );
    let scope_id = created_scope.text("scopeId");
    let scope_arn = created_scope.text("scopeArn");
    let scope_arn_from_read = emulator.get_scope(&scope_id).await.text("scopeArn");
    assert_eq!(
        scope_arn_from_read, scope_arn,
        "the scope reader must report the minted ARN: expected {scope_arn:?}, got \
         {scope_arn_from_read:?}"
    );

    let monitor_name = "round-trip-probe";
    let created_monitor = emulator
        .create_monitor(json!({
            "monitorName": monitor_name,
            "scopeArn": scope_arn,
            "localResources": [{ "type": "AWS::EC2::VPC", "identifier": "vpc-1" }],
            "remoteResources": [{ "type": "AWS::AWSService", "identifier": "S3" }],
            "clientToken": client_token(2),
        }))
        .await;
    assert_eq!(
        created_monitor.status, 200,
        "the monitor must be created: got {} with body {}",
        created_monitor.status, created_monitor.body
    );
    let monitor_arn = created_monitor.text("monitorArn");
    let monitor_arn_from_read = emulator.get_monitor(monitor_name).await.text("monitorArn");
    assert_eq!(
        monitor_arn_from_read, monitor_arn,
        "the monitor reader must report the minted ARN: expected {monitor_arn:?}, got \
         {monitor_arn_from_read:?}"
    );

    assert_eq!(
        emulator.delete_monitor(monitor_name).await.status,
        200,
        "the monitor must delete"
    );
    assert_eq!(
        emulator.delete_scope(&scope_id).await.status,
        200,
        "the scope must delete"
    );

    assert_not_found(
        "the deleted monitor, read back",
        &emulator.get_monitor(monitor_name).await,
        &format!("Monitor [{monitor_arn}] doesn't exist"),
    );
    assert_not_found(
        "the deleted scope, read back",
        &emulator.get_scope(&scope_id).await,
        &format!("There's no scope with ARN: [{scope_arn}]"),
    );
}

/// RULE 2: an enum's admissible set is listed in ascending order of its values.
///
/// The value sets below are the model's own, read with
/// `smithy_query(query="shape",shape="MonitorStatus,MonitorMetric,
/// WorkloadInsightsMetric,DestinationCategory")`, and each row carries them BOTH as
/// the model declares them and as the message must list them, so a row asserts the
/// order rather than merely the membership. The rows differ along the enum (3, 4, 5
/// and 7 values; declared order differing from sorted order in every one), the member,
/// and where the member is bound: a query-string member, two body members of different
/// operations, and a member of a structure nested inside a list in the body.
#[tokio::test]
async fn an_enum_value_set_is_listed_in_ascending_order() {
    const BAD: &str = "NOT_A_REAL_VALUE";
    let window = json!({ "startTime": "2026-01-01T00:00:00Z", "endTime": "2026-01-01T01:00:00Z" });
    let start = |member: &str, value: &str| {
        let mut body = window.clone();
        let object = body.as_object_mut().expect("a JSON object");
        object.insert("metricName".to_string(), json!("TIMEOUTS"));
        object.insert("destinationCategory".to_string(), json!("INTRA_AZ"));
        object.insert(member.to_string(), json!(value));
        body
    };

    let rows: [EnumRow; 4] = [
        EnumRow {
            case: "MonitorStatus, bound to ListMonitors' query string",
            member: "monitorStatus",
            declared: &["PENDING", "ACTIVE", "INACTIVE", "ERROR", "DELETING"],
            listed: &["ACTIVE", "DELETING", "ERROR", "INACTIVE", "PENDING"],
            method: "GET",
            uri: format!("/monitors?monitorStatus={BAD}"),
            body: None,
        },
        EnumRow {
            case: "MonitorMetric, a body member of StartQueryMonitorTopContributors",
            member: "metricName",
            declared: &[
                "ROUND_TRIP_TIME",
                "TIMEOUTS",
                "RETRANSMISSIONS",
                "DATA_TRANSFERRED",
            ],
            listed: &[
                "DATA_TRANSFERRED",
                "RETRANSMISSIONS",
                "ROUND_TRIP_TIME",
                "TIMEOUTS",
            ],
            method: "POST",
            uri: "/monitors/any-monitor/topContributorsQueries".to_string(),
            body: Some(start("metricName", BAD)),
        },
        EnumRow {
            case: "DestinationCategory, a body member of the same operation",
            member: "destinationCategory",
            declared: &[
                "INTRA_AZ",
                "INTER_AZ",
                "INTER_VPC",
                "UNCLASSIFIED",
                "AMAZON_S3",
                "AMAZON_DYNAMODB",
                "INTER_REGION",
            ],
            listed: &[
                "AMAZON_DYNAMODB",
                "AMAZON_S3",
                "INTER_AZ",
                "INTER_REGION",
                "INTER_VPC",
                "INTRA_AZ",
                "UNCLASSIFIED",
            ],
            method: "POST",
            uri: "/monitors/any-monitor/topContributorsQueries".to_string(),
            body: Some(start("destinationCategory", BAD)),
        },
        EnumRow {
            case: "WorkloadInsightsMetric, a body member of the scope-wide query",
            member: "metricName",
            declared: &["TIMEOUTS", "RETRANSMISSIONS", "DATA_TRANSFERRED"],
            listed: &["DATA_TRANSFERRED", "RETRANSMISSIONS", "TIMEOUTS"],
            method: "POST",
            uri: "/workloadInsights/00000000-0000-4000-8000-000000000001/topContributorsQueries"
                .to_string(),
            body: Some(start("metricName", BAD)),
        },
    ];

    let emulator = Emulator::new();
    for row in rows {
        let case = row.case;
        let answer = emulator.call(row.method, &row.uri, row.body).await;
        assert_validation(
            case,
            &answer,
            &one_validation_error(BAD, row.member, &enum_value_set(row.listed)),
        );
        // The rule is about ORDER, so the declaration order the model happens to use
        // must NOT be what the message lists.
        assert_ne!(
            row.declared, row.listed,
            "case {case:?}: this row only tests the ordering rule if the two orders differ"
        );
        assert!(
            !answer.text("message").contains(&enum_value_set(row.declared)),
            "case {case:?}: expected the sorted value set, got the declared order in {:?}",
            answer.text("message")
        );
    }
}

/// RULE 2, reached through a structure NESTED inside a list in the body: the clause is
/// rendered by the same renderer, so a nested enum carries its value set too.
///
/// `TargetType` declares exactly one value (`ACCOUNT`,
/// `smithy_query(query="shape",shape="TargetType")`), so this row pins the reach of
/// the rule rather than the ordering the row above pins.
#[tokio::test]
async fn a_nested_enum_value_set_is_listed_too() {
    let emulator = Emulator::new();
    let answer = emulator
        .create_scope(json!({
            "targets": [{
                "targetIdentifier": {
                    "targetId": { "accountId": "123456789012" },
                    "targetType": "ORGANIZATION"
                },
                "region": "us-east-1"
            }],
        }))
        .await;
    assert_eq!(
        answer.status, 400,
        "a nested enum outside its value set must be refused: got {} with body {}",
        answer.status, answer.body
    );
    let message = answer.text("message");
    assert!(
        message.starts_with("1 validation error detected: Value 'ORGANIZATION' at '")
            && message.contains("targetType")
            && message.ends_with(&enum_value_set(&["ACCOUNT"])),
        "expected the nested member's clause to state the ACCOUNT value set, got {message:?}"
    );
}

/// The complement of rule 2: every OTHER constraint class keeps the framework's own
/// requirement and the framework's envelope, because the renderer rewrites the enum
/// value set and nothing else.
///
/// The rows differ along the class — a `@range` maximum and minimum from the model's
/// `MaxResults` (1..25), tested on the published bound and one past it, and an absent
/// `@required` member — and along where the member is bound.
#[tokio::test]
async fn a_requirement_that_is_not_an_enum_keeps_the_frameworks_wording() {
    let emulator = Emulator::new();

    // `MaxResults`' own `@range` (min 1, max 25), from the model.
    let in_range = emulator.list_monitors("?maxResults=25").await;
    assert_eq!(
        in_range.status, 200,
        "the published maximum must be accepted: got {} with body {}",
        in_range.status, in_range.body
    );
    let in_range = emulator.list_monitors("?maxResults=1").await;
    assert_eq!(
        in_range.status, 200,
        "the published minimum must be accepted: got {} with body {}",
        in_range.status, in_range.body
    );

    assert_validation(
        "one past the range maximum",
        &emulator.list_monitors("?maxResults=26").await,
        &one_validation_error(
            "26",
            "maxResults",
            "Member must have value less than or equal to 25",
        ),
    );
    assert_validation(
        "one past the range minimum",
        &emulator.list_monitors("?maxResults=0").await,
        &one_validation_error(
            "0",
            "maxResults",
            "Member must have value greater than or equal to 1",
        ),
    );

    let absent = emulator
        .call(
            "POST",
            "/monitors/any-monitor/topContributorsQueries",
            Some(json!({
                "startTime": "2026-01-01T00:00:00Z",
                "endTime": "2026-01-01T01:00:00Z",
                "destinationCategory": "INTRA_AZ",
            })),
        )
        .await;
    assert_validation(
        "an absent required member",
        &absent,
        "1 validation error detected: Value null at 'metricName' failed to satisfy constraint: \
         Member must not be null",
    );
}

/// The statuses this service refuses a CLIENT with, one per modelled client-error class:
/// `ValidationException` (400), `ServiceQuotaExceededException` (402),
/// `AccessDeniedException` (403), `ResourceNotFoundException` (404) and `ConflictException`
/// (409) — from the classes the operations of this service declare in the model. A table
/// claiming to span the refusals of this service is compared against this one definition.
fn client_error_statuses() -> BTreeSet<u16> {
    BTreeSet::from([400u16, 402, 403, 404, 409])
}

/// Assert `answer` is a client error whose message is `sentence` and nothing after it, and
/// report the id its own header named the request by.
///
/// The id is read off the answer's header rather than out of the text: the envelope a client
/// renders around a refusal is the SDK's, so the message closes at the sentence.
fn assert_envelope(case: &str, answer: &crate::harness::Answer, sentence: &str) -> String {
    assert!(
        (400..500).contains(&answer.status),
        "case {case:?}: expected a client error, got {} with body {}",
        answer.status,
        answer.body
    );
    assert!(
        ::service_common::query_error_envelope::is_request_id(&answer.request_id),
        "case {case:?}: the response must name its request in the header AWS reports it in, \
         got {:?}",
        answer.request_id
    );
    assert_eq!(
        answer.text("message"),
        sentence,
        "case {case:?}: expected {sentence:?}, got {:?}",
        answer.text("message")
    );
    answer.request_id.clone()
}

/// RULE 3: every client-error refusal this service's handlers answer states its sentence and
/// nothing after it, and names its own call in the header.
///
/// The rows span the error classes this service refuses a client with and their statuses —
/// `ValidationException` (400), `ServiceQuotaExceededException` (402),
/// `AccessDeniedException` (403), `ResourceNotFoundException` (404) and
/// `ConflictException` (409) — the operations that answer them, and both kinds of
/// sentence: one built from the request (an ARN, a limit) and one wholly fixed.
#[tokio::test]
async fn every_client_error_refusal_states_its_sentence_alone() {
    let emulator = Emulator::new();
    let scope_arn = emulator.scope_for_monitors().await;

    // A monitor that EXISTS, so the conflict row below is refused for the name it names.
    let held = "envelope-probe";
    let created = emulator
        .create_monitor(json!({
            "monitorName": held,
            "scopeArn": scope_arn,
            "localResources": [{ "type": "AWS::EC2::VPC", "identifier": "vpc-envelope" }],
            "clientToken": client_token(11),
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "the held monitor must be created: got {} with body {}",
        created.status, created.body
    );

    // Identifiers of the shape this service mints that the store holds nothing under.
    let missing_monitor = "sim2real-probe-missing-monitor";
    let missing_scope = "00000000-0000-4000-8000-0000000000ff";
    let absent_scope_arn =
        format!("arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:scope/{missing_scope}");
    let untagged_arn =
        format!("arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:monitor/{missing_monitor}");
    let monitor_body = |name: &str, scope: &str| {
        json!({
            "monitorName": name,
            "scopeArn": scope,
            "localResources": [{ "type": "AWS::EC2::VPC", "identifier": "vpc-row" }],
        })
    };

    // (case, the answer, the sentence that answer must word)
    let rows: Vec<(&str, crate::harness::Answer, String)> = vec![
        (
            "GetMonitor on a name the store holds nothing under",
            emulator.get_monitor(missing_monitor).await,
            monitor_not_found(REGION, missing_monitor),
        ),
        (
            "DeleteMonitor on that same name",
            emulator.delete_monitor(missing_monitor).await,
            monitor_not_found(REGION, missing_monitor),
        ),
        (
            "UpdateMonitor on that same name",
            emulator
                .update_monitor(missing_monitor, json!({ "clientToken": client_token(12) }))
                .await,
            monitor_not_found(REGION, missing_monitor),
        ),
        (
            "GetScope on a well-formed id the store holds nothing under",
            emulator.get_scope(missing_scope).await,
            scope_not_found(REGION, missing_scope),
        ),
        (
            "DeleteScope on that same id",
            emulator.delete_scope(missing_scope).await,
            scope_not_found(REGION, missing_scope),
        ),
        (
            "GetScope on an identifier of another form",
            emulator.get_scope("not-a-scope-identifier").await,
            "The provided Scope ARN is not valid".to_string(),
        ),
        (
            "CreateMonitor naming a scope nothing resolves",
            emulator
                .create_monitor(monitor_body("unparented", &absent_scope_arn))
                .await,
            "scope supplied doesn't exist".to_string(),
        ),
        (
            "ListMonitors resuming from a cursor this service never issued",
            emulator.list_monitors("?nextToken=zzzz").await,
            "NextToken provided isn't valid.".to_string(),
        ),
        (
            "ListTagsForResource on an ARN the store holds nothing under",
            emulator.list_tags(&untagged_arn).await,
            format!("Resource [{untagged_arn}] doesn't exist"),
        ),
        (
            "CreateScope claiming a second slot in the Region",
            emulator
                .create_scope(json!({
                    "targets": targets(&[("123456789012", "us-east-1")]),
                    "clientToken": client_token(13),
                }))
                .await,
            // `state::SCOPE_QUOTA` is one scope per account and Region.
            format!(
                "Maximum limit of [1] for resource [scope] exceeded for account [{ACCOUNT_ID}]."
            ),
        ),
        (
            "CreateMonitor of a name the store already holds",
            emulator.create_monitor(monitor_body(held, &scope_arn)).await,
            "Cannot create a monitor which already exists".to_string(),
        ),
    ];

    let mut statuses: BTreeSet<u16> = BTreeSet::new();
    let mut request_ids: Vec<String> = Vec::new();
    for (case, answer, sentence) in &rows {
        request_ids.push(assert_envelope(case, answer, sentence));
        statuses.insert(answer.status);
    }

    // The table spans the five client-error statuses this service refuses with.
    assert_eq!(
        statuses,
        client_error_statuses(),
        "the rows must span every client-error status this service answers, got {statuses:?}"
    );
    // Per REFUSAL, not per condition: no two of the refusals above named one request id.
    let distinct: BTreeSet<&String> = request_ids.iter().collect();
    assert_eq!(
        distinct.len(),
        request_ids.len(),
        "each refusal names its own call, got {request_ids:?}"
    );
}

/// RULE 3's two halves that one refusal cannot show: the id a response names its request by
/// is a fresh one per call, and NO refusal — neither one this service's handlers word nor one
/// the framework raises at the request boundary — carries the client's envelope in its text.
#[tokio::test]
async fn each_refusal_names_its_own_call_and_none_carries_the_clients_envelope() {
    let emulator = Emulator::new();
    let missing = "repeated-probe";
    let sentence = monitor_not_found(REGION, missing);

    // The same condition, refused twice: one sentence, two calls, two ids.
    let mut ids: Vec<String> = Vec::new();
    for attempt in 0..3 {
        let answer = emulator.get_monitor(missing).await;
        let case = format!("attempt {attempt}");
        ids.push(assert_envelope(&case, &answer, &sentence));
    }
    let distinct: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(
        distinct.len(),
        ids.len(),
        "three refusals of one condition are three calls, so each names its own, got \
         {ids:?}"
    );

    // A refusal raised at the request BOUNDARY keeps the framework's wording, and it states
    // no envelope either: the sentence a client reads there is the constraint clause alone.
    for (case, answer) in [
        (
            "a page size past the declared maximum",
            emulator.list_monitors("?maxResults=26").await,
        ),
        (
            "a value outside a declared enum",
            emulator.list_monitors("?monitorStatus=NOT_A_REAL_VALUE").await,
        ),
        (
            "a resource this service holds nothing for",
            emulator.get_monitor(missing).await,
        ),
    ] {
        let message = answer.text("message");
        assert!(
            !message.contains("(Service: ") && !message.contains("Status Code: "),
            "case {case:?}: a refusal states its sentence alone, got {message:?}"
        );
    }

    // And a SUCCESS still names the request it answered, so a client correlating on the
    // header reads one id whether the call was served or refused.
    let served = emulator.list_monitors("").await;
    assert_eq!(
        served.status, 200,
        "the listing must succeed, got {} with body {}",
        served.status, served.body
    );
    assert!(
        ::service_common::query_error_envelope::is_request_id(&served.request_id),
        "a served call names its request too, got {:?}",
        served.request_id
    );
}

/// One row of the refusal-BODY table: the call that is refused, the status and the modelled
/// error class it is refused with, and the sentence its `message` must carry.
struct BodyRow {
    /// What this row varies, named in every assertion.
    case: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    status: u16,
    /// The error class the model declares for this condition, as the protocol renders it.
    class: &'static str,
    sentence: String,
}

/// The whole of what a refused call carries: the status, and a body of the two members this
/// protocol renders an error with — the sentence in `message` and the class in `__type`.
///
/// Nothing a client renders around the call is in it: not the service that answered, not the
/// status, not the id of the request (which travels in the header and is read from there) and
/// not the attempts the call took.
fn assert_refusal_body(
    case: &str,
    answer: &crate::harness::Answer,
    status: u16,
    class: &str,
    sentence: &str,
) {
    // The status, the sentence and the id in the header are rule 3's own assertions, made by
    // the one helper that makes them; what follows is what the BODY adds to them.
    assert_envelope(case, answer, sentence);
    assert_eq!(
        answer.status, status,
        "case {case:?}: expected HTTP {status}, got {} with body {}",
        answer.status, answer.body
    );
    let members: BTreeSet<&str> = answer
        .body
        .as_object()
        .map(|body| body.keys().map(String::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        members,
        BTreeSet::from(["__type", "message"]),
        "case {case:?}: a refusal body carries the class and the sentence alone, got members \
         {members:?}"
    );
    assert_eq!(
        answer.class(),
        class,
        "case {case:?}: expected the class {class:?}, got {:?} with body {}",
        answer.error_type,
        answer.body
    );

    let rendered = answer.body.to_string();
    // The clauses an SDK renders AROUND a refusal when it raises its exception; no byte of
    // one belongs to the answer.
    for rendered_by_the_client in [" (Service: ", "Status Code: ", "SDK Attempt Count"] {
        assert!(
            !rendered.contains(rendered_by_the_client),
            "case {case:?}: {rendered_by_the_client:?} is the client's own rendering and \
             reaches no wire, got {rendered}"
        );
    }
    assert!(
        !rendered.contains(&answer.request_id),
        "case {case:?}: the id of the call belongs to the header alone, got {rendered}"
    );
}

/// RULE 3 at the BODY a refusal carries: the sentence in `message`, the class in `__type`,
/// nothing else — so two refusals of one condition carry the SAME body, while each names its
/// own call in the header.
///
/// This is the half one message comparison cannot show. A refusal whose text carried the
/// client's envelope would carry THAT call's request id inside `message`, so two refusals of
/// one condition would never compare equal, no exact-match assertion on an error body could
/// hold, and every error body would be longer than the service's own answer.
///
/// The rows span the status (400, 402, 403, 404, 409), the layer that refuses (a handler of
/// this service, and the request boundary), the operation, and the kind of sentence (built
/// from the request, or wholly fixed). The 404 row is the teardown read of a tag/untag round
/// trip — the scope is created, tagged, untagged and deleted — so it is the genuine
/// not-found path a flow reaches after its own teardown.
#[tokio::test]
async fn a_refusal_body_carries_the_sentence_and_the_class_alone() {
    let emulator = Emulator::new();

    // The round trip the 404 row below closes: a scope created, tagged, untagged, torn down.
    let created = emulator
        .create_scope(json!({
            "targets": targets(&[("123456789012", "us-east-1")]),
            "clientToken": client_token(21),
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "the scope must be created: got {} with body {}",
        created.status, created.body
    );
    let scope_id = created.text("scopeId");
    let scope_arn = created.text("scopeArn");
    for (case, answer) in [
        (
            "tagging the scope",
            emulator
                .tag_resource(&scope_arn, &json!({ "owner": "sweep" }))
                .await,
        ),
        (
            "untagging it again",
            emulator.untag_resource(&scope_arn, &["owner"]).await,
        ),
    ] {
        assert_eq!(
            answer.status, 204,
            "case {case:?}: expected 204, got {} with body {}",
            answer.status, answer.body
        );
    }
    let deleted = emulator.delete_scope(&scope_id).await;
    assert_eq!(
        deleted.status, 200,
        "the scope must delete: got {} with body {}",
        deleted.status, deleted.body
    );

    // A scope and a monitor the store HOLDS, so the quota row is refused for the slot the
    // scope took and the conflict row for the name the monitor holds.
    let live_scope = emulator.scope_for_monitors().await;
    let held = "wire-body-probe";
    let monitor_body = |name: &str, scope: &str| {
        json!({
            "monitorName": name,
            "scopeArn": scope,
            "localResources": [{ "type": "AWS::EC2::VPC", "identifier": "vpc-body" }],
        })
    };
    let held_monitor = emulator.create_monitor(monitor_body(held, &live_scope)).await;
    assert_eq!(
        held_monitor.status, 200,
        "the held monitor must be created: got {} with body {}",
        held_monitor.status, held_monitor.body
    );

    let missing_monitor = "wire-body-missing";
    let absent_scope_arn = format!(
        "arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:scope/\
         00000000-0000-4000-8000-0000000000ff"
    );
    let rows: Vec<BodyRow> = vec![
        BodyRow {
            case: "the teardown read of the round trip (GetScope)",
            method: "GET",
            uri: format!("/scopes/{scope_id}"),
            body: None,
            status: 404,
            class: "ResourceNotFoundException",
            sentence: scope_not_found(REGION, &scope_id),
        },
        BodyRow {
            case: "a scope identifier of another form",
            method: "GET",
            uri: "/scopes/not-a-scope-identifier".to_string(),
            body: None,
            status: 400,
            class: "ValidationException",
            sentence: "The provided Scope ARN is not valid".to_string(),
        },
        BodyRow {
            case: "a monitor name the store holds nothing under",
            method: "GET",
            uri: format!("/monitors/{missing_monitor}"),
            body: None,
            status: 404,
            class: "ResourceNotFoundException",
            sentence: monitor_not_found(REGION, missing_monitor),
        },
        BodyRow {
            case: "a cursor this service never issued",
            method: "GET",
            uri: "/monitors?nextToken=zzzz".to_string(),
            body: None,
            status: 400,
            class: "ValidationException",
            sentence: "NextToken provided isn't valid.".to_string(),
        },
        BodyRow {
            case: "a page size past the declared maximum, raised at the boundary",
            method: "GET",
            uri: "/monitors?maxResults=26".to_string(),
            body: None,
            status: 400,
            class: "ValidationException",
            sentence: one_validation_error(
                "26",
                "maxResults",
                "Member must have value less than or equal to 25",
            ),
        },
        BodyRow {
            case: "a second scope slot in the Region",
            method: "POST",
            uri: "/scopes".to_string(),
            body: Some(json!({
                "targets": targets(&[("123456789012", "us-east-1")]),
                "clientToken": client_token(22),
            })),
            status: 402,
            class: "ServiceQuotaExceededException",
            // `state::SCOPE_QUOTA` is one scope per account and Region.
            sentence: format!(
                "Maximum limit of [1] for resource [scope] exceeded for account [{ACCOUNT_ID}]."
            ),
        },
        BodyRow {
            case: "a monitor naming a scope nothing resolves",
            method: "POST",
            uri: "/monitors".to_string(),
            body: Some(monitor_body("unparented", &absent_scope_arn)),
            status: 403,
            class: "AccessDeniedException",
            sentence: "scope supplied doesn't exist".to_string(),
        },
        BodyRow {
            case: "a monitor name the store already holds",
            method: "POST",
            uri: "/monitors".to_string(),
            body: Some(monitor_body(held, &live_scope)),
            status: 409,
            class: "ConflictException",
            sentence: "Cannot create a monitor which already exists".to_string(),
        },
    ];

    let mut statuses: BTreeSet<u16> = BTreeSet::new();
    for row in &rows {
        let first = emulator.call(row.method, &row.uri, row.body.clone()).await;
        let again = emulator.call(row.method, &row.uri, row.body.clone()).await;
        assert_refusal_body(row.case, &first, row.status, row.class, &row.sentence);
        assert_refusal_body(row.case, &again, row.status, row.class, &row.sentence);
        // One condition, refused twice: one body, and two calls each naming its own id.
        assert_eq!(
            first.body, again.body,
            "case {:?}: two refusals of one condition carry one body, got {} then {}",
            row.case, first.body, again.body
        );
        assert_ne!(
            first.request_id, again.request_id,
            "case {:?}: each refusal names its own call, got {:?} twice",
            row.case, first.request_id
        );
        statuses.insert(row.status);
    }
    assert_eq!(
        statuses,
        client_error_statuses(),
        "the rows must span every client-error status this service answers, got {statuses:?}"
    );
}
