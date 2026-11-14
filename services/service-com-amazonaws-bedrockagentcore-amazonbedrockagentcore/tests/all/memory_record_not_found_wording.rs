// SPDX-License-Identifier: CC0-1.0

//! How this service REFUSES a memory-record read or delete that addresses no record.
//!
//! THE RULE: a `GetMemoryRecord` or `DeleteMemoryRecord` whose `memoryRecordId` is not one
//! the addressed memory container holds is refused `ResourceNotFoundException`
//! (`@httpError 404`, one `message` member — `smithy_query(query="shape",
//! shape="ResourceNotFoundException")`) with ONE fixed sentence that names the resource the
//! API exposes, the memory record, and has no variable part at all: no record id, no memory
//! id and no namespace the request spelled appears in it. It is a property of the RESOURCE,
//! so the sentence is the same whichever of the two operations refuses, however the record
//! came not to be held, and whatever the request spelled.
//!
//! The rows span what the rule generalises over:
//!
//! * which OPERATION refuses (`GetMemoryRecord`, on `GET
//!   /memories/{memoryId}/memoryRecord/{memoryRecordId}`, and `DeleteMemoryRecord`, on
//!   `DELETE /memories/{memoryId}/memoryRecords/{memoryRecordId}`);
//! * WHY the record is not held: an id no batch ever minted, an id a `DeleteMemoryRecord`
//!   removed (read again, and deleted a second time), and an id that IS held — by a
//!   different memory container;
//! * the FORM the request spelled its identifiers in: `MemoryId`'s pattern admits the bare
//!   id and the `arn:…:memory/<id>` spelling, `MemoryRecordId`'s admits 40 to 50 characters
//!   over `mem-[a-zA-Z0-9-_]*`, and the optional `namespace` query may be present or absent.
//!
//! Every case drives real HTTP through the same `MultiServiceRouter` the emulator serves
//! with, so the routes, status codes and `restJson1` error bodies are exercised and not
//! just the handler functions.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use service_com_amazonaws_bedrockagentcore_amazonbedrockagentcore as agentcore;

use super::harness::{create_memory, label};

/// The error code every refusal under test carries.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The whole sentence AWS refuses a memory-record lookup that finds nothing with. Fixed
/// text: it carries no part the request or the store supplies.
const NOT_FOUND_MESSAGE: &str = "The memory record was not found";

/// A memory id matching `MemoryId`'s pattern, which this suite stores a record in.
const MEMORY: &str = "record-memory-0123456789";
/// The same container addressed by the ARN spelling that same pattern admits.
const MEMORY_ARN: &str =
    "arn:aws:bedrock-agentcore:us-east-1:123456789012:memory/record-memory-0123456789";
/// A second container, which holds a record of its own.
const OTHER_MEMORY: &str = "other-memory-9876543210";

/// The namespace the fixture records are written under, and the one a refusal under test
/// may carry on the optional `namespace` query (`Namespace`, pattern
/// `^[a-zA-Z0-9/*][a-zA-Z0-9-_/*]*(?::[a-zA-Z0-9-_/*]+)*[a-zA-Z0-9-_/*]*$`).
const NAMESPACE: &str = "support/facts/actor-1";

/// The emulator, with request authentication off so a test signs nothing. Both memory
/// containers are created first, through the shared inventory the CONTROL plane's
/// `CreateMemory` writes: a write into a container that was never created is refused.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    create_memory(&engine, MEMORY);
    create_memory(&engine, OTHER_MEMORY);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(agentcore::EmulatedService::new(engine));
    router
}

/// Send one request and return its status and parsed JSON body (`Value::Null` for an empty
/// body). The SigV4 credential scope is what routes the request to this service.
async fn send(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(bytes::Bytes::new, |v| bytes::Bytes::from(v.to_string()));
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://bedrock-agentcore.us-east-1.amazonaws.com{path}"))
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/bedrock-agentcore/aws4_request, \
             SignedHeaders=host, Signature=0",
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

fn error_code(body: &Value) -> &str {
    body.get("__type").and_then(Value::as_str).unwrap_or("")
}

fn message(body: &Value) -> &str {
    body.get("message").and_then(Value::as_str).unwrap_or("")
}

/// Which of the two operations a row sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Get,
    Delete,
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Self::Get => "GetMemoryRecord",
            Self::Delete => "DeleteMemoryRecord",
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Delete => "DELETE",
        }
    }

    /// The modelled URI of each operation: the READ is bound to the singular
    /// `memoryRecord` segment and the DELETE to the plural `memoryRecords` one (source:
    /// smithy_query query="operation" operation="GetMemoryRecord,DeleteMemoryRecord").
    fn segment(self) -> &'static str {
        match self {
            Self::Get => "memoryRecord",
            Self::Delete => "memoryRecords",
        }
    }
}

/// One addressed call: the operation, the identifiers it spells, and whether it carries
/// the optional `namespace` query.
#[derive(Debug, Clone)]
struct Call {
    op: Op,
    memory: String,
    record: String,
    namespace: Option<String>,
}

impl Call {
    fn path(&self) -> String {
        let mut path = format!(
            "/memories/{}/{}/{}",
            label(&self.memory),
            self.op.segment(),
            label(&self.record)
        );
        if let Some(namespace) = &self.namespace {
            path.push_str(&format!("?namespace={}", label(namespace)));
        }
        path
    }

    /// Every identifier this call names, none of which the refusal may carry.
    fn identifiers(&self) -> Vec<String> {
        let mut named = vec![self.memory.clone(), self.record.clone()];
        named.extend(self.namespace.clone());
        named
    }

    async fn send(&self, router: &MultiServiceRouter) -> (u16, Value) {
        send(router, self.op.method(), &self.path(), None).await
    }
}

/// `POST /memories/{memoryId}/memoryRecords/batchCreate` with one entry, answering with
/// the `memoryRecordId` the server minted for it.
async fn create_one_record(
    router: &MultiServiceRouter,
    memory: &str,
    request_identifier: &str,
) -> String {
    let (status, created) = send(
        router,
        "POST",
        &format!("/memories/{}/memoryRecords/batchCreate", label(memory)),
        Some(json!({
            "records": [{
                "requestIdentifier": request_identifier,
                "namespaces": [NAMESPACE],
                "content": {"text": "the user prefers window seats"},
                "timestamp": 1_700_000_000,
            }],
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "the fixture record must be created in {memory:?}, got {status} with {created:?}"
    );
    created["successfulRecords"][0]["memoryRecordId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A well-formed `MemoryRecordId` that no batch ever minted: `mem-` followed by `filler`
/// repeated to `length` characters in all (`MemoryRecordId`, length 40..50, pattern
/// `^mem-[a-zA-Z0-9-_]*$` — source: smithy_query query="shape" shape="MemoryRecordId").
fn unminted_record_id(filler: &str, length: usize) -> String {
    let mut id = String::from("mem-");
    while id.len() < length {
        id.push_str(filler);
    }
    id.truncate(length);
    id
}

/// Every memory-record read or delete that addresses no held record is refused 404
/// `ResourceNotFoundException` with the one fixed sentence, naming nothing the request
/// spelled — whichever operation refuses, however the record came not to be held, and in
/// whatever form the identifiers arrived.
#[tokio::test]
async fn a_memory_record_lookup_that_misses_states_one_fixed_sentence() {
    let router = emulator();
    let deleted = create_one_record(&router, MEMORY, "req-doomed").await;
    let surviving = create_one_record(&router, MEMORY, "req-kept").await;
    let elsewhere = create_one_record(&router, OTHER_MEMORY, "req-elsewhere").await;

    let (status, removed) = send(
        &router,
        "DELETE",
        &format!(
            "/memories/{}/memoryRecords/{}",
            label(MEMORY),
            label(&deleted)
        ),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture delete must succeed, got {status} with {removed:?}"
    );

    // `mem-` + 36 fill characters is the shortest id the model admits, `mem-` + 46 the
    // longest; the third row mixes the `-` and `_` the expression also admits.
    let shortest = unminted_record_id("0", 40);
    let longest = unminted_record_id("a", 50);
    let punctuated = unminted_record_id("A1-_", 40);

    let cases: Vec<(&str, Call)> = vec![
        (
            "get-an-id-no-batch-ever-minted",
            Call {
                op: Op::Get,
                memory: MEMORY.to_string(),
                record: shortest.clone(),
                namespace: None,
            },
        ),
        (
            "get-an-unminted-id-at-the-length-maximum",
            Call {
                op: Op::Get,
                memory: MEMORY.to_string(),
                record: longest.clone(),
                namespace: None,
            },
        ),
        (
            "get-an-unminted-id-carrying-dashes-and-underscores",
            Call {
                op: Op::Get,
                memory: MEMORY.to_string(),
                record: punctuated.clone(),
                namespace: None,
            },
        ),
        (
            "get-a-deleted-id",
            Call {
                op: Op::Get,
                memory: MEMORY.to_string(),
                record: deleted.clone(),
                namespace: None,
            },
        ),
        (
            "get-a-deleted-id-through-the-container-arn",
            Call {
                op: Op::Get,
                memory: MEMORY_ARN.to_string(),
                record: deleted.clone(),
                namespace: None,
            },
        ),
        (
            "get-a-deleted-id-with-the-namespace-query",
            Call {
                op: Op::Get,
                memory: MEMORY.to_string(),
                record: deleted.clone(),
                namespace: Some(NAMESPACE.to_string()),
            },
        ),
        (
            "get-a-record-held-by-another-container",
            Call {
                op: Op::Get,
                memory: MEMORY.to_string(),
                record: elsewhere.clone(),
                namespace: None,
            },
        ),
        (
            "delete-an-id-no-batch-ever-minted",
            Call {
                op: Op::Delete,
                memory: MEMORY.to_string(),
                record: longest.clone(),
                namespace: None,
            },
        ),
        (
            "delete-a-deleted-id-again",
            Call {
                op: Op::Delete,
                memory: MEMORY.to_string(),
                record: deleted.clone(),
                namespace: None,
            },
        ),
        (
            "delete-a-deleted-id-again-through-the-container-arn",
            Call {
                op: Op::Delete,
                memory: MEMORY_ARN.to_string(),
                record: deleted.clone(),
                namespace: None,
            },
        ),
        (
            "delete-an-unminted-id-with-the-namespace-query",
            Call {
                op: Op::Delete,
                memory: MEMORY.to_string(),
                record: punctuated.clone(),
                namespace: Some(NAMESPACE.to_string()),
            },
        ),
        (
            "delete-a-record-held-by-another-container",
            Call {
                op: Op::Delete,
                memory: MEMORY.to_string(),
                record: elsewhere.clone(),
                namespace: None,
            },
        ),
    ];

    for (case, call) in cases {
        let operation = call.op.name();
        let (status, body) = call.send(&router).await;
        assert_eq!(
            status, 404,
            "case {case:?} on {operation}: expected HTTP 404, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            RESOURCE_NOT_FOUND,
            "case {case:?} on {operation}: expected {RESOURCE_NOT_FOUND}, got {body:?}"
        );
        assert_eq!(
            message(&body),
            NOT_FOUND_MESSAGE,
            "case {case:?} on {operation}: expected message {NOT_FOUND_MESSAGE:?}, got {:?}",
            message(&body)
        );
        // The sentence has no variable part, so nothing the request spelled is in it.
        for named in call.identifiers() {
            assert!(
                !message(&body).contains(&named),
                "case {case:?} on {operation}: the message must not name {named:?}, got {:?}",
                message(&body)
            );
        }
    }

    // None of the refusals above removed anything: the surviving record of this container
    // still reads back, and the record of the other container still reads back there.
    for (case, memory, record) in [
        ("the surviving record of this container", MEMORY, &surviving),
        ("the record of the other container", OTHER_MEMORY, &elsewhere),
    ] {
        let (status, body) = Call {
            op: Op::Get,
            memory: memory.to_string(),
            record: record.clone(),
            namespace: None,
        }
        .send(&router)
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected it to still read back, got {status} with {body:?}"
        );
        assert_eq!(
            body["memoryRecord"]["memoryRecordId"].as_str(),
            Some(record.as_str()),
            "case {case:?}: expected GetMemoryRecord to report {record:?}, got {body:?}"
        );
    }
}
