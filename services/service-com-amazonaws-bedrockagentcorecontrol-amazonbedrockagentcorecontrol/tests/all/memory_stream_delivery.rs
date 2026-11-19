// SPDX-License-Identifier: CC0-1.0

//! The data stream a memory's `streamDeliveryResources` names is RESOLVED while the create
//! or the update is served, driven end to end over the wire path with Kinesis mounted beside
//! this control plane on ONE engine — what the assembled emulator is — so the stream a
//! request resolves is one Kinesis really holds.
//!
//! THE RULE UNDER TEST: a memory configured for stream delivery writes its records INTO the
//! stream the configuration names, so that stream is part of what the create persists rather
//! than a string it echoes back. A `CreateMemory` (or `UpdateMemory`) whose
//! `streamDeliveryResources.resources[].kinesis.dataStreamArn` names a stream this account
//! does not hold is refused with the operation's modelled `ValidationException` (HTTP 400)
//! stating that the memory execution role cannot reach the stream, and no memory is created
//! — nor is a stored one changed; one naming a stream Kinesis DOES hold is accepted, and the
//! configuration reads back.
//!
//! The stream is resolved IN-PROCESS, against the Kinesis store of the ONE engine every
//! service of this emulator shares — the store `kinesis:CreateStream` writes — so each row
//! creates the stream it expects a request to find through Kinesis's own front door, and a
//! row about a stream that is not there creates none.
//!
//! The rows span what the rule generalises over: whether the stream exists, the Region its
//! ARN names (the signed one and another), the content level configured, and whether the
//! request is a create or an update.
//!
//! Sources: `smithy_query(query="shape",shape="StreamDeliveryResources,StreamDeliveryResource,\
//! KinesisResource,ContentConfiguration,ContentType,ContentLevel",model_path="services/\
//! service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/smithy-model/\
//! com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol.json")` for the
//! configuration's members and enum values.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as agentcore;

/// The Region every control-plane request is signed for, and a second one for the row whose
/// stream lives behind another Region's endpoint.
const REGION: &str = "us-east-1";
const OTHER_REGION: &str = "eu-west-3";

/// `ContentType`'s only value, and both `ContentLevel` values.
const MEMORY_RECORDS: &str = "MEMORY_RECORDS";
const METADATA_ONLY: &str = "METADATA_ONLY";
const FULL_CONTENT: &str = "FULL_CONTENT";

/// The whole sentence AWS refuses a stream-delivery configuration it cannot reach with. It
/// carries no variable part, so a row cannot pass by echoing back the ARN it sent.
const INACCESSIBLE: &str = "Memory execution role does not have access to provided Kinesis \
                            stream. If the stream is KMS-encrypted, ensure the role has \
                            kms:GenerateDataKey and kms:Decrypt permissions on the KMS key.";

/// `eventExpiryDuration` inside its modelled range (3..365).
const EVENT_EXPIRY_DAYS: i64 = 30;

/// One answer: the status, the error code and the parsed body.
struct Answer {
    status: u16,
    code: String,
    body: Value,
}

/// This emulator: the control plane, and Kinesis beside it when the row wants one, over the
/// one shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    /// The assembly the router performs: both services on one engine.
    fn with_kinesis() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::default();
        router.register_service(agentcore::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        Self { router }
    }

    /// One `restJson1` call to the control plane, signed for [`REGION`].
    async fn control(&self, method: &str, uri: &str, body: Option<&Value>) -> Answer {
        let payload = body.map_or_else(Bytes::new, |value| {
            Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
        });
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
             aws4_request, SignedHeaders=host, Signature=abc"
        );
        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", authorization)
            .header("host", "bedrock-agentcore-control.amazonaws.com");
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let request = request.body(payload).expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        Answer {
            status: response.status().as_u16(),
            code: response
                .headers()
                .get("x-amzn-errortype")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string(),
            body: if response.body().is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(response.body()).unwrap_or(Value::Null)
            },
        }
    }

    /// Create a stream through Kinesis's OWN front door (`awsJson1_1`,
    /// `Kinesis_20131202.CreateStream`) in `region`, and answer the ARN it is addressed by.
    async fn given_stream(&self, case: &str, region: &str, name: &str) -> String {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", "Kinesis_20131202.CreateStream")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/kinesis/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=abc"
                ),
            )
            .body(Bytes::from(
                json!({"StreamName": name, "ShardCount": 1}).to_string(),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "case {case:?}: setup CreateStream must succeed, got {} with {}",
            response.status().as_u16(),
            String::from_utf8_lossy(response.body())
        );
        stream_arn(region, name)
    }

    async fn create_memory(&self, body: &Value) -> Answer {
        self.control("POST", "/memories/create", Some(body)).await
    }

    async fn get_memory(&self, id: &str) -> Answer {
        self.control("GET", &format!("/memories/{id}/details"), None)
            .await
    }

    /// Read the memory until it reports `ACTIVE` — the transition the clock owes a create —
    /// or fail once the generous deadline passes. The loop looks for the CONDITION rather
    /// than waiting a fixed time, so nothing here depends on how fast the host runs.
    async fn read_until_active(&self, case: &str, id: &str) -> Answer {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let answer = self.get_memory(id).await;
            if answer.status == 200 && answer.body["memory"]["status"] == json!("ACTIVE") {
                return answer;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: the memory never reached ACTIVE, last answer {} with {}",
                answer.status,
                answer.body
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    async fn update_memory(&self, id: &str, body: &Value) -> Answer {
        self.control("PUT", &format!("/memories/{id}/update"), Some(body))
            .await
    }

    /// The names of every memory the control plane reports.
    async fn listed_memory_names(&self, case: &str) -> Vec<String> {
        let answer = self.control("POST", "/memories/", Some(&json!({}))).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200 from ListMemories, got {} with {}",
            answer.status, answer.body
        );
        answer.body["memories"]
            .as_array()
            .map(|memories| {
                memories
                    .iter()
                    .filter_map(|memory| memory["id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The ARN a Kinesis data stream of this account is addressed by in `region`.
fn stream_arn(region: &str, name: &str) -> String {
    format!(
        "arn:aws:kinesis:{region}:{}:stream/{name}",
        ::engine::emulator_account_id()
    )
}

/// A `streamDeliveryResources` naming `arn` at `level`.
fn delivery(arn: &str, level: &str) -> Value {
    json!({"resources": [{"kinesis": {
        "dataStreamArn": arn,
        "contentConfigurations": [{"type": MEMORY_RECORDS, "level": level}],
    }}]})
}

/// A `CreateMemory` body for `name`, carrying `delivery` when the row configures one.
fn create_body(name: &str, delivery: Option<Value>) -> Value {
    let mut body = json!({
        "name": name,
        "eventExpiryDuration": EVENT_EXPIRY_DAYS,
    });
    if let Some(delivery) = delivery {
        body["streamDeliveryResources"] = delivery;
    }
    body
}

/// One row: whether the stream exists, which Region its ARN names, and the content level.
struct Row {
    case: &'static str,
    memory: &'static str,
    /// The stream to create first, when the row's stream is one this account holds.
    stream: Option<(&'static str, &'static str)>,
    /// The stream name the memory's configuration names.
    named: &'static str,
    /// The Region the named stream's ARN carries.
    region: &'static str,
    level: &'static str,
}

/// RULE: a create whose stream-delivery configuration names a stream Kinesis holds is
/// accepted and persists it; one naming a stream Kinesis reports absent is refused 400 with
/// the sentence AWS words that refusal in, and no memory is created.
#[tokio::test]
async fn a_create_resolves_the_stream_its_delivery_configuration_names() {
    let rows = [
        Row {
            case: "a stream this account holds, metadata only",
            memory: "streamingMemory",
            stream: Some((REGION, "sim2real-delivery-stream")),
            named: "sim2real-delivery-stream",
            region: REGION,
            level: METADATA_ONLY,
        },
        Row {
            case: "a stream this account holds in another Region, full content",
            memory: "crossRegionMemory",
            stream: Some((OTHER_REGION, "sim2real-elsewhere-stream")),
            named: "sim2real-elsewhere-stream",
            region: OTHER_REGION,
            level: FULL_CONTENT,
        },
        Row {
            case: "a well-formed ARN for a stream that was never created",
            memory: "missingStreamMemory",
            stream: None,
            named: "sim2real-missing-stream",
            region: REGION,
            level: METADATA_ONLY,
        },
        Row {
            case: "a stream of the signed Region named in another Region's ARN",
            memory: "wrongRegionMemory",
            stream: Some((REGION, "sim2real-only-here-stream")),
            named: "sim2real-only-here-stream",
            region: OTHER_REGION,
            level: FULL_CONTENT,
        },
        Row {
            case: "another name entirely, beside a stream that does exist",
            memory: "typoedStreamMemory",
            stream: Some((REGION, "sim2real-real-stream")),
            named: "sim2real-reel-stream",
            region: REGION,
            level: METADATA_ONLY,
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = Emulator::with_kinesis();
        if let Some((region, name)) = row.stream {
            emulator.given_stream(case, region, name).await;
        }
        let holds_the_named_stream = row.stream == Some((row.region, row.named));
        let arn = stream_arn(row.region, row.named);
        let configuration = delivery(&arn, row.level);
        let answer = emulator
            .create_memory(&create_body(row.memory, Some(configuration.clone())))
            .await;

        if holds_the_named_stream {
            assert_eq!(
                answer.status, 202,
                "case {case:?}: a create naming a stream Kinesis holds is accepted, got {} {:?} \
                 with {}",
                answer.status, answer.code, answer.body
            );
            let id = answer.body["memory"]["id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: the create reports an id, got {}", answer.body))
                .to_string();
            // The round trip: the stored memory reports the configuration it persisted.
            let read = emulator.get_memory(&id).await;
            assert_eq!(
                (read.status, read.body["memory"]["streamDeliveryResources"].clone()),
                (200, configuration),
                "case {case:?}: the read reports the configuration the create persisted, got {} \
                 with {}",
                read.status,
                read.body
            );
            continue;
        }

        assert_eq!(
            (answer.status, answer.code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: a create naming a stream Kinesis does not hold is the modelled 400, \
             got {} {:?} with {}",
            answer.status,
            answer.code,
            answer.body
        );
        assert_eq!(
            answer.body["message"], json!(INACCESSIBLE),
            "case {case:?}: expected {INACCESSIBLE:?}, got {}",
            answer.body
        );
        // Nothing was created: no memory is listed, so the refusal came BEFORE the write.
        let listed = emulator.listed_memory_names(case).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: a refused create materialises no memory, got {listed:?}"
        );
    }
}

/// RULE: the same resolution guards an UPDATE, and a refused update leaves the stored
/// configuration exactly as it was.
#[tokio::test]
async fn an_update_resolves_the_stream_it_would_store_and_changes_nothing_when_it_cannot() {
    let case = "an update pointing the memory at a stream that is not there";
    let emulator = Emulator::with_kinesis();
    let held = emulator
        .given_stream(case, REGION, "sim2real-update-stream")
        .await;
    let stored = delivery(&held, METADATA_ONLY);
    let created = emulator
        .create_memory(&create_body("updatedStreamMemory", Some(stored.clone())))
        .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: setup CreateMemory must succeed, got {} with {}",
        created.status, created.body
    );
    let id = created.body["memory"]["id"]
        .as_str()
        .expect("the create reports an id")
        .to_string();
    // The create is accepted with the memory `CREATING`; an update is served once the
    // transition the clock owes it is due, so wait for that before asking for a change.
    emulator.read_until_active(case, &id).await;

    let refused = emulator
        .update_memory(
            &id,
            &json!({
                "streamDeliveryResources": delivery(
                    &stream_arn(REGION, "sim2real-update-absent-stream"),
                    FULL_CONTENT,
                ),
            }),
        )
        .await;
    assert_eq!(
        (refused.status, refused.code.as_str()),
        (400, "ValidationException"),
        "case {case:?}: the update is the modelled 400, got {} {:?} with {}",
        refused.status,
        refused.code,
        refused.body
    );
    assert_eq!(
        refused.body["message"], json!(INACCESSIBLE),
        "case {case:?}: expected {INACCESSIBLE:?}, got {}",
        refused.body
    );
    let read = emulator.get_memory(&id).await;
    assert_eq!(
        (read.status, read.body["memory"]["streamDeliveryResources"].clone()),
        (200, stored),
        "case {case:?}: the refused update leaves the stored configuration as it was, got {} \
         with {}",
        read.status,
        read.body
    );

    // And an update naming a stream Kinesis DOES hold is served.
    let second = emulator
        .given_stream(case, REGION, "sim2real-update-second-stream")
        .await;
    let replacement = delivery(&second, FULL_CONTENT);
    let served = emulator
        .update_memory(&id, &json!({"streamDeliveryResources": replacement.clone()}))
        .await;
    assert_eq!(
        served.status, 202,
        "case {case:?}: an update naming a stream Kinesis holds is accepted, got {} with {}",
        served.status, served.body
    );
    let read = emulator.get_memory(&id).await;
    assert_eq!(
        (read.status, read.body["memory"]["streamDeliveryResources"].clone()),
        (200, replacement),
        "case {case:?}: the read reports the configuration the update persisted, got {} with {}",
        read.status,
        read.body
    );
}

/// RULE: a create that configures NO stream delivery has no dependency to resolve, so it is
/// served whether or not any stream exists in the account.
#[tokio::test]
async fn a_create_without_stream_delivery_resolves_nothing() {
    for (case, seeded) in [
        ("no stream in the account at all", None),
        (
            "a stream nothing references",
            Some("sim2real-unreferenced-stream"),
        ),
    ] {
        let emulator = Emulator::with_kinesis();
        if let Some(stream) = seeded {
            emulator.given_stream(case, REGION, stream).await;
        }
        let answer = emulator
            .create_memory(&create_body("plainMemory", None))
            .await;
        assert_eq!(
            answer.status, 202,
            "case {case:?}: a create configuring no delivery is served, got {} {:?} with {}",
            answer.status, answer.code, answer.body
        );
        assert!(
            answer.body["memory"]
                .get("streamDeliveryResources")
                .is_none(),
            "case {case:?}: a memory configured with no delivery reports none, got {}",
            answer.body
        );
    }
}
