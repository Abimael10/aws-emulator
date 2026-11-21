// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateQuickResponse` driven by SEVERAL callers at once, through
//! the same router the emulator serves with.
//!
//! The rules under test, each asserted over a TABLE of widths and target members rather
//! than one recorded request:
//!
//! * an update of a stored quick response is applied as ONE step: a caller that no
//!   modelled check refuses is answered HTTP 200 with the record carrying its own write,
//!   however many other callers are updating the same record at the same moment — a
//!   concurrent writer is never a reason to refuse — and the record a later read reports
//!   is one accepted request's write whole, never a mixture of two;
//! * a member that identifies a quick response inside its knowledge base (`name`,
//!   `shortcutKey`) is held by at most one live record, so of several callers claiming one
//!   free value for different records exactly one is answered 200 and every other is
//!   refused the modelled `ConflictException` (HTTP 409) with the sentence this endpoint
//!   states for a value already in use, which writes nothing;
//! * an update names ONE record, so concurrent updates of different records of one
//!   knowledge base each apply in full and none overwrites another.
//!
//! Every assertion goes over the wire — only the HTTP status, the `x-amzn-errortype`
//! header and the JSON body are inspected, and every write is read back through
//! `GetQuickResponse` — so an answer that is not computed from persisted state cannot
//! pass.
//!
//! The harness here builds the router behind an [`Arc`] (as the content-revision race
//! suite does) because a racer is spawned onto its own task and so needs a router it can
//! own a handle to; the single-caller suites share the simpler harness in
//! `resource_tags.rs`.
//!
//! This view of the shared `wisdom` endpoint answers under its own registration key (see
//! `src/service.rs`), which is what every request below is signed for.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_wisdom_wisdomservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite is signed for.
const REGION: &str = "eu-west-1";

/// The routing key this service answers under (`src/service.rs`'s
/// `OWN_REGISTRATION_KEY`): the shared `wisdom` endpoint identity is served by the
/// Amazon Q in Connect crate, so this legacy model is addressed by its own key.
const ROUTE_KEY: &str = "com-amazonaws-wisdom-wisdomservice";

/// One of the two values the modelled `QuickResponseType` pattern admits
/// (`smithy_query(query="operation",operation="UpdateQuickResponse")`: `contentType`
/// matches `^(application/x\.quickresponse;format=(plain|markdown))$`).
const CONTENT_TYPE_PLAIN: &str = "application/x.quickresponse;format=plain";

/// The `QuickResponseStatus` a stored quick response reports, which an accepted update
/// leaves where it was (`smithy_query(query="shape",shape="QuickResponseStatus")`).
const STATUS_CREATED: &str = "CREATED";

/// The error code the modelled 409 of this operation answers under
/// (`smithy_query(query="operation",operation="UpdateQuickResponse")` lists
/// `ConflictException`).
const CONFLICT_CODE: &str = "ConflictException";

/// The sentence this endpoint refuses a claim on a value a live quick response of the
/// same knowledge base already holds with, written out here rather than imported from the
/// implementation, and the member names it is worded with — as the API words them in that
/// sentence, which for the shortcut key is prose rather than the modelled member name.
fn message_value_in_use(member: &str) -> String {
    format!("The value for {member} is already in use. Please update and try again.")
}
const MEMBER_NAME: &str = "name";
const MEMBER_SHORTCUT_KEY: &str = "shortcut key";

/// How many callers a row puts in flight together: one pair, an odd width, and a width
/// wider than the runtime's worker threads, so a row also exercises callers that queue.
const WIDTHS: [usize; 3] = [2, 3, 5];

/// One wire answer, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_type: String,
    body: Value,
}

impl Reply {
    fn field(&self, pointer: &str) -> Option<&Value> {
        self.body.pointer(pointer).filter(|value| !value.is_null())
    }

    fn string(&self, pointer: &str) -> String {
        self.field(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The error code a client branches on: the `x-amzn-errortype` header, falling back to
    /// the body's `__type` for a protocol that carries it only there.
    fn code(&self) -> String {
        if self.error_type.is_empty() {
            self.string("/__type")
        } else {
            self.error_type.clone()
        }
    }
}

/// One signed call of this endpoint, built the way an SDK builds it. Free-standing so a
/// racer can be spawned with nothing but the shared router.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Reply {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{ROUTE_KEY}/aws4_request, \
         SignedHeaders=host, Signature=not-verified-in-this-suite"
    );
    let builder = http::Request::builder()
        .method(method)
        .uri(format!("https://wisdom.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("wisdom.{REGION}.amazonaws.com"))
        .header("authorization", authorization);
    let request = match &body {
        Some(value) => builder
            .header("content-type", "application/json")
            .body(Bytes::from(
                serde_json::to_vec(value).expect("serialize body"),
            ))
            .expect("build request"),
        None => builder.body(Bytes::new()).expect("build request"),
    };
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply {
        status,
        error_type,
        body,
    }
}

/// One `UpdateQuickResponse` call, as a racer runs it: it owns a handle to the router, so
/// it can be spawned onto its own task.
async fn update(
    router: Arc<MultiServiceRouter>,
    knowledge_base_id: String,
    quick_response_id: String,
    body: Value,
) -> Reply {
    call(
        &router,
        "POST",
        &format!("/knowledgeBases/{knowledge_base_id}/quickResponses/{quick_response_id}"),
        Some(body),
    )
    .await
}

/// An emulator with just this service mounted on a fresh engine, driven in-process
/// through the production router.
struct Emulator {
    router: Arc<MultiServiceRouter>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signatures are not what these tests are about; the router still routes by the
        // SigV4 credential scope above.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self {
            router: Arc::new(router),
        }
    }

    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> Reply {
        call(&self.router, method, uri, body).await
    }

    /// A knowledge base that holds quick responses, as its create persisted it.
    async fn knowledge_base(&self, name: &str) -> String {
        let reply = self
            .send(
                "POST",
                "/knowledgeBases",
                Some(json!({ "name": name, "knowledgeBaseType": "QUICK_RESPONSES" })),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case \"knowledge base {name}\": expected HTTP 200, got {} with {}",
            reply.status, reply.body
        );
        reply.string("/knowledgeBase/knowledgeBaseId")
    }

    /// A quick response of `knowledge_base_id`, as its create persisted it.
    async fn quick_response(&self, knowledge_base_id: &str, body: Value) -> Reply {
        let reply = self
            .send(
                "POST",
                &format!("/knowledgeBases/{knowledge_base_id}/quickResponses"),
                Some(body.clone()),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case \"quick response {body}\": expected HTTP 200 from the create, got {} with {}",
            reply.status, reply.body
        );
        reply
    }

    async fn get_quick_response(&self, knowledge_base_id: &str, quick_response_id: &str) -> Reply {
        self.send(
            "GET",
            &format!("/knowledgeBases/{knowledge_base_id}/quickResponses/{quick_response_id}"),
            None,
        )
        .await
    }
}

/// A create body carrying every member a racer below writes over, so a read after the
/// race can be read as "what the accepted write changed".
fn baseline_body(name: &str) -> Value {
    json!({
        "name": name,
        "content": { "content": "The baseline reply" },
        "contentType": CONTENT_TYPE_PLAIN,
        "description": "the baseline description",
        "isActive": true,
        "channels": ["Chat"],
        "language": "en_US",
    })
}

/// The members one racer writes together — the triple a read after the race must report
/// from ONE racer, never one member from one caller and another from a second. The row's
/// width is part of each value so that no row claims a `name` a previous row's record of
/// the same knowledge base still holds.
fn written_members(width: usize, caller: usize) -> Value {
    json!({
        "name": format!("racer-{width}-{caller}"),
        "content": { "content": format!("the reply written by caller {width}-{caller}") },
        "description": format!("written by caller {width}-{caller}"),
    })
}

/// The `(name, content, description)` triple a reported record carries, which is what the
/// whole-write assertions compare.
fn reported_triple(reply: &Reply, pointer: &str) -> (String, String, String) {
    (
        reply.string(&format!("{pointer}/name")),
        reply.string(&format!("{pointer}/contents/plainText/content")),
        reply.string(&format!("{pointer}/description")),
    )
}

/// RULE: concurrent updates of one quick response are serialised, so every caller is
/// answered 200 with its own write applied whole, and the record a later read reports is
/// one caller's write — never a mixture.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_updates_of_one_quick_response_each_apply_whole() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.knowledge_base("concurrent-update-kb").await;

    for width in WIDTHS {
        let case = format!("{width} callers updating one quick response");
        let created = emulator
            .quick_response(
                &knowledge_base_id,
                baseline_body(&format!("contended-{width}")),
            )
            .await;
        let quick_response_id = created.string("/quickResponse/quickResponseId");
        let created_time = created.field("/quickResponse/createdTime").cloned();

        let racers: Vec<_> = (0..width)
            .map(|caller| {
                tokio::spawn(update(
                    Arc::clone(&emulator.router),
                    knowledge_base_id.clone(),
                    quick_response_id.clone(),
                    written_members(width, caller),
                ))
            })
            .collect();

        let mut writes = BTreeSet::new();
        for (caller, racer) in racers.into_iter().enumerate() {
            let answer = racer.await.expect("the update ran to completion");
            assert_eq!(
                answer.status, 200,
                "case {case:?}, caller {caller}: a concurrent writer is no reason to refuse an \
                 update, got HTTP {} ({:?}) with {}",
                answer.status,
                answer.code(),
                answer.body
            );
            // The answer is the record as this caller's own write left it: every member it
            // supplied, and the id and parent of the record it named.
            let expected = (
                format!("racer-{width}-{caller}"),
                format!("the reply written by caller {width}-{caller}"),
                format!("written by caller {width}-{caller}"),
            );
            let answered = reported_triple(&answer, "/quickResponse");
            assert_eq!(
                answered, expected,
                "case {case:?}, caller {caller}: the answer must report this caller's own write, \
                 got {answered:?} in {}",
                answer.body
            );
            assert_eq!(
                answer.string("/quickResponse/quickResponseId"),
                quick_response_id,
                "case {case:?}, caller {caller}: the answer must report the record the request \
                 named, got {}",
                answer.body
            );
            writes.insert(answered);
        }
        assert_eq!(
            writes.len(),
            width,
            "case {case:?}: every caller must be answered its own write, got {writes:?}"
        );

        // The stored record holds exactly one caller's write, whole — and the members no
        // caller supplied are the ones the create persisted.
        let read = emulator
            .get_quick_response(&knowledge_base_id, &quick_response_id)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the record must still be readable, got HTTP {} with {}",
            read.status, read.body
        );
        let stored = reported_triple(&read, "/quickResponse");
        assert!(
            writes.contains(&stored),
            "case {case:?}: the stored record must hold one caller's write whole, got \
             {stored:?} which is none of {writes:?} in {}",
            read.body
        );
        for (member, expected) in [
            ("contentType", json!(CONTENT_TYPE_PLAIN)),
            ("status", json!(STATUS_CREATED)),
            ("isActive", json!(true)),
            ("channels", json!(["Chat"])),
            ("language", json!("en_US")),
        ] {
            assert_eq!(
                read.field(&format!("/quickResponse/{member}")),
                Some(&expected),
                "case {case:?}: no caller supplied {member}, so it must keep the value the \
                 create stored, got {}",
                read.body
            );
        }
        assert_eq!(
            read.field("/quickResponse/createdTime").cloned(),
            created_time,
            "case {case:?}: an update must not move createdTime, got {}",
            read.body
        );
    }
}

/// RULE: a member that identifies a quick response inside its knowledge base is held by
/// at most one live record, so of several callers claiming one free value for different
/// records exactly one wins and every other is refused the modelled conflict, which
/// writes nothing. Asserted for each such member, at several widths.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_claims_on_one_identifying_value_have_exactly_one_winner() {
    // (the request member carrying the claim, the member of the record that reports it,
    //  the member name the refusal sentence is worded with)
    let members = [
        ("name", "name", MEMBER_NAME),
        ("shortcutKey", "shortcutKey", MEMBER_SHORTCUT_KEY),
    ];

    for (request_member, reported_member, worded_member) in members {
        for width in WIDTHS {
            let case = format!("{width} callers claiming one {request_member}");
            // One knowledge base per row: the value is unique inside a knowledge base, so
            // a row must not see another row's claim.
            let emulator = Emulator::new();
            let knowledge_base_id = emulator
                .knowledge_base(&format!("claim-{request_member}-{width}"))
                .await;
            let mut records = Vec::new();
            for caller in 0..width {
                let created = emulator
                    .quick_response(&knowledge_base_id, baseline_body(&format!("held-{caller}")))
                    .await;
                records.push(created.string("/quickResponse/quickResponseId"));
            }
            // A value no record holds yet, within the published bounds of both members
            // (`smithy_query(query="operation",operation="UpdateQuickResponse")`:
            // `QuickResponseName` len 1..40, `ShortCutKey` len 1..10).
            let claimed = format!("/claim{width}");

            let racers: Vec<_> = records
                .iter()
                .map(|quick_response_id| {
                    tokio::spawn(update(
                        Arc::clone(&emulator.router),
                        knowledge_base_id.clone(),
                        quick_response_id.clone(),
                        json!({ request_member: claimed.clone() }),
                    ))
                })
                .collect();

            let mut winners = Vec::new();
            let mut refusals = 0;
            for (caller, racer) in racers.into_iter().enumerate() {
                let answer = racer.await.expect("the update ran to completion");
                match answer.status {
                    200 => winners.push(caller),
                    409 => {
                        assert_eq!(
                            answer.code(),
                            CONFLICT_CODE,
                            "case {case:?}, caller {caller}: a loser must be refused \
                             {CONFLICT_CODE}, got {:?} with {}",
                            answer.code(),
                            answer.body
                        );
                        assert_eq!(
                            answer.string("/message"),
                            message_value_in_use(worded_member),
                            "case {case:?}, caller {caller}: the refusal must state the value is \
                             in use, got {}",
                            answer.body
                        );
                        refusals += 1;
                    }
                    status => panic!(
                        "case {case:?}, caller {caller}: expected HTTP 200 or 409, got {status} \
                         with {}",
                        answer.body
                    ),
                }
            }
            assert_eq!(
                winners.len(),
                1,
                "case {case:?}: expected exactly one winner, got winners {winners:?} and \
                 {refusals} refusals"
            );
            assert_eq!(
                refusals,
                width - 1,
                "case {case:?}: expected every other caller to be refused, got {refusals}"
            );

            // Exactly one stored record answers to the claimed value, and a refused
            // caller's record is as its create left it.
            let winner = winners[0];
            for (caller, quick_response_id) in records.iter().enumerate() {
                let read = emulator
                    .get_quick_response(&knowledge_base_id, quick_response_id)
                    .await;
                let held = read.field(&format!("/quickResponse/{reported_member}")).cloned();
                if caller == winner {
                    assert_eq!(
                        held,
                        Some(json!(claimed.clone())),
                        "case {case:?}: the winner's record must hold the claimed value, got {}",
                        read.body
                    );
                } else {
                    assert_ne!(
                        held,
                        Some(json!(claimed.clone())),
                        "case {case:?}, caller {caller}: a refused claim must write nothing, got \
                         {}",
                        read.body
                    );
                    assert_eq!(
                        read.string("/quickResponse/name"),
                        format!("held-{caller}"),
                        "case {case:?}, caller {caller}: a refused claim must leave the record as \
                         its create stored it, got {}",
                        read.body
                    );
                }
            }
        }
    }
}

/// RULE: an update names ONE record, so concurrent updates of DIFFERENT quick responses of
/// one knowledge base each apply in full and none overwrites another.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_updates_of_different_quick_responses_all_apply() {
    let emulator = Emulator::new();
    let knowledge_base_id = emulator.knowledge_base("parallel-update-kb").await;

    for width in WIDTHS {
        let case = format!("{width} callers updating {width} quick responses");
        let mut records = Vec::new();
        for caller in 0..width {
            let created = emulator
                .quick_response(
                    &knowledge_base_id,
                    baseline_body(&format!("parallel-{width}-{caller}")),
                )
                .await;
            records.push(created.string("/quickResponse/quickResponseId"));
        }

        let racers: Vec<_> = records
            .iter()
            .enumerate()
            .map(|(caller, quick_response_id)| {
                tokio::spawn(update(
                    Arc::clone(&emulator.router),
                    knowledge_base_id.clone(),
                    quick_response_id.clone(),
                    json!({
                        "description": format!("written by caller {width}-{caller}"),
                        "content": { "content": format!("the reply of caller {width}-{caller}") },
                        "isActive": caller % 2 == 0,
                    }),
                ))
            })
            .collect();

        for (caller, racer) in racers.into_iter().enumerate() {
            let answer = racer.await.expect("the update ran to completion");
            assert_eq!(
                answer.status, 200,
                "case {case:?}, caller {caller}: expected HTTP 200, got {} ({:?}) with {}",
                answer.status,
                answer.code(),
                answer.body
            );
        }

        for (caller, quick_response_id) in records.iter().enumerate() {
            let read = emulator
                .get_quick_response(&knowledge_base_id, quick_response_id)
                .await;
            assert_eq!(
                read.string("/quickResponse/description"),
                format!("written by caller {width}-{caller}"),
                "case {case:?}, caller {caller}: each record must hold its own writer's \
                 description, got {}",
                read.body
            );
            assert_eq!(
                read.string("/quickResponse/contents/plainText/content"),
                format!("the reply of caller {width}-{caller}"),
                "case {case:?}, caller {caller}: each record must hold its own writer's content, \
                 got {}",
                read.body
            );
            assert_eq!(
                read.field("/quickResponse/isActive"),
                Some(&json!(caller % 2 == 0)),
                "case {case:?}, caller {caller}: each record must hold its own writer's isActive, \
                 got {}",
                read.body
            );
            assert_eq!(
                read.string("/quickResponse/name"),
                format!("parallel-{width}-{caller}"),
                "case {case:?}, caller {caller}: no caller supplied a name, so each record must \
                 keep the one its create stored, got {}",
                read.body
            );
        }
    }
}
