// SPDX-License-Identifier: CC0-1.0

//! How ConnectHealth words a request whose `domainId` is not a domain id.
//!
//! The RULE under test: a value supplied for this service's `domainId` member that is
//! outside what `DomainId` declares — `@length(min: 20, max: 25)` and `@pattern
//! "^(hai-|dom-)[a-z0-9]+$"`
//! (`smithy_query(query="shape",shape="DomainId",model_path="services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json")`)
//! — is refused by the SERVICE with its own sentence: HTTP 400, error code
//! `ValidationException`, and a detail text that opens with the value exactly as the
//! request supplied it, then the fixed `is not a valid domain id.`, then the SDK-style
//! diagnostic tail, of which only the leading `(Service: ` is fixed (every field inside it
//! is per-request). It is NOT the framework's generic member-constraint clause, which names
//! the member and the requirement it failed.
//!
//! The rows differ along what the rule generalises over:
//!
//! * WHICH operation carries the domain id — every operation of this service that binds a
//!   `DomainId` member, across three HTTP methods, both URI families (`/domain/…` and
//!   `/domains/…`) and the one operation that binds it to a request HEADER and reads its
//!   own members (`StartMedicalScribeListeningSession`, driven in its own test below),
//!   listed from
//!   `smithy_query(query="digest",model_path="services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json")`;
//! * the FORM of the fault — a prefix outside the pattern at a length inside the bound (the
//!   form the corpus recorded), a value below the shortest modelled length, one above the
//!   longest, a modelled prefix with characters outside the pattern's class, and a value
//!   that fails the length and the pattern at once. The length rows sit at each published
//!   bound and one past each.
//!
//! The positive control is the request this service SERVES: a `domainId` it minted itself,
//! under which `CreateSubscription` is accepted and `ListSubscriptions` reports the
//! subscription — so every refusal below is answered by a service that does serve these
//! operations. The negative controls are the refusals this sentence must NOT reach: another
//! member's value outside its own bound, and a bad domain id alongside another bad member,
//! both of which keep the framework's clause.

use serde_json::{json, Value};

use service_com_amazonaws_connecthealth_connecthealth::ops::start_medical_scribe_listening_session::{
    MODELED_METHOD, MODELED_PATH,
};

use crate::domain_subscription_lifecycle::{error_code, string_at, Emulator};
use crate::patient_insights_job_lifecycle::{minimal_body, start_path};

/// The Region every request of this suite is signed for.
const REGION: &str = "us-west-2";

/// The wire error code a refused domain id carries.
const VALIDATION_CODE: &str = "ValidationException";

/// The body member a refusal reports its detail text in for every plane of this service
/// but one, and for a refusal the FRAMEWORK renders before any handler runs: the member
/// name the model spells (`src/ops/types/exception_wire.rs`, and
/// `vendor/smithy-server-runtime/src/pre_dispatch.rs` for the framework's own envelope).
/// Every row of this suite that names a `domainId` is answered under it.
const MODEL_MEMBER_DETAIL: &str = "message";

/// The body member the SUBSCRIPTION plane reports it in — the one row below that plane
/// answers, a page size it cannot serve.
const SUBSCRIPTION_PLANE_DETAIL: &str = "Message";

/// The FIXED text of this service's domain-id refusal, after the value the request
/// supplied.
const NOT_A_VALID_DOMAIN_ID: &str = "is not a valid domain id.";

/// The one part of the SDK-style diagnostic tail that is a requirement; everything after it
/// is per-request (it carries the id of the request being answered).
const DIAGNOSTIC_TAIL_OPENING: &str = " (Service: ";

/// The FIXED half of that envelope: the component the relayed call was made to, and the
/// status it answered. Only the request id and the attempt count after it are per-request,
/// so this much of the tail is pinned — a drift in it is a drift a client parsing the
/// message would see.
const DIAGNOSTIC_TAIL_FIXED: &str = " (Service: HealthAgent, Status Code: 400, Request ID: ";

/// The leading words of the framework's generic member-constraint clause — the wording this
/// service does NOT use for a domain id, and does still use for every other member.
const GENERIC_CLAUSE_OPENING: &str = "validation error";

/// A well-formed `SubscriptionId` (`@length 25..25`, `^sub-[a-zA-Z0-9]{21}$`), so a row
/// addressing a subscription fails on its domain id alone.
const WELL_FORMED_SUBSCRIPTION_ID: &str = "sub-000000000000000000000";

/// A well-formed `ScribeSessionId` (`@length 36..36`, the 8-4-4-4-12 hex form), for the same
/// reason.
const WELL_FORMED_SESSION_ID: &str = "00000000-0000-4000-8000-000000000000";

/// A well-formed `JobId` (`@length 1..36`, no pattern), for the same reason.
const WELL_FORMED_JOB_ID: &str = "job-000000000000";

/// The media type an event-stream body carries, which is what
/// `StartMedicalScribeListeningSession` is addressed with.
const EVENT_STREAM: &str = "application/vnd.amazon.eventstream";

/// The headers `StartMedicalScribeListeningSession` binds its input members to
/// (`smithy_query(query="shape",shape="StartMedicalScribeListeningSessionInput")`): the
/// `domainId` of that operation rides in one of them rather than in the URI.
const HEADER_SESSION_ID: &str = "x-amzn-medscribe-session-id";
const HEADER_DOMAIN_ID: &str = "x-amzn-medscribe-domain-id";
const HEADER_SUBSCRIPTION_ID: &str = "x-amzn-medscribe-subscription-id";
const HEADER_LANGUAGE_CODE: &str = "x-amzn-medscribe-language-code";
const HEADER_SAMPLE_RATE: &str = "x-amzn-medscribe-sample-rate";
const HEADER_MEDIA_ENCODING: &str = "x-amzn-medscribe-media-encoding";

/// A well-formed `ScribeSessionId` for the event-stream rows.
const WELL_FORMED_STREAM_SESSION_ID: &str = "0000000a-1a2b-4c3d-8e4f-00000000000a";

/// The detail text of an error body, under the member the layer that rendered it reports
/// it in.
fn detail_text(case: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the error body must carry a {member} member, got {body}")
        })
        .to_string()
}

/// Every request of this service that carries a `domainId` in its URI: the method and the
/// path it is addressed at, with `domain_id` substituted in, plus the body it needs.
///
/// One row per operation whose model binds a `DomainId` `@httpLabel`. The companion
/// identifiers are all well formed, so the domain id is the only fault in the request.
fn requests_carrying(domain_id: &str) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    vec![
        (
            "CreateSubscription",
            "POST",
            format!("/domains/{domain_id}/subscriptions"),
            Some(json!({})),
        ),
        (
            "ListSubscriptions",
            "GET",
            format!("/domains/{domain_id}/subscriptions"),
            None,
        ),
        (
            "GetSubscription",
            "GET",
            format!("/domains/{domain_id}/subscriptions/{WELL_FORMED_SUBSCRIPTION_ID}"),
            None,
        ),
        (
            "ActivateSubscription",
            "POST",
            format!("/domains/{domain_id}/subscriptions/{WELL_FORMED_SUBSCRIPTION_ID}/activate"),
            None,
        ),
        (
            "DeactivateSubscription",
            "POST",
            format!("/domains/{domain_id}/subscriptions/{WELL_FORMED_SUBSCRIPTION_ID}/deactivate"),
            None,
        ),
        ("GetDomain", "GET", format!("/domain/{domain_id}"), None),
        (
            "DeleteDomain",
            "DELETE",
            format!("/domain/{domain_id}"),
            None,
        ),
        (
            "GetMedicalScribeListeningSession",
            "GET",
            format!(
                "/medical-scribe-stream/domain/{domain_id}/subscription/\
                 {WELL_FORMED_SUBSCRIPTION_ID}/session/{WELL_FORMED_SESSION_ID}"
            ),
            None,
        ),
        (
            "GetPatientInsightsJob",
            "GET",
            format!("/domain/{domain_id}/patient-insights-job/{WELL_FORMED_JOB_ID}"),
            None,
        ),
        (
            "StartPatientInsightsJob",
            "POST",
            start_path(domain_id),
            Some(minimal_body()),
        ),
    ]
}

/// The values a request may supply for a domain id that this service cannot read as one:
/// the case name and the value, one row per form of the fault.
fn unreadable_domain_ids() -> Vec<(&'static str, String)> {
    vec![
        // The form the corpus recorded: a 21-character value inside the modelled length
        // whose prefix is outside the modelled pattern.
        (
            "a foreign prefix at a length inside the bound",
            format!("xxx-{}", "0".repeat(17)),
        ),
        // One below the shortest modelled length (20), with a modelled prefix.
        (
            "one character below the shortest modelled length",
            format!("dom-{}", "a".repeat(15)),
        ),
        // One above the longest modelled length (25), with a modelled prefix.
        (
            "one character above the longest modelled length",
            format!("hai-{}", "b".repeat(22)),
        ),
        // Inside the length, modelled prefix, characters outside `[a-z0-9]`.
        (
            "uppercase characters outside the pattern's class",
            format!("dom-{}", "A".repeat(18)),
        ),
        (
            "a separator outside the pattern's class",
            format!("hai-{}_{}", "c".repeat(8), "9".repeat(8)),
        ),
        // Fails the length AND the pattern at once, so the refusal carries two violations
        // of the one member.
        (
            "a bare token, failing both constraints",
            "nope".to_string(),
        ),
    ]
}

#[tokio::test]
async fn a_domain_id_the_service_cannot_read_is_refused_with_its_own_sentence() {
    let emulator = Emulator::new(REGION);

    // The positive control, the same one the corpus recorded: a domain id this service
    // minted, under which a subscription is created and then listed.
    let minted = string_at(
        &emulator
            .create_domain(&json!({ "name": "clinic-domain-id-refusal" }))
            .await,
        "domainId",
    );
    let created = emulator.create_subscription(&minted).await;
    let (status, listed) = emulator
        .send("GET", &format!("/domains/{minted}/subscriptions"), None)
        .await;
    assert_eq!(
        status, 200,
        "the control ListSubscriptions must be served, got {status} with {listed}"
    );
    let subscriptions = listed
        .get("subscriptions")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("ListSubscriptions must report a list, got {listed}"));
    assert_eq!(
        subscriptions.len(),
        1,
        "the control must list the one subscription {created:?}, got {listed}"
    );

    for (case, bad_id) in &unreadable_domain_ids() {
        // The sentence's opening is the whole requirement: the value as supplied, the fixed
        // sentence, and the tail's opening.
        let expected_opening = format!("{bad_id} {NOT_A_VALID_DOMAIN_ID}{DIAGNOSTIC_TAIL_OPENING}");
        for (operation, method, path, body) in requests_carrying(bad_id) {
            let case = format!("{operation}: {case} ({bad_id})");
            let (status, refused) = emulator.send(method, &path, body.as_ref()).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (400, VALIDATION_CODE),
                "case {case:?}: expected 400 {VALIDATION_CODE}, got {status} with {refused}"
            );
            let text = detail_text(&case, &refused, MODEL_MEMBER_DETAIL);
            assert!(
                text.starts_with(&expected_opening),
                "case {case:?}: expected a message starting {expected_opening:?}, got {text:?}"
            );
            assert!(
                text.starts_with(&format!(
                    "{bad_id} {NOT_A_VALID_DOMAIN_ID}{DIAGNOSTIC_TAIL_FIXED}"
                )),
                "case {case:?}: the envelope's fixed half must be stated, got {text:?}"
            );
            assert!(
                !text.contains(GENERIC_CLAUSE_OPENING),
                "case {case:?}: the generic constraint clause must not be stated, got {text:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_event_stream_operation_states_the_same_sentence() {
    let emulator = Emulator::new(REGION);

    // `StartMedicalScribeListeningSession` binds its `domainId` to a request HEADER and
    // reads its own members (its output is a `@streaming` union, so the generated request
    // walk does not serve it), which is why it is driven byte-for-byte here rather than
    // through the JSON-shaped helper. The rule holds there too: one value, one sentence.
    for (case, bad_id) in &unreadable_domain_ids() {
        let rate = 16_000.to_string();
        let headers = [
            ("content-type", EVENT_STREAM),
            (HEADER_SESSION_ID, WELL_FORMED_STREAM_SESSION_ID),
            (HEADER_DOMAIN_ID, bad_id.as_str()),
            (HEADER_SUBSCRIPTION_ID, WELL_FORMED_SUBSCRIPTION_ID),
            (HEADER_LANGUAGE_CODE, "en-US"),
            (HEADER_SAMPLE_RATE, rate.as_str()),
            (HEADER_MEDIA_ENCODING, "pcm"),
        ];
        let case = format!("StartMedicalScribeListeningSession: {case} ({bad_id})");
        let response = emulator
            .send_bytes(MODELED_METHOD, MODELED_PATH, &headers, Vec::new())
            .await;
        assert_eq!(
            response.status().as_u16(),
            400,
            "case {case:?}: expected 400, got {} with {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
        let refused: Value = serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: a refusal carries a JSON error document ({error}), got {:?}",
                String::from_utf8_lossy(response.body())
            )
        });
        assert_eq!(
            error_code(&refused),
            VALIDATION_CODE,
            "case {case:?}: expected {VALIDATION_CODE}, got {refused}"
        );
        // Answered by the operation itself rather than by the boundary walk, and by the
        // Medical Scribe streaming plane, which keys the text as the model names the
        // member.
        let text = detail_text(&case, &refused, MODEL_MEMBER_DETAIL);
        let expected_opening = format!("{bad_id} {NOT_A_VALID_DOMAIN_ID}{DIAGNOSTIC_TAIL_OPENING}");
        assert!(
            text.starts_with(&expected_opening),
            "case {case:?}: expected a message starting {expected_opening:?}, got {text:?}"
        );
    }
}

#[tokio::test]
async fn the_sentence_reaches_no_other_members_refusal() {
    let emulator = Emulator::new(REGION);
    let minted = string_at(
        &emulator
            .create_domain(&json!({ "name": "clinic-other-members" }))
            .await,
        "domainId",
    );
    let bad_domain_id = format!("xxx-{}", "0".repeat(17));

    // One row per refusal the domain-id sentence must NOT reach: another member's value
    // outside its own bound (with a domain id this service minted), and a bad domain id
    // alongside another bad member, where one sentence could only name one of them. The
    // third element is the member the row's answer reports its text in: the boundary walk
    // and the domain plane spell it as the model names the member, while the page size is a
    // bound the SUBSCRIPTION plane's handler applies and renders itself
    // (`src/ops/types/list_subscriptions_types.rs`).
    let rows: Vec<(&str, &str, String, &str)> = vec![
        (
            "a subscriptionId outside its own pattern",
            "GET",
            format!("/domains/{minted}/subscriptions/sub-not-a-subscription-id"),
            MODEL_MEMBER_DETAIL,
        ),
        (
            "a maxResults above its own range",
            "GET",
            format!("/domains/{minted}/subscriptions?maxResults=101"),
            SUBSCRIPTION_PLANE_DETAIL,
        ),
        (
            "a bad domain id and a bad subscription id at once",
            "GET",
            format!("/domains/{bad_domain_id}/subscriptions/sub-not-a-subscription-id"),
            MODEL_MEMBER_DETAIL,
        ),
    ];

    for (case, method, path, member) in &rows {
        let (status, refused) = emulator.send(method, path, None).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, VALIDATION_CODE),
            "case {case:?}: expected 400 {VALIDATION_CODE}, got {status} with {refused}"
        );
        let text = detail_text(case, &refused, member);
        assert!(
            text.contains(GENERIC_CLAUSE_OPENING),
            "case {case:?}: expected the framework's own clause, got {text:?}"
        );
        assert!(
            !text.contains(NOT_A_VALID_DOMAIN_ID),
            "case {case:?}: the domain-id sentence must not be stated, got {text:?}"
        );
    }
}

#[tokio::test]
async fn the_diagnostic_tail_is_per_request() {
    let emulator = Emulator::new(REGION);
    let bad_id = format!("xxx-{}", "0".repeat(17));
    let path = format!("/domains/{bad_id}/subscriptions");
    let expected_opening = format!("{bad_id} {NOT_A_VALID_DOMAIN_ID}{DIAGNOSTIC_TAIL_OPENING}");

    // The same request twice: the sentence in front of the tail is identical (it is built
    // from the request), while the tail is not (it names the request being answered), so a
    // client correlating two refusals can tell them apart.
    let mut tails = Vec::new();
    for case in ["the first refusal", "the second refusal"] {
        let (status, refused) = emulator
            .send("POST", &path, Some(&json!({})))
            .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, VALIDATION_CODE),
            "case {case:?}: expected 400 {VALIDATION_CODE}, got {status} with {refused}"
        );
        let text = detail_text(case, &refused, MODEL_MEMBER_DETAIL);
        let tail = text
            .strip_prefix(&expected_opening)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a message starting {expected_opening:?}, got {text:?}")
            })
            .to_string();
        assert!(
            !tail.is_empty() && tail.ends_with(')'),
            "case {case:?}: expected a closed diagnostic tail, got {tail:?}"
        );
        tails.push(tail);
    }
    assert_ne!(
        tails[0], tails[1],
        "case \"two refusals of one value\": expected per-request diagnostics, got {:?}",
        tails[0]
    );
}
