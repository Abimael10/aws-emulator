// SPDX-License-Identifier: CC0-1.0

//! The wire FORM every timestamp this service reports takes, driven over HTTP through the
//! real router so each assertion reads the bytes a client would.
//!
//! The rule under test: a timestamp member goes onto the JSON wire in the form its target
//! timestamp shape declares. Every timestamp member of this model targets
//! `com.amazonaws.agentregistrycontrol#DateTimestamp`, which carries
//! `@timestampFormat("date-time")`
//! (`smithy_query(query="shape", shape="DateTimestamp")`), so each one is an RFC 3339 /
//! ISO 8601 UTC date-time STRING — never the epoch-seconds NUMBER the `restJson1` protocol
//! defaults to, which no SDK bound to this model can parse for such a member.
//!
//! The rule is the protocol's, not one operation's, so the tests assert it over a TABLE
//! spanning what it generalises over: which operation answers, whether the member sits at
//! the top of the answer or inside a list element, how many elements that list has, and
//! which of `createdAt` / `updatedAt` the shape declares. The two member names are the
//! only members of this model that target that shape (`smithy_query(query="shape",
//! shape="GetRegistryResponse,GetRegistryRecordResponse,RegistryRecordSummary,
//! SubmitRegistryRecordForApprovalResponse")`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region in the test's SigV4 credential scope.
const REGION: &str = "us-west-2";

/// A signed-looking request, so the router resolves this service's signing name
/// (`agent-registry`). The signature itself is never checked: the test router runs with
/// authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTAGENTREGISTRY/20260101/us-west-2/agent-registry/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// How long a wait for a transition the clock owes may take before the test gives up.
/// Generous on purpose: the test waits for the CONDITION, so a loaded or slow host only
/// polls more times.
const TRANSITION_DEADLINE: Duration = Duration::from_secs(30);

/// How long the poll loop pauses between two looks at the resource.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The members of this model that carry an instant. Both target `DateTimestamp`; no other
/// member of any shape in this model does.
const TIMESTAMP_MEMBERS: [&str; 2] = ["createdAt", "updatedAt"];

/// One HTTP answer, reduced to what these tests assert on.
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    fn str_field(&self, name: &str) -> &str {
        self.body[name].as_str().unwrap_or_default()
    }
}

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_agentregistrycontrol_agentregistrycontrol::EmulatedService::new(
            engine,
        ),
    );
    router
}

async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Reply {
    let payload = match &body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serializable body")),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://agent-registry-control.{REGION}.amazonaws.com{path}"
        ))
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(payload)
        .expect("a valid request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply { status, body }
}

/// Percent-encode a path segment the way an SDK encodes a non-greedy `@httpLabel`.
fn encode_label(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn id_of(arn: &str) -> &str {
    arn.rsplit('/').next().unwrap_or(arn)
}

async fn create_registry(router: &MultiServiceRouter, body: Value) -> Reply {
    call(router, "POST", "/registries", Some(body)).await
}

async fn get_registry(router: &MultiServiceRouter, identifier: &str) -> Reply {
    call(
        router,
        "GET",
        &format!("/registries/{}", encode_label(identifier)),
        None,
    )
    .await
}

async fn create_record(router: &MultiServiceRouter, registry_id: &str, body: Value) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{}/records", encode_label(registry_id)),
        Some(body),
    )
    .await
}

async fn get_record(router: &MultiServiceRouter, registry_id: &str, record_id: &str) -> Reply {
    call(
        router,
        "GET",
        &format!(
            "/registries/{}/records/{}",
            encode_label(registry_id),
            encode_label(record_id)
        ),
        None,
    )
    .await
}

async fn list_records(router: &MultiServiceRouter, registry_id: &str, body: Value) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{}/records-list", encode_label(registry_id)),
        Some(body),
    )
    .await
}

async fn submit_record(router: &MultiServiceRouter, registry_id: &str, record_id: &str) -> Reply {
    call(
        router,
        "POST",
        &format!(
            "/registries/{}/records/{}/submit-for-approval",
            encode_label(registry_id),
            encode_label(record_id)
        ),
        None,
    )
    .await
}

/// Look at a registry until it reports `wanted`, or give up at the deadline. The wait is on
/// the CONDITION — a transition the clock owes — never on a fixed span.
async fn registry_reaching(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    wanted: &str,
) -> Reply {
    let started = Instant::now();
    loop {
        let reply = get_registry(router, registry_id).await;
        if reply.str_field("status") == wanted {
            return reply;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: the registry never reached {wanted}, last answer {} {:?}",
            reply.status,
            reply.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look at a record until it reports `wanted`, or give up at the deadline.
async fn record_reaching(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    record_id: &str,
    wanted: &str,
) -> Reply {
    let started = Instant::now();
    loop {
        let reply = get_record(router, registry_id, record_id).await;
        if reply.str_field("status") == wanted {
            return reply;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: the record never reached {wanted}, last answer {} {:?}",
            reply.status,
            reply.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create a registry and wait for the creation the clock owes. Returns its registry id.
async fn ready_registry(router: &MultiServiceRouter, case: &str, body: Value) -> String {
    let created = create_registry(router, body).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistry answers 202, got {} {:?}",
        created.status, created.body
    );
    let registry_id = id_of(created.str_field("registryArn")).to_string();
    registry_reaching(router, case, &registry_id, "READY").await;
    registry_id
}

/// An MCP server definition a supported MCP server schema version accepts: the MCP server
/// descriptor's content is validated against that schema, so a record of this type carries a
/// named server with a description and a version.
const MCP_SERVER_DEFINITION: &str =
    r#"{"name":"com.example/checkout","description":"Checkout tools","version":"1.4.2"}"#;

/// A `descriptors` value per `RecordType`, so each row of a record table carries a
/// descriptor of its own shape. Member names are `Descriptors`' own
/// (`smithy_query(query="shape", shape="Descriptors")`).
fn descriptors_for(record_type: &str) -> Value {
    match record_type {
        "MCP" => {
            json!({ "mcpServer": { "data": MCP_SERVER_DEFINITION, "dataSchemaVersion": "2025-06-18" } })
        }
        "AGENT" => {
            json!({ "a2aAgentCard": { "data": "{\"name\":\"checkout\"}", "dataSchemaVersion": "0.2.1" } })
        }
        "SKILL" => json!({ "agentSkillsDefinition": { "data": "{\"skills\":[]}" } }),
        "GATEWAY" => {
            json!({ "http": { "source": { "fromUrl": { "url": "https://example.test/gateway" } } } })
        }
        _ => json!({ "custom": { "data": "{\"anything\":true}" } }),
    }
}

/// Create a record of `record_type` and wait for the creation the clock owes, so it is
/// `DRAFT`. Returns its record id.
async fn draft_record(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    name: &str,
    record_type: &str,
) -> String {
    let created = create_record(
        router,
        registry_id,
        json!({
            "name": name,
            "recordType": record_type,
            "descriptors": descriptors_for(record_type),
        }),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistryRecord answers 202, got {} {:?}",
        created.status, created.body
    );
    let record_id = id_of(created.str_field("recordArn")).to_string();
    record_reaching(router, case, registry_id, &record_id, "DRAFT").await;
    record_id
}

/// Every timestamp member reachable in `value`, each with the JSON path it was found at, so
/// a failure names the member rather than the whole document. A list element's timestamps
/// are reached too: most of this model's instants sit one level down, inside a summary.
fn timestamps_in(value: &Value, path: &str, found: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(members) => {
            for (name, member) in members {
                let member_path = format!("{path}.{name}");
                if TIMESTAMP_MEMBERS.contains(&name.as_str()) {
                    found.push((member_path, member.clone()));
                } else {
                    timestamps_in(member, &member_path, found);
                }
            }
        }
        Value::Array(elements) => {
            for (index, element) in elements.iter().enumerate() {
                timestamps_in(element, &format!("{path}[{index}]"), found);
            }
        }
        _ => {}
    }
}

fn timestamps_of(body: &Value) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    timestamps_in(body, "$", &mut found);
    found.sort_by(|left, right| left.0.cmp(&right.0));
    found
}

/// Whether `text` is the `date-time` form Smithy declares — an RFC 3339 instant in UTC,
/// `YYYY-MM-DDTHH:MM:SS[.fraction]Z`, which is what the format renders. Written out rather
/// than delegated so the SHAPE of the text is asserted and not just that some parser
/// accepted it.
fn is_utc_date_time_text(text: &str) -> bool {
    let Some(civil) = text.strip_suffix('Z') else {
        return false;
    };
    let Some((date, time)) = civil.split_once('T') else {
        return false;
    };
    let date_fields: Vec<&str> = date.split('-').collect();
    if date_fields.len() != 3 {
        return false;
    }
    let date_widths = [4, 2, 2];
    for (field, width) in date_fields.iter().zip(date_widths) {
        if field.len() != width || !field.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
    }
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (time, None),
    };
    let clock_fields: Vec<&str> = clock.split(':').collect();
    if clock_fields.len() != 3 {
        return false;
    }
    for field in &clock_fields {
        if field.len() != 2 || !field.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
    }
    match fraction {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()),
        None => true,
    }
}

/// The instant a reported timestamp names, asserting the rule on the way: the member is a
/// date-time STRING, that string is the UTC date-time form, a client's own date-time parser
/// reads it, and re-rendering what was read gives the text back unchanged.
fn instant_reported_as_date_time(case: &str, path: &str, value: &Value) -> f64 {
    let text = match value {
        Value::String(text) => text.clone(),
        other => panic!(
            "case {case:?}: {path} must be an RFC 3339 date-time string, got {other:?}"
        ),
    };
    assert!(
        is_utc_date_time_text(&text),
        "case {case:?}: {path} must be YYYY-MM-DDTHH:MM:SS[.fraction]Z, got {text:?}"
    );
    let parsed = DateTime::from_str(&text, Format::DateTime);
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(error) => panic!(
            "case {case:?}: {path} must parse as a date-time, got {text:?} ({error})"
        ),
    };
    let rendered = parsed.fmt(Format::DateTime).ok();
    assert_eq!(
        rendered.as_deref(),
        Some(text.as_str()),
        "case {case:?}: {path} must survive a date-time round trip, got {rendered:?} from {text:?}"
    );
    parsed.as_secs_f64()
}

#[tokio::test]
async fn every_timestamp_a_reporting_operation_answers_with_is_a_date_time_string() {
    let router = router();
    let registry_id = ready_registry(
        &router,
        "the registry under test",
        json!({ "name": "timestamp-wire-form", "description": "every instant is a string" }),
    )
    .await;
    // Two records, so a list answer carries more than one element and the rule is asserted
    // on a repeated member and not only on a singleton.
    let mcp_record = draft_record(&router, "MCP record", &registry_id, "wire.mcp", "MCP").await;
    let agent_record =
        draft_record(&router, "AGENT record", &registry_id, "wire-agent", "AGENT").await;

    let got_registry = get_registry(&router, &registry_id).await;
    let got_record = get_record(&router, &registry_id, &mcp_record).await;
    let listed = list_records(&router, &registry_id, json!({})).await;
    let submitted = submit_record(&router, &registry_id, &agent_record).await;

    // Rows differ along the answering operation, the status it answers at, where the
    // timestamps sit (a top-level member, or one per element of a list) and how many its
    // shape declares — so a member silently dropped or rendered as a number fails here.
    let cases: Vec<(&str, &Reply, u16, usize)> = vec![
        ("GetRegistry: createdAt and updatedAt at the top", &got_registry, 200, 2),
        ("GetRegistryRecord: createdAt and updatedAt at the top", &got_record, 200, 2),
        (
            "ListRegistryRecords: two per summary, two summaries",
            &listed,
            200,
            4,
        ),
        (
            "SubmitRegistryRecordForApproval: updatedAt alone",
            &submitted,
            202,
            1,
        ),
    ];

    for (case, reply, expected_status, expected_count) in cases {
        assert_eq!(
            reply.status, expected_status,
            "case {case:?}: the operation answers {expected_status}, got {} {:?}",
            reply.status, reply.body
        );
        let reported = timestamps_of(&reply.body);
        assert_eq!(
            reported.len(),
            expected_count,
            "case {case:?}: {expected_count} timestamp members are reported, got {:?} in {:?}",
            reported.iter().map(|(path, _)| path).collect::<Vec<_>>(),
            reply.body
        );
        for (path, value) in &reported {
            instant_reported_as_date_time(case, path, value);
        }
    }
}

#[tokio::test]
async fn the_instant_a_record_carries_is_one_value_however_it_is_reported() {
    let router = router();
    let registry_id = ready_registry(
        &router,
        "the registry under test",
        json!({ "name": "timestamp-agreement" }),
    )
    .await;

    // Rows differ along the record type and name, so agreement between two operations is
    // not a property of one stored record.
    let cases: Vec<(&str, &str)> = vec![
        ("an MCP record", "MCP"),
        ("an AGENT record", "AGENT"),
        ("a SKILL record", "SKILL"),
        ("a CUSTOM record", "CUSTOM"),
    ];

    for (case, record_type) in cases {
        let name = format!("agree-{}", record_type.to_ascii_lowercase());
        let record_id = draft_record(&router, case, &registry_id, &name, record_type).await;

        let got = get_record(&router, &registry_id, &record_id).await;
        let created_at = got.body["createdAt"].clone();
        let updated_at = got.body["updatedAt"].clone();
        let created = instant_reported_as_date_time(case, "$.createdAt", &created_at);
        let updated = instant_reported_as_date_time(case, "$.updatedAt", &updated_at);
        assert!(
            updated >= created,
            "case {case:?}: the update the clock owed is not before creation, got createdAt={created_at:?} updatedAt={updated_at:?}"
        );

        // The listing reports the SAME text for the same stored instant: the form is the
        // protocol's, so two operations cannot render one instant differently.
        let listed = list_records(&router, &registry_id, json!({})).await;
        let summary = listed.body["registryRecords"]
            .as_array()
            .and_then(|summaries| {
                summaries
                    .iter()
                    .find(|summary| summary["recordId"] == json!(record_id))
                    .cloned()
            });
        let summary = match summary {
            Some(summary) => summary,
            None => panic!(
                "case {case:?}: ListRegistryRecords must report the record just created, got {:?}",
                listed.body
            ),
        };
        assert_eq!(
            summary["createdAt"], created_at,
            "case {case:?}: the summary reports the same createdAt, got {:?} and {:?}",
            summary["createdAt"], created_at
        );

        // Reading again reports the same text: the rendering is of stored state, not of the
        // clock at read time.
        let again = get_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            again.body["createdAt"], created_at,
            "case {case:?}: a second read reports the same createdAt, got {:?} and {:?}",
            again.body["createdAt"], created_at
        );
    }
}

#[tokio::test]
async fn an_update_the_service_makes_moves_updated_at_and_keeps_the_date_time_form() {
    let router = router();
    let case = "a submitted record";
    let registry_id = ready_registry(
        &router,
        case,
        json!({
            "name": "timestamp-progress",
            "approvalConfiguration": { "autoApprovalRules": ["APPROVE_ALL"] }
        }),
    )
    .await;
    let record_id = draft_record(&router, case, &registry_id, "progress.record", "MCP").await;

    let before = get_record(&router, &registry_id, &record_id).await;
    let before_updated =
        instant_reported_as_date_time(case, "$.updatedAt before the submit", &before.body["updatedAt"]);

    let submitted = submit_record(&router, &registry_id, &record_id).await;
    assert_eq!(
        submitted.status, 202,
        "case {case:?}: SubmitRegistryRecordForApproval answers 202, got {} {:?}",
        submitted.status, submitted.body
    );
    let submitted_updated = instant_reported_as_date_time(
        case,
        "$.updatedAt on the submit answer",
        &submitted.body["updatedAt"],
    );
    assert!(
        submitted_updated >= before_updated,
        "case {case:?}: the submit's updatedAt is not before the one it replaced, got {:?} after {:?}",
        submitted.body["updatedAt"],
        before.body["updatedAt"]
    );

    // The instant the submit reported is the one now stored, in the same form.
    let after = get_record(&router, &registry_id, &record_id).await;
    assert_eq!(
        after.body["updatedAt"], submitted.body["updatedAt"],
        "case {case:?}: GetRegistryRecord reports the instant the submit stored, got {:?} and {:?}",
        after.body["updatedAt"], submitted.body["updatedAt"]
    );
    instant_reported_as_date_time(case, "$.updatedAt after the submit", &after.body["updatedAt"]);
}
