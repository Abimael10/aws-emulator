// SPDX-License-Identifier: CC0-1.0

//! An accepted AgentCore Memory deletion ends the DATA plane's access to that container.
//!
//! RULE: a control-plane delete takes effect for the whole emulator the instant it is
//! accepted — the account stops holding that resource — so every call addressing it,
//! including another service's data plane that resolves the resource against the plane
//! which owns it, is refused `ResourceNotFoundException` (HTTP 404) exactly as an
//! identifier nothing ever created is, while every other resource of the same kind is
//! served unchanged. The control plane goes on reporting the resource `DELETING` until the
//! removal the clock owes falls due — the refusals below are answered while that record is
//! still there, because access ends when the deletion is ACCEPTED, not when the record
//! finally goes. (What the control plane itself reports over that span is the subject of
//! the owning crate's own `tests/all/memory_deletion.rs`.)
//!
//! This suite lives here rather than in either service's crate because the rule spans the
//! two: AgentCore Control mints and deletes the Memory container, the AgentCore data plane
//! writes and reads inside it and creates no container of its own, and a single-service
//! router has only one half of that.
//!
//! The rows span what the rule generalises over: which data-plane operation names the
//! container (two writes and three reads, at three route depths), whether the container was
//! deleted or never created, and which container is addressed (the deleted one or the
//! sibling the delete must leave alone).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The two planes over one shared engine, one restJson1 call through the router, the error
// members of an answer, the control plane's own `CreateMemory`, the data plane's two writes
// and the actor and session they address: the sibling cross-plane suite owns them, so both
// suites build and read a request the one way.
use crate::bedrock_agentcore_data_plane_parent_resources::{
    call, create_event, create_memory, emulator, error_code, ingest_data, message, ACTOR, SESSION,
};
// The one percent-encoder these suites share: an `eventId`'s `#` reaches the server as part
// of the `@httpLabel` value rather than as URI syntax, exactly as an AWS SDK sends it.
use crate::cloudfront_key_value_store_handshake::segment as percent_encode;

/// An id of the modelled `MemoryId` shape (`^[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$`,
/// length 12..) that no create in this suite ever mints.
const GHOST_MEMORY: &str = "never-created-memory-0123456789";

/// `DeleteMemory` on the control plane: `DELETE /memories/{memoryId}/delete`.
async fn delete_memory(router: &MultiServiceRouter, memory: &str) -> (u16, Value) {
    call(
        router,
        "DELETE",
        &format!("/memories/{}/delete", percent_encode(memory)),
        None,
    )
    .await
}

/// Write one event into `memory` through the data plane and hand back the `eventId` the
/// service minted, so the reads below address something the container really holds.
async fn write_one_event(router: &MultiServiceRouter, memory: &str) -> String {
    let (status, created) = create_event(router, memory).await;
    assert_eq!(
        status, 201,
        "the seed event for {memory} must be created, got {status} with {created}"
    );
    created["event"]["eventId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateEvent reported no eventId: {created}"))
        .to_string()
}

/// One data-plane call that addresses a Memory container by path. Two writes and three
/// reads, at the three route depths the memory plane binds: the container, the actor under
/// it, and one event of one session of that actor.
#[derive(Debug, Clone, Copy)]
enum MemoryCall {
    CreateEvent,
    IngestData,
    ListSessions,
    ListEvents,
    GetEvent,
}

impl MemoryCall {
    /// Every row, so the tests can state the rule over all of them.
    const ALL: [Self; 5] = [
        Self::CreateEvent,
        Self::IngestData,
        Self::ListSessions,
        Self::ListEvents,
        Self::GetEvent,
    ];

    fn operation(self) -> &'static str {
        match self {
            Self::CreateEvent => "CreateEvent",
            Self::IngestData => "IngestData",
            Self::ListSessions => "ListSessions",
            Self::ListEvents => "ListEvents",
            Self::GetEvent => "GetEvent",
        }
    }

    /// The status this call answers for a container the control plane holds (each one's
    /// `@http` code).
    fn success_status(self) -> u16 {
        match self {
            Self::CreateEvent => 201,
            Self::IngestData => 202,
            Self::ListSessions | Self::ListEvents | Self::GetEvent => 200,
        }
    }

    /// The whole `ResourceNotFoundException` message every one of these calls is refused
    /// with when the container its path names is not one the account holds: the memory is
    /// the outermost identifier, so it is the one that failed to resolve and the message's
    /// only variable part. A method rather than a free function because the sibling suite
    /// keeps a per-operation wording table in the same place — the memory-RECORDS surface
    /// words its container level differently — so a row that needs its own sentence has one
    /// place to state it.
    fn absent_container_message(self, memory: &str) -> String {
        format!("Memory not found: {memory}")
    }

    async fn send(
        self,
        router: &MultiServiceRouter,
        memory: &str,
        event_id: &str,
    ) -> (u16, Value) {
        let memory_label = percent_encode(memory);
        match self {
            // The two writes are the sibling suite's, so the body this suite sends is the
            // body that one asserts on.
            Self::CreateEvent => create_event(router, memory).await,
            Self::IngestData => ingest_data(router, memory).await,
            Self::ListSessions => {
                call(
                    router,
                    "POST",
                    &format!(
                        "/memories/{memory_label}/actor/{}/sessions",
                        percent_encode(ACTOR)
                    ),
                    Some(json!({"maxResults": 10})),
                )
                .await
            }
            Self::ListEvents => {
                call(
                    router,
                    "POST",
                    &format!(
                        "/memories/{memory_label}/actor/{}/sessions/{}",
                        percent_encode(ACTOR),
                        percent_encode(SESSION)
                    ),
                    Some(json!({"maxResults": 10, "includePayloads": true})),
                )
                .await
            }
            Self::GetEvent => {
                call(
                    router,
                    "GET",
                    &format!(
                        "/memories/{memory_label}/actor/{}/sessions/{}/events/{}",
                        percent_encode(ACTOR),
                        percent_encode(SESSION),
                        percent_encode(event_id)
                    ),
                    None,
                )
                .await
            }
        }
    }
}

/// Every data-plane call is served for a container the control plane holds, and refused for
/// the one it has deleted — exactly as for an identifier nothing ever created — while the
/// sibling container goes on being served.
#[tokio::test]
async fn a_deleted_memory_container_serves_no_data_plane_call() {
    let router = emulator();
    let doomed = create_memory(&router, "doomed_memory").await;
    let kept = create_memory(&router, "kept_memory").await;
    let doomed_event = write_one_event(&router, &doomed).await;
    let kept_event = write_one_event(&router, &kept).await;

    // Before the delete, both containers answer every call: the refusals below are the
    // deletion's doing and not something these paths always answer.
    for held in MemoryCall::ALL {
        for (case, memory, event) in [
            ("the container a delete later removes", &doomed, &doomed_event),
            ("the container the delete leaves alone", &kept, &kept_event),
        ] {
            let expected = held.success_status();
            let (status, body) = held.send(&router, memory, event).await;
            assert_eq!(
                status,
                expected,
                "case {:?} on {case:?} before the delete: expected HTTP {expected}, got {status} \
                 with {body}",
                held.operation()
            );
        }
    }

    // The control plane accepts the deletion and reports the memory it was addressed by.
    let (status, accepted) = delete_memory(&router, &doomed).await;
    assert_eq!(
        (
            status,
            accepted["memoryId"].clone(),
            accepted["status"].clone()
        ),
        (202, json!(doomed.clone()), json!("DELETING")),
        "DeleteMemory must accept the deletion and report the addressed memory DELETING, got \
         {status} with {accepted}"
    );

    for refused_call in MemoryCall::ALL {
        let case = refused_call.operation();

        // The deleted container takes nothing, read or write.
        let (status, refused) = refused_call.send(&router, &doomed, &doomed_event).await;
        assert_eq!(
            (status, error_code(&refused), message(&refused)),
            (
                404,
                "ResourceNotFoundException".to_string(),
                refused_call.absent_container_message(&doomed)
            ),
            "case {case:?} on the deleted container: expected HTTP 404 \
             ResourceNotFoundException naming the memory, got {status} with {refused}"
        );

        // An identifier nothing ever created is refused the very same way, so a deleted
        // container really is one the account no longer holds.
        let (status, never) = refused_call
            .send(&router, GHOST_MEMORY, &doomed_event)
            .await;
        assert_eq!(
            (status, error_code(&never), message(&never)),
            (
                404,
                "ResourceNotFoundException".to_string(),
                refused_call.absent_container_message(GHOST_MEMORY)
            ),
            "case {case:?} on a container never created: expected the same HTTP 404 \
             ResourceNotFoundException, got {status} with {never}"
        );

        // And the sibling container is untouched by a delete that named another id.
        let expected = refused_call.success_status();
        let (status, served) = refused_call.send(&router, &kept, &kept_event).await;
        assert_eq!(
            status, expected,
            "case {case:?} on the container the delete left alone: expected HTTP {expected}, got \
             {status} with {served}"
        );
    }
}
