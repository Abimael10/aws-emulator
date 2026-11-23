// SPDX-License-Identifier: CC0-1.0

//! WHICH LEVEL a memory-records call reports missing.
//!
//! THE RULE: every operation of the memory-records API addresses something INSIDE a memory
//! container, and the container is created through the CONTROL plane, so the container is
//! resolved first — against the component that owns it — and the refusal is worded at the
//! level that failed:
//!
//! * a `memoryId` the account holds no container for is refused at the CONTAINER level,
//!   with the one sentence the memory-records API words that level with, whichever of its
//!   operations the request reached;
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
//! * which OPERATION refuses — every memory-records operation this emulator implements,
//!   the two that address ONE record by id (`GET /memories/{memoryId}/memoryRecord/…` and
//!   `DELETE /memories/{memoryId}/memoryRecords/…`) and the two that address the
//!   collection (`POST /memories/{memoryId}/memoryRecords` and its `…/batchCreate`
//!   sub-resource), since the container level belongs to the API and not to one handler;
//! * whether the operation only REPORTS or WRITES: the container level is the same gate
//!   either way;
//! * WHY the container is absent: one nothing ever created, and one the control plane
//!   created and then deleted while this plane still has records indexed under its id;
//! * the FORM the `memoryId` arrived in: `MemoryId`'s pattern admits the bare id and the
//!   `arn:…:memory/<id>` spelling;
//! * the `memoryRecordId` the call spells: ids of either length bound no batch minted, and
//!   one a LIVE container really holds — the container level answers either way;
//! * whether the optional `namespace` is present, as the query the per-record operations
//!   bind it to or the body member the listing binds it to.
//!
//! The collection-level WRITE has a second set of rows of its own, because what it offers
//! is where a container-level refusal could have been worded from the request or could have
//! left something behind: those rows span how many records the batch offers (up to the
//! modelled maximum), which optional members each record carries, which record expiry the
//! request asks for, which spelling and partition the container id arrived in, and whether
//! the batch carries a `clientToken` — and each of them then proves the refusal persisted
//! NOTHING, by creating the container through the control plane and replaying the very same
//! request.
//!
//! Every case drives real HTTP through the same `MultiServiceRouter` the emulator serves
//! with, so routes, status codes and `restJson1` error bodies are exercised too.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::collections::BTreeSet;
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

/// Which memory-records operation a row sends. Every one of them this emulator implements
/// is here, because the container level belongs to the API rather than to one handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Get,
    Delete,
    List,
    BatchCreate,
}

/// Every operation under test, so no row set can quietly leave one out.
const EVERY_OP: [Op; 4] = [Op::Get, Op::Delete, Op::List, Op::BatchCreate];

impl Op {
    fn name(self) -> &'static str {
        match self {
            Self::Get => "GetMemoryRecord",
            Self::Delete => "DeleteMemoryRecord",
            Self::List => "ListMemoryRecords",
            Self::BatchCreate => "BatchCreateMemoryRecords",
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Delete => "DELETE",
            Self::List | Self::BatchCreate => "POST",
        }
    }

    /// Whether the operation addresses ONE record by id in its path, which is what decides
    /// whether a row spells a `memoryRecordId` at all and where a `namespace` is bound.
    fn addresses_one_record(self) -> bool {
        matches!(self, Self::Get | Self::Delete)
    }

    /// The status the operation answers once the container exists and the request is one it
    /// can serve: the `@http` code the batch create declares, 200 for the three that
    /// default it (source: smithy_query query="operation" operation="GetMemoryRecord,
    /// DeleteMemoryRecord,ListMemoryRecords,BatchCreateMemoryRecords").
    fn success_status(self) -> u16 {
        match self {
            Self::Get | Self::Delete | Self::List => 200,
            Self::BatchCreate => 201,
        }
    }

    /// The modelled URI of each operation with `{memoryId}` filled in: the READ is bound to
    /// the singular `memoryRecord` segment, the DELETE to the plural `memoryRecords` one,
    /// the listing to that collection itself and the batch create to its `batchCreate`
    /// sub-resource (same source as [`Op::success_status`]).
    fn uri(self, memory: &str, record: Option<&str>) -> String {
        let memory = label(memory);
        match self {
            Self::Get => {
                let record = record.expect("GetMemoryRecord addresses one record by id");
                format!("/memories/{memory}/memoryRecord/{}", label(record))
            }
            Self::Delete => {
                let record = record.expect("DeleteMemoryRecord addresses one record by id");
                format!("/memories/{memory}/memoryRecords/{}", label(record))
            }
            Self::List => format!("/memories/{memory}/memoryRecords"),
            Self::BatchCreate => format!("/memories/{memory}/memoryRecords/batchCreate"),
        }
    }

    /// The body the operation carries: the two path-addressed ones send none, the listing
    /// asks for a page (`namespace` is a BODY member there, not the query the per-record
    /// operations bind it to) and the batch create offers one record, tracked by the
    /// caller's own `requestIdentifier` so no two rows offer the same one.
    fn body(self, namespace: Option<&str>, request_identifier: &str) -> Option<Value> {
        match self {
            Self::Get | Self::Delete => None,
            Self::List => Some(
                namespace.map_or_else(|| json!({}), |namespace| json!({"namespace": namespace})),
            ),
            Self::BatchCreate => {
                Some(json!({"records": [one_record_payload(request_identifier)]}))
            }
        }
    }
}

/// One addressed call: the operation, the identifiers it spells, and whether it carries the
/// optional `namespace`.
#[derive(Debug, Clone)]
struct Call {
    op: Op,
    memory: String,
    record: Option<String>,
    namespace: Option<String>,
}

impl Call {
    /// One row's call: the operation, the container and record it addresses, and the
    /// `namespace` it carries, if any.
    fn new(op: Op, memory: &str, record: Option<&str>, namespace: Option<&str>) -> Self {
        Self {
            op,
            memory: memory.to_string(),
            record: record.map(str::to_string),
            namespace: namespace.map(str::to_string),
        }
    }

    fn path(&self) -> String {
        let mut path = self.op.uri(&self.memory, self.record.as_deref());
        if self.op.addresses_one_record() {
            if let Some(namespace) = &self.namespace {
                path.push_str(&format!("?namespace={}", label(namespace)));
            }
        }
        path
    }

    /// Every identifier this call names, none of which either refusal may carry.
    fn identifiers(&self) -> Vec<String> {
        let mut named = vec![self.memory.clone()];
        named.extend(self.record.clone());
        named.extend(self.namespace.clone());
        named
    }

    /// Send this row's call. `case` is the row's own name, which is what a write this row
    /// offers tracks itself by (`RequestIdentifier`, length 1..80, pattern
    /// `^[a-zA-Z0-9_-]+$` — source: smithy_query query="shape"
    /// shape="MemoryRecordCreateInput"), so one row's write is never another's.
    async fn send(&self, router: &MultiServiceRouter, case: &str) -> (u16, Value) {
        let body = self
            .op
            .body(self.namespace.as_deref(), &format!("req-{case}"));
        send(router, self.op.method(), &self.path(), body).await
    }
}

/// One `MemoryRecordCreateInput` a batch create offers, carrying only the members the model
/// requires.
fn one_record_payload(request_identifier: &str) -> Value {
    record_payload(request_identifier, None, None)
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
        Some(json!({"records": [one_record_payload(request_identifier)]})),
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
            Call::new(Op::Get, GHOST_MEMORY, Some(&shortest), None),
        ),
        (
            "get-in-a-container-nothing-created-by-arn",
            Call::new(Op::Get, GHOST_MEMORY_ARN, Some(&longest), None),
        ),
        (
            "get-in-a-container-nothing-created-with-the-namespace-query",
            Call::new(Op::Get, GHOST_MEMORY, Some(&punctuated), Some(NAMESPACE)),
        ),
        (
            "get-a-record-still-indexed-under-a-deleted-container",
            Call::new(Op::Get, DELETED_MEMORY, Some(&orphaned_record), None),
        ),
        (
            "get-in-a-deleted-container-by-arn",
            Call::new(
                Op::Get,
                DELETED_MEMORY_ARN,
                Some(&orphaned_record),
                Some(NAMESPACE),
            ),
        ),
        (
            "get-a-record-another-live-container-holds",
            Call::new(Op::Get, GHOST_MEMORY, Some(&live_record), None),
        ),
        (
            "delete-in-a-container-nothing-created",
            Call::new(Op::Delete, GHOST_MEMORY, Some(&longest), None),
        ),
        (
            "delete-in-a-container-nothing-created-by-arn",
            Call::new(
                Op::Delete,
                GHOST_MEMORY_ARN,
                Some(&shortest),
                Some(NAMESPACE),
            ),
        ),
        (
            "delete-a-record-still-indexed-under-a-deleted-container",
            Call::new(Op::Delete, DELETED_MEMORY, Some(&orphaned_record), None),
        ),
        (
            "delete-in-a-deleted-container-by-arn",
            Call::new(Op::Delete, DELETED_MEMORY_ARN, Some(&punctuated), None),
        ),
        (
            "delete-a-record-another-live-container-holds",
            Call::new(Op::Delete, GHOST_MEMORY, Some(&live_record), None),
        ),
        // The two collection-level operations name no record at all, and one of them
        // WRITES: the container level is the same gate either way.
        (
            "list-in-a-container-nothing-created",
            Call::new(Op::List, GHOST_MEMORY, None, None),
        ),
        (
            "list-in-a-container-nothing-created-by-arn-with-the-namespace-member",
            Call::new(Op::List, GHOST_MEMORY_ARN, None, Some(NAMESPACE)),
        ),
        (
            "list-a-deleted-container-this-plane-still-has-records-for",
            Call::new(Op::List, DELETED_MEMORY, None, None),
        ),
        (
            "batch-create-in-a-container-nothing-created",
            Call::new(Op::BatchCreate, GHOST_MEMORY, None, None),
        ),
        (
            "batch-create-in-a-container-nothing-created-by-arn",
            Call::new(Op::BatchCreate, GHOST_MEMORY_ARN, None, None),
        ),
        (
            "batch-create-in-a-deleted-container-by-arn",
            Call::new(Op::BatchCreate, DELETED_MEMORY_ARN, None, None),
        ),
    ];

    // The rule is the API's, so the rows must reach every operation of it under test.
    for op in EVERY_OP {
        assert!(
            cases.iter().any(|(_, call)| call.op == op),
            "case set: {} must have at least one row",
            op.name()
        );
    }

    for (case, call) in cases {
        let operation = call.op.name();
        let (status, body) = call.send(&router, case).await;
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
    let (status, body) = Call::new(Op::Get, LIVE_MEMORY, Some(&live_record), None)
        .send(&router, "the-live-containers-record-survives")
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

/// The two levels stay distinct: on a container the account DOES hold, the two per-record
/// operations refuse a record it does not hold with the RECORD sentence, the record it does
/// hold is served, and the two collection-level operations are served too — so the
/// container sentence is reached only by a missing container and the gate in front of it
/// refuses nothing else.
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
        let call = Call::new(op, memory, Some(&unminted), None);
        let operation = op.name();
        let (status, body) = call.send(&router, case).await;
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
        let (status, body) = Call::new(Op::Get, memory, Some(&held), None)
            .send(&router, case)
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

    // The container gate refuses nothing inside a container the account holds: the two
    // collection-level operations answer their modelled success status there, out of real
    // state — the listing reports the record the container holds, and the batch create
    // reports the id it minted, which the per-record read then serves — by either spelling
    // of the container id.
    for (case, memory) in [
        ("list-by-the-bare-id", LIVE_MEMORY),
        ("list-by-the-container-arn", LIVE_MEMORY_ARN),
    ] {
        let (status, listed) = Call::new(Op::List, memory, None, None)
            .send(&router, case)
            .await;
        assert_eq!(
            status,
            Op::List.success_status(),
            "case {case:?} on {}: expected HTTP {}, got {status} with {listed:?}",
            Op::List.name(),
            Op::List.success_status()
        );
        let reported: Vec<&str> = listed["memoryRecordSummaries"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row["memoryRecordId"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            reported.contains(&held.as_str()),
            "case {case:?}: expected the listing to report {held:?}, got {reported:?}"
        );
    }

    for (case, memory) in [
        ("batch-create-by-the-bare-id", LIVE_MEMORY),
        ("batch-create-by-the-container-arn", LIVE_MEMORY_ARN),
    ] {
        let (status, created) = Call::new(Op::BatchCreate, memory, None, None)
            .send(&router, case)
            .await;
        assert_eq!(
            status,
            Op::BatchCreate.success_status(),
            "case {case:?} on {}: expected HTTP {}, got {status} with {created:?}",
            Op::BatchCreate.name(),
            Op::BatchCreate.success_status()
        );
        let minted = created["successfulRecords"][0]["memoryRecordId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // `MemoryRecordId` is `^mem-[a-zA-Z0-9-_]*$`, length 40..50.
        assert!(
            minted.starts_with("mem-") && (40..=50).contains(&minted.len()),
            "case {case:?}: expected a minted MemoryRecordId, got {created:?}"
        );
        let (status, read) = Call::new(Op::Get, memory, Some(&minted), None)
            .send(&router, case)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the minted record to read back, got {status} with {read:?}"
        );
        assert_eq!(
            read["memoryRecord"]["memoryRecordId"].as_str(),
            Some(minted.as_str()),
            "case {case:?}: expected GetMemoryRecord to report {minted:?}, got {read:?}"
        );
    }

    // The delete of a held record still succeeds inside a live container, and a second
    // delete of it is the RECORD level again — not the container level.
    let (status, deleted) = Call::new(Op::Delete, LIVE_MEMORY, Some(&held), None)
        .send(&router, "delete-the-held-record")
        .await;
    assert_eq!(
        status, 200,
        "case \"delete the held record\": expected HTTP 200, got {status} with {deleted:?}"
    );
    let (status, body) = Call::new(Op::Delete, LIVE_MEMORY, Some(&held), None)
        .send(&router, "delete-the-held-record-twice")
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

// ---------------------------------------------------------------------------------------
// The collection-level WRITE: `POST /memories/{memoryId}/memoryRecords/batchCreate`.
//
// It is the one memory-records operation whose request carries content, so it is where a
// container-level refusal could have taken a word from the request, and the only one where
// a refusal could leave something behind. The rows below span what it can offer.
// ---------------------------------------------------------------------------------------

/// The account the test harness signs for, which is the account every ARN spelling below
/// names.
const ACCOUNT: &str = "123456789012";

/// One `MemoryRecordCreateInput`: `requestIdentifier`, `namespaces` (at most one entry),
/// `content` and `timestamp` are required, `memoryStrategyId` and `metadata` optional
/// (source: smithy_query query="shape" shape="MemoryRecordCreateInput").
fn record_payload(
    request_identifier: &str,
    memory_strategy_id: Option<&str>,
    metadata: Option<Value>,
) -> Value {
    let mut record = json!({
        "requestIdentifier": request_identifier,
        "namespaces": [NAMESPACE],
        "content": {"text": format!("content offered by {request_identifier}")},
        "timestamp": 1_700_000_000,
    });
    if let Some(strategy) = memory_strategy_id {
        record["memoryStrategyId"] = json!(strategy);
    }
    if let Some(metadata) = metadata {
        record["metadata"] = metadata;
    }
    record
}

/// A `MemoryRecordMetadataMap` with one entry per member of `MemoryRecordMetadataValue`, so
/// a row offers every value form the map admits (source: smithy_query query="shape"
/// shape="MemoryRecordMetadataValue").
fn every_metadata_value_form() -> Value {
    json!({
        "topic": {"stringValue": "seating"},
        "tags": {"stringListValue": ["window", "aisle"]},
        "confidence": {"numberValue": 0.75},
        "observed_at": {"dateTimeValue": 1_700_000_000},
    })
}

/// A well-formed bare `MemoryId` of this row's own, so no row's container is another's
/// (`MemoryId`: length 12.., pattern
/// `^(arn:(aws|aws-cn|aws-us-gov):bedrock-agentcore:[a-z0-9-]+:[0-9]{12}:memory/)?[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$`
/// — source: smithy_query query="shape" shape="MemoryId").
fn row_memory_id(row: usize) -> String {
    format!("batch-row{row}-0123456789")
}

/// The same container spelled as the memory ARN `MemoryId`'s pattern also admits, in any of
/// the three partitions it names.
fn memory_arn(partition: &str, region: &str, memory: &str) -> String {
    format!("arn:{partition}:bedrock-agentcore:{region}:{ACCOUNT}:memory/{memory}")
}

/// One batch create offered to a container the account does not hold.
#[derive(Debug)]
struct BatchCase {
    /// The row's own name, which every assertion names and every record tracks itself by.
    case: &'static str,
    /// The bare id of the row's container.
    memory: String,
    /// How the row SPELLS that container in the URI: the bare id, or an ARN.
    addressed_as: String,
    /// Whether the control plane created the container and then deleted it, rather than
    /// never having created it at all.
    deleted_rather_than_never_created: bool,
    /// The `records` the row offers, in order.
    records: Vec<Value>,
    /// The `clientToken` the row carries, if any: an `@idempotencyToken`, so a refusal that
    /// memoised it would answer a later retry out of what it never wrote.
    client_token: Option<String>,
    /// The `recordExpirySpecification` the row carries, if any.
    expiry: Option<Value>,
}

impl BatchCase {
    fn new(case: &'static str, row: usize, records: Vec<Value>) -> Self {
        let memory = row_memory_id(row);
        Self {
            case,
            addressed_as: memory.clone(),
            memory,
            deleted_rather_than_never_created: false,
            records,
            client_token: None,
            expiry: None,
        }
    }

    fn addressed_as(mut self, addressed_as: String) -> Self {
        self.addressed_as = addressed_as;
        self
    }

    fn deleted(mut self) -> Self {
        self.deleted_rather_than_never_created = true;
        self
    }

    fn with_client_token(mut self) -> Self {
        self.client_token = Some(format!("token-{}", self.case));
        self
    }

    fn with_expiry(mut self, expiry: Value) -> Self {
        self.expiry = Some(expiry);
        self
    }

    fn request_body(&self) -> Value {
        let mut body = json!({"records": self.records});
        if let Some(token) = &self.client_token {
            body["clientToken"] = json!(token);
        }
        if let Some(expiry) = &self.expiry {
            body["recordExpirySpecification"] = expiry.clone();
        }
        body
    }

    fn batch_create_path(&self) -> String {
        format!(
            "/memories/{}/memoryRecords/batchCreate",
            label(&self.addressed_as)
        )
    }

    /// The `requestIdentifier` of each record the row offers, in order.
    fn request_identifiers(&self) -> Vec<String> {
        self.records
            .iter()
            .map(|record| {
                record["requestIdentifier"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    /// Every value this row spells, none of which the container sentence may carry.
    fn named_values(&self) -> Vec<String> {
        let mut named = vec![self.memory.clone(), self.addressed_as.clone()];
        named.extend(self.client_token.clone());
        named.extend(self.request_identifiers());
        named.push(NAMESPACE.to_string());
        named
    }
}

/// The rows: each differs from the others in what the batch offers — how many records (one,
/// a few, and the modelled maximum of 100), which optional members a record carries, which
/// record expiry the request asks for, whether a `clientToken` is carried, which spelling
/// and partition the container id arrived in, and whether the container was deleted or never
/// created.
fn batch_cases() -> Vec<BatchCase> {
    let mut rows = vec![
        BatchCase::new(
            "one-record-and-nothing-optional",
            1,
            vec![record_payload("one-record-1", None, None)],
        ),
        BatchCase::new(
            "two-records-under-a-client-token",
            2,
            vec![
                record_payload("two-records-1", None, None),
                record_payload("two-records-2", None, None),
            ],
        )
        .with_client_token(),
        BatchCase::new(
            "every-metadata-value-form-and-a-strategy",
            3,
            vec![record_payload(
                "metadata-1",
                Some("semanticMemoryStrategy"),
                Some(every_metadata_value_form()),
            )],
        ),
        // `recordExpiryDurationDays` has `@range` min 31, so 31 is the shortest expiry the
        // model admits (source: smithy_query query="shape"
        // shape="RecordExpirySpecification").
        BatchCase::new(
            "an-expiry-at-the-shortest-duration-the-model-admits",
            4,
            vec![
                record_payload("expiry-min-1", None, None),
                record_payload("expiry-min-2", None, None),
            ],
        )
        .with_client_token()
        .with_expiry(json!({"recordExpiryDurationDays": 31})),
        // `RecordExpiryAction`'s only value, which creates records that do not expire.
        BatchCase::new(
            "an-expiry-remove-action",
            5,
            vec![record_payload("expiry-remove-1", None, None)],
        )
        .with_expiry(json!({"action": "REMOVE"})),
        BatchCase::new(
            "addressed-by-the-container-arn",
            6,
            vec![
                record_payload("by-arn-1", None, None),
                record_payload("by-arn-2", None, None),
                record_payload("by-arn-3", None, None),
            ],
        )
        .addressed_as(memory_arn("aws", "us-east-1", &row_memory_id(6)))
        .with_client_token(),
        BatchCase::new(
            "addressed-by-a-gov-partition-container-arn",
            7,
            vec![record_payload("by-gov-arn-1", None, None)],
        )
        .addressed_as(memory_arn("aws-us-gov", "us-gov-west-1", &row_memory_id(7))),
        BatchCase::new(
            "a-container-the-control-plane-deleted",
            8,
            vec![
                record_payload("deleted-container-1", None, None),
                record_payload("deleted-container-2", None, None),
            ],
        )
        .deleted()
        .with_client_token(),
    ];
    // `records` has `@length` max 100, so a batch of 100 is the largest one request may
    // offer (source: smithy_query query="shape" shape="MemoryRecordsCreateInputList").
    rows.push(BatchCase::new(
        "the-largest-batch-the-model-admits",
        9,
        (1..=100)
            .map(|n| record_payload(&format!("largest-batch-{n}"), None, None))
            .collect(),
    ));
    rows
}

/// A batch create whose `memoryId` names no container the account holds is refused 404
/// `ResourceNotFoundException` with the container sentence — whatever the batch offers —
/// and the refusal writes NOTHING: once the control plane creates the container, the very
/// same request (the same `clientToken` included) stores every record it offers, each of
/// them readable one by one and listed exactly once.
#[tokio::test]
async fn a_batch_create_into_an_absent_container_is_refused_and_stores_nothing() {
    let (engine, router) = emulator();

    for case in batch_cases() {
        let name = case.case;
        let offered = case.request_identifiers();
        let body = case.request_body();

        if case.deleted_rather_than_never_created {
            create_memory(&engine, &case.memory);
            delete_memory(&engine, &case.memory);
        }

        let (status, refused) = send(&router, "POST", &case.batch_create_path(), Some(body.clone()))
            .await;
        assert_eq!(
            status, 404,
            "case {name:?}: expected HTTP 404, got {status} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            RESOURCE_NOT_FOUND,
            "case {name:?}: expected {RESOURCE_NOT_FOUND}, got {refused:?}"
        );
        assert_eq!(
            message(&refused),
            CONTAINER_NOT_FOUND_MESSAGE,
            "case {name:?}: expected message {CONTAINER_NOT_FOUND_MESSAGE:?}, got {:?}",
            message(&refused)
        );
        for named in case.named_values() {
            assert!(
                !message(&refused).contains(&named),
                "case {name:?}: the container sentence must not carry {named:?}, got {:?}",
                message(&refused)
            );
        }

        // The container now exists, and the SAME request is replayed: a refusal that had
        // written records, or memoised the `clientToken` it carried, would answer out of
        // what it never stored.
        create_memory(&engine, &case.memory);
        let (status, created) = send(&router, "POST", &case.batch_create_path(), Some(body)).await;
        assert_eq!(
            status, 201,
            "case {name:?}: expected HTTP 201 once the container exists, got {status} with \
             {created:?}"
        );
        assert_eq!(
            created["failedRecords"].as_array().map(Vec::len),
            Some(0),
            "case {name:?}: expected no failed record, got {created:?}"
        );
        let successful = created["successfulRecords"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            successful.len(),
            offered.len(),
            "case {name:?}: expected {} successful record(s), got {created:?}",
            offered.len()
        );

        let mut minted: Vec<String> = Vec::with_capacity(successful.len());
        for (at, outcome) in successful.iter().enumerate() {
            assert_eq!(
                outcome["requestIdentifier"].as_str(),
                Some(offered[at].as_str()),
                "case {name:?}: expected outcome {at} to track {:?}, got {outcome:?}",
                offered[at]
            );
            assert_eq!(
                outcome["status"].as_str(),
                Some("SUCCEEDED"),
                "case {name:?}: expected outcome {at} to have SUCCEEDED, got {outcome:?}"
            );
            let id = outcome["memoryRecordId"].as_str().unwrap_or_default();
            // `MemoryRecordId`: length 40..50, pattern `^mem-[a-zA-Z0-9-_]*$`.
            assert!(
                id.starts_with("mem-") && (40..=50).contains(&id.len()),
                "case {name:?}: expected outcome {at} to carry a minted MemoryRecordId, got \
                 {outcome:?}"
            );
            minted.push(id.to_string());
        }
        let distinct: BTreeSet<&String> = minted.iter().collect();
        assert_eq!(
            distinct.len(),
            minted.len(),
            "case {name:?}: expected {} distinct record ids, got {minted:?}",
            minted.len()
        );

        // Every stored record reads back on its own, …
        for id in &minted {
            let (status, read) = send(
                &router,
                "GET",
                &format!(
                    "/memories/{}/memoryRecord/{}",
                    label(&case.addressed_as),
                    label(id)
                ),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {name:?}: expected {id:?} to read back, got {status} with {read:?}"
            );
            assert_eq!(
                read["memoryRecord"]["memoryRecordId"].as_str(),
                Some(id.as_str()),
                "case {name:?}: expected GetMemoryRecord to report {id:?}, got {read:?}"
            );
        }

        // … and the container holds exactly those records: nothing the refused call left
        // behind, and no record stored twice. `maxResults` max is 100, which is also the
        // largest batch a row offers, so one page holds the whole container.
        let (status, listed) = send(
            &router,
            "POST",
            &format!("/memories/{}/memoryRecords", label(&case.addressed_as)),
            Some(json!({"maxResults": 100})),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the listing to answer HTTP 200, got {status} with {listed:?}"
        );
        let reported: BTreeSet<String> = listed["memoryRecordSummaries"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row["memoryRecordId"].as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let expected: BTreeSet<String> = minted.iter().cloned().collect();
        assert_eq!(
            reported, expected,
            "case {name:?}: expected the container to hold exactly {expected:?}, got {reported:?}"
        );
    }
}
