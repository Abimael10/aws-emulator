// SPDX-License-Identifier: CC0-1.0

//! WHICH LEVEL a memory-record read or delete reports missing.
//!
//! THE RULE: `GetMemoryRecord` and `DeleteMemoryRecord` address a record INSIDE a memory
//! container, and the container is created through the CONTROL plane, so the container is
//! resolved first — against the component that owns it — and the refusal is worded at the
//! level that failed:
//!
//! * a `memoryId` the account holds no container for is refused at the CONTAINER level,
//!   with the sentence the memory-records API words that level with;
//! * a `memoryRecordId` a container that DOES exist does not hold is refused at the RECORD
//!   level, with the record sentence (pinned by the sibling suite
//!   `memory_record_not_found_wording`).
//!
//! Both are `ResourceNotFoundException` (`@httpError 404`, one `message` member — source:
//! smithy_query query="shape" shape="ResourceNotFoundException"), so the message is the
//! only thing that tells the two preconditions apart; both sentences are wholly fixed text
//! and carry no memory id, record id or namespace from the request.
//!
//! The rows span what the rule generalises over:
//!
//! * which OPERATION refuses (`GET /memories/{memoryId}/memoryRecord/{memoryRecordId}` and
//!   `DELETE /memories/{memoryId}/memoryRecords/{memoryRecordId}`);
//! * WHY the container is absent: one nothing ever created, and one the control plane
//!   created and then deleted while this plane still has records indexed under its id;
//! * the FORM the `memoryId` arrived in: `MemoryId`'s pattern admits the bare id and the
//!   `arn:…:memory/<id>` spelling;
//! * the `memoryRecordId` the call spells: ids of either length bound no batch minted, and
//!   one a LIVE container really holds — the container level answers either way;
//! * whether the optional `namespace` query is present.
//!
//! Every case drives real HTTP through the same `MultiServiceRouter` the emulator serves
//! with, so routes, status codes and `restJson1` error bodies are exercised too.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use service_com_amazonaws_bedrockagentcore_amazonbedrockagentcore as agentcore;

use super::harness::{create_memory, delete_memory, label};

/// The error code every refusal under test carries.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The whole sentence a memory-record call is refused with when the `memoryId` it addresses
/// names no memory container. Fixed text: no part of it comes from the request or the store.
const CONTAINER_NOT_FOUND_MESSAGE: &str = "MemoryItem does not exist";

/// The whole sentence a memory-record call is refused with when the container exists and
/// the record does not — the OTHER level, equally fixed.
const RECORD_NOT_FOUND_MESSAGE: &str = "The memory record was not found";

/// A container the control plane created and kept, which holds one record.
const LIVE_MEMORY: &str = "live-memory-0123456789";
/// The same live container addressed by the ARN spelling `MemoryId` also admits.
const LIVE_MEMORY_ARN: &str =
    "arn:aws:bedrock-agentcore:us-east-1:123456789012:memory/live-memory-0123456789";

/// A container the control plane created, which took a record, and which the control plane
/// then deleted: this plane still has that record indexed under the id.
const DELETED_MEMORY: &str = "gone-memory-9876543210";
/// The ARN spelling of that same deleted container.
const DELETED_MEMORY_ARN: &str =
    "arn:aws:bedrock-agentcore:us-east-1:123456789012:memory/gone-memory-9876543210";

/// A well-formed `MemoryId` nothing ever created, and its ARN spelling.
const GHOST_MEMORY: &str = "ghost-memory-1234567890";
const GHOST_MEMORY_ARN: &str =
    "arn:aws:bedrock-agentcore:us-east-1:123456789012:memory/ghost-memory-1234567890";

/// The namespace the fixture records are written under, and the one a refusal under test
/// may carry on the optional `namespace` query (`Namespace`, pattern
/// `^[a-zA-Z0-9/*][a-zA-Z0-9-_/*]*(?::[a-zA-Z0-9-_/*]+)*[a-zA-Z0-9-_/*]*$`).
const NAMESPACE: &str = "support/facts/actor-1";

/// The emulator with request authentication off, plus the two containers this suite starts
/// from: both are created through the shared inventory the CONTROL plane's `CreateMemory`
/// writes, since this data plane's own API creates no container.
fn emulator() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    create_memory(&engine, LIVE_MEMORY);
    create_memory(&engine, DELETED_MEMORY);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(agentcore::EmulatedService::new(Arc::clone(&engine)));
    (engine, router)
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

/// One addressed call: the operation, the identifiers it spells, and whether it carries the
/// optional `namespace` query.
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

    /// Every identifier this call names, none of which either refusal may carry.
    fn identifiers(&self) -> Vec<String> {
        let mut named = vec![self.memory.clone(), self.record.clone()];
        named.extend(self.namespace.clone());
        named
    }

    async fn send(&self, router: &MultiServiceRouter) -> (u16, Value) {
        send(router, self.op.method(), &self.path(), None).await
    }
}

/// `POST /memories/{memoryId}/memoryRecords/batchCreate` with one entry, answering with the
/// `memoryRecordId` the server minted for it.
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

/// Every memory-record read or delete whose `memoryId` names no container the account holds
/// is refused 404 `ResourceNotFoundException` with the CONTAINER sentence — whichever
/// operation refuses, however the container came not to be held, in whichever form the
/// memory id arrived, and whatever record the call names, including one a live container
/// really holds.
#[tokio::test]
async fn a_memory_record_call_on_an_absent_container_states_the_container_sentence() {
    let (engine, router) = emulator();
    let live_record = create_one_record(&router, LIVE_MEMORY, "req-live").await;
    let orphaned_record = create_one_record(&router, DELETED_MEMORY, "req-orphan").await;
    // The control plane deletes the container, while this plane keeps what was written
    // into it: existence is the owning component's answer, not this plane's index.
    delete_memory(&engine, DELETED_MEMORY);

    // `mem-` + 36 fill characters is the shortest id the model admits and `mem-` + 46 the
    // longest; the third mixes the `-` and `_` the expression also admits.
    let shortest = unminted_record_id("0", 40);
    let longest = unminted_record_id("a", 50);
    let punctuated = unminted_record_id("A1-_", 40);

    let cases: Vec<(&str, Call)> = vec![
        (
            "get-in-a-container-nothing-created",
            Call {
                op: Op::Get,
                memory: GHOST_MEMORY.to_string(),
                record: shortest.clone(),
                namespace: None,
            },
        ),
        (
            "get-in-a-container-nothing-created-by-arn",
            Call {
                op: Op::Get,
                memory: GHOST_MEMORY_ARN.to_string(),
                record: longest.clone(),
                namespace: None,
            },
        ),
        (
            "get-in-a-container-nothing-created-with-the-namespace-query",
            Call {
                op: Op::Get,
                memory: GHOST_MEMORY.to_string(),
                record: punctuated.clone(),
                namespace: Some(NAMESPACE.to_string()),
            },
        ),
        (
            "get-a-record-still-indexed-under-a-deleted-container",
            Call {
                op: Op::Get,
                memory: DELETED_MEMORY.to_string(),
                record: orphaned_record.clone(),
                namespace: None,
            },
        ),
        (
            "get-in-a-deleted-container-by-arn",
            Call {
                op: Op::Get,
                memory: DELETED_MEMORY_ARN.to_string(),
                record: orphaned_record.clone(),
                namespace: Some(NAMESPACE.to_string()),
            },
        ),
        (
            "get-a-record-another-live-container-holds",
            Call {
                op: Op::Get,
                memory: GHOST_MEMORY.to_string(),
                record: live_record.clone(),
                namespace: None,
            },
        ),
        (
            "delete-in-a-container-nothing-created",
            Call {
                op: Op::Delete,
                memory: GHOST_MEMORY.to_string(),
                record: longest.clone(),
                namespace: None,
            },
        ),
        (
            "delete-in-a-container-nothing-created-by-arn",
            Call {
                op: Op::Delete,
                memory: GHOST_MEMORY_ARN.to_string(),
                record: shortest.clone(),
                namespace: Some(NAMESPACE.to_string()),
            },
        ),
        (
            "delete-a-record-still-indexed-under-a-deleted-container",
            Call {
                op: Op::Delete,
                memory: DELETED_MEMORY.to_string(),
                record: orphaned_record.clone(),
                namespace: None,
            },
        ),
        (
            "delete-in-a-deleted-container-by-arn",
            Call {
                op: Op::Delete,
                memory: DELETED_MEMORY_ARN.to_string(),
                record: punctuated.clone(),
                namespace: None,
            },
        ),
        (
            "delete-a-record-another-live-container-holds",
            Call {
                op: Op::Delete,
                memory: GHOST_MEMORY.to_string(),
                record: live_record.clone(),
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
            CONTAINER_NOT_FOUND_MESSAGE,
            "case {case:?} on {operation}: expected message {CONTAINER_NOT_FOUND_MESSAGE:?}, \
             got {:?}",
            message(&body)
        );
        // The container sentence has no variable part, so nothing the request spelled is
        // in it.
        for named in call.identifiers() {
            assert!(
                !message(&body).contains(&named),
                "case {case:?} on {operation}: the message must not name {named:?}, got {:?}",
                message(&body)
            );
        }
    }

    // None of those refusals touched the live container: its record still reads back.
    let (status, body) = Call {
        op: Op::Get,
        memory: LIVE_MEMORY.to_string(),
        record: live_record.clone(),
        namespace: None,
    }
    .send(&router)
    .await;
    assert_eq!(
        status, 200,
        "case \"the live container's record survives\": expected HTTP 200, got {status} with \
         {body:?}"
    );
    assert_eq!(
        body["memoryRecord"]["memoryRecordId"].as_str(),
        Some(live_record.as_str()),
        "case \"the live container's record survives\": expected {live_record:?}, got {body:?}"
    );
}

/// The two levels stay distinct: on a container the account DOES hold, the same two
/// operations refuse a record it does not hold with the RECORD sentence, and the record it
/// does hold is served — so the container sentence is reached only by a missing container.
#[tokio::test]
async fn the_record_sentence_answers_only_inside_a_container_that_exists() {
    let (_engine, router) = emulator();
    let held = create_one_record(&router, LIVE_MEMORY, "req-held").await;
    let unminted = unminted_record_id("b", 44);

    assert_ne!(
        CONTAINER_NOT_FOUND_MESSAGE, RECORD_NOT_FOUND_MESSAGE,
        "the two levels must not share one sentence, or nothing on the wire tells them apart"
    );

    // Rows: each operation × each form of the live container's id, on a record the
    // container does not hold.
    for (case, op, memory) in [
        ("get-by-the-bare-id", Op::Get, LIVE_MEMORY),
        ("get-by-the-container-arn", Op::Get, LIVE_MEMORY_ARN),
        ("delete-by-the-bare-id", Op::Delete, LIVE_MEMORY),
        ("delete-by-the-container-arn", Op::Delete, LIVE_MEMORY_ARN),
    ] {
        let call = Call {
            op,
            memory: memory.to_string(),
            record: unminted.clone(),
            namespace: None,
        };
        let operation = op.name();
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
            RECORD_NOT_FOUND_MESSAGE,
            "case {case:?} on {operation}: expected message {RECORD_NOT_FOUND_MESSAGE:?}, got \
             {:?}",
            message(&body)
        );
    }

    // And the record the live container holds is still served, by either spelling of the
    // container id.
    for (case, memory) in [
        ("read-by-the-bare-id", LIVE_MEMORY),
        ("read-by-the-container-arn", LIVE_MEMORY_ARN),
    ] {
        let (status, body) = Call {
            op: Op::Get,
            memory: memory.to_string(),
            record: held.clone(),
            namespace: None,
        }
        .send(&router)
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body:?}"
        );
        assert_eq!(
            body["memoryRecord"]["memoryRecordId"].as_str(),
            Some(held.as_str()),
            "case {case:?}: expected GetMemoryRecord to report {held:?}, got {body:?}"
        );
    }

    // The delete of a held record still succeeds inside a live container, and a second
    // delete of it is the RECORD level again — not the container level.
    let (status, deleted) = Call {
        op: Op::Delete,
        memory: LIVE_MEMORY.to_string(),
        record: held.clone(),
        namespace: None,
    }
    .send(&router)
    .await;
    assert_eq!(
        status, 200,
        "case \"delete the held record\": expected HTTP 200, got {status} with {deleted:?}"
    );
    let (status, body) = Call {
        op: Op::Delete,
        memory: LIVE_MEMORY.to_string(),
        record: held.clone(),
        namespace: None,
    }
    .send(&router)
    .await;
    assert_eq!(
        status, 404,
        "case \"delete the held record twice\": expected HTTP 404, got {status} with {body:?}"
    );
    assert_eq!(
        message(&body),
        RECORD_NOT_FOUND_MESSAGE,
        "case \"delete the held record twice\": expected message {RECORD_NOT_FOUND_MESSAGE:?}, \
         got {:?}",
        message(&body)
    );
}
