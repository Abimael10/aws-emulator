// SPDX-License-Identifier: CC0-1.0

//! What an accepted DELETION of an AWS Supply Chain instance makes observable, and how
//! this service words a call that names an instance it does not hold — driven end to end
//! over the real wire path (a restJson1 request through the multi-service router into the
//! mounted service), so every assertion is about what a client observes.
//!
//! RULES under test:
//!
//! * the not-found refusal of a call whose `instanceId` names no instance this service
//!   holds is `ResourceNotFoundException` (HTTP 404) carrying ONE fixed sentence —
//!   "Instance not found." — which names the resource kind and interpolates nothing from
//!   the request, whichever operation was called and whichever id it carried;
//! * `DeleteInstance` ACCEPTS a deletion rather than performing it: the instance moves to
//!   `Deleting` and stays addressable while the deletion settles — a read reports it, with
//!   the members it carried — and only afterwards does the record, with everything
//!   addressed by it, cease to exist;
//! * a MUTATION of an instance is served only while the instance is `Active`: in any other
//!   state the call is refused `ValidationException` (HTTP 400) naming the state
//!   requirement, and nothing about the instance changes. That is a different refusal from
//!   the not-found one, because what refused the call is the instance's state machine
//!   rather than its existence.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header whose credential scope names this service (`scn`), which
/// is how the router picks the service that claims the request. Signature verification is
/// switched off for the test router, so the signature itself is never read.
const SCN_CREDENTIAL: &str = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/scn/aws4_request, SignedHeaders=host, Signature=00";

/// The sentences this service answers these two refusals with, written out HERE rather than
/// read from the crate: the wording is the requirement, so a drift in the constant the
/// handler answers from fails this suite instead of travelling with it.
const INSTANCE_NOT_FOUND_SENTENCE: &str = "Instance not found.";
const NOT_ACTIVE_SENTENCE: &str = "Instance must be in Active state to perform this operation.";

/// The `InstanceState` values these cases name, as the model spells them.
const STATE_INITIALIZING: &str = "Initializing";
const STATE_ACTIVE: &str = "Active";
const STATE_DELETING: &str = "Deleting";

/// How long a poll loop waits for the condition it is about before giving up, and how long
/// it waits between looks.
///
/// Generous on purpose: each loop below waits for a CONDITION the emulator has reached, so
/// the deadline is only there to end a case that can never satisfy it and never a budget
/// the host has to beat.
const POLL_DEADLINE: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The emulator under test: one engine, one service, driven through the router.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Keep the case independent of how this process was launched: no authenticator, so
        // a request routes on its credential scope alone.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Send one request and return its HTTP status and JSON body.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let body_bytes = match &body {
            Some(value) => Bytes::from(serde_json::to_vec(value).expect("serialize request body")),
            None => Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://scn.us-east-1.amazonaws.com{path}"))
            .header("host", "scn.us-east-1.amazonaws.com")
            .header("authorization", SCN_CREDENTIAL)
            .header("content-type", "application/json")
            .body(body_bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "{method} {path}: body is not JSON ({error}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, parsed)
    }

    /// A fresh instance, as `CreateInstance` answered it.
    async fn create(&self, case: &str, request: Value) -> Value {
        let (status, body) = self.send("POST", "/api/instance", Some(request)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateInstance expected 200, got {status} with {body}"
        );
        instance_of(case, &body)
    }

    async fn get(&self, id: &str) -> (u16, Value) {
        self.send("GET", &format!("/api/instance/{id}"), None).await
    }

    async fn update(&self, id: &str, patch: Value) -> (u16, Value) {
        self.send("PATCH", &format!("/api/instance/{id}"), Some(patch))
            .await
    }

    async fn delete(&self, id: &str) -> (u16, Value) {
        self.send("DELETE", &format!("/api/instance/{id}"), None)
            .await
    }

    /// The state `GetInstance` reports for `id`, or `None` once it answers 404.
    async fn state(&self, id: &str) -> Option<String> {
        let (status, body) = self.get(id).await;
        if status == 404 {
            return None;
        }
        assert_eq!(
            status, 200,
            "GetInstance expected 200 or 404, got {status} with {body}"
        );
        Some(string_of("state", &instance_of("state", &body), "state"))
    }

    /// Look again until `id` reports `Active`, or until [`POLL_DEADLINE`].
    async fn wait_until_active(&self, case: &str, id: &str) {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            if self.state(id).await.as_deref() == Some(STATE_ACTIVE) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: {id} never became Active"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Look again until the accepted deletion of `id` has SETTLED — until `GetInstance`
    /// answers 404 — or until [`POLL_DEADLINE`].
    async fn wait_until_deleted(&self, case: &str, id: &str) {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            if self.state(id).await.is_none() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the deletion of {id} never settled"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// The `instance` member of a response body.
fn instance_of(case: &str, body: &Value) -> Value {
    body.get("instance")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: response carries no `instance` member: {body}"))
}

/// A string member of a payload.
fn string_of(case: &str, payload: &Value, member: &str) -> String {
    payload
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: payload reports no `{member}`: {payload}"))
        .to_string()
}

/// The error code a refusal envelope carries.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The sentence a refusal envelope carries.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// RULE: every operation that refuses a call because the addressed instance does not exist
/// answers 404 `ResourceNotFoundException` with the one fixed sentence, echoing nothing of
/// the request — so the sentence does not depend on the operation, on the id, or on whether
/// that id was ever an instance.
#[tokio::test]
async fn an_instance_not_found_is_one_fixed_sentence_that_echoes_nothing() {
    let emulator = Emulator::new();

    // One id that WAS an instance of this account and whose deletion has settled, so the
    // rows cover both ways an id can name no instance.
    let gone = string_of(
        "a deleted instance",
        &emulator.create("a deleted instance", json!({})).await,
        "instanceId",
    );
    let (status, body) = emulator.delete(&gone).await;
    assert_eq!(
        status, 200,
        "the instance under test must be deleted first, got {status} with {body}"
    );
    emulator.wait_until_deleted("a deleted instance", &gone).await;

    // Rows differ along the id: ids of the modelled `UUID` shape that no instance holds —
    // all zeroes, all f's, a random-looking one — and one this account really did delete.
    let ids = [
        ("the lowest id the shape admits", "00000000-0000-0000-0000-000000000000".to_string()),
        ("the highest id the shape admits", "ffffffff-ffff-ffff-ffff-ffffffffffff".to_string()),
        ("an id nothing created", "d0e75531-eb2d-4a3b-a1bf-8dd8e148e377".to_string()),
        ("an id this account deleted", gone.clone()),
    ];

    for (case, id) in ids {
        // Rows differ along the operation: every one that addresses one instance, read and
        // mutation alike, plus a call addressing a resource WITHIN the instance.
        let calls: Vec<(&str, (u16, Value))> = vec![
            ("GetInstance", emulator.get(&id).await),
            (
                "UpdateInstance",
                emulator
                    .update(&id, json!({ "instanceName": "renamed" }))
                    .await,
            ),
            ("DeleteInstance", emulator.delete(&id).await),
            (
                "CreateDataLakeDataset",
                emulator
                    .send(
                        "PUT",
                        &format!("/api/datalake/instance/{id}/namespaces/default/datasets/probe"),
                        Some(json!({
                            "schema": {
                                "name": "Probe",
                                "fields": [{ "name": "id", "type": "STRING", "isRequired": true }],
                            },
                        })),
                    )
                    .await,
            ),
        ];

        for (what, (status, body)) in calls {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?}: {what} expected 404 ResourceNotFoundException, got {status} \
                 with {body}"
            );
            assert_eq!(
                error_message(&body),
                INSTANCE_NOT_FOUND_SENTENCE,
                "case {case:?}: {what} must carry the fixed not-found sentence, got {body}"
            );
            assert!(
                !error_message(&body).contains(&id),
                "case {case:?}: {what} must not echo the instanceId the request carried, got \
                 {body}"
            );
        }
    }
}

/// RULE: `DeleteInstance` accepts the deletion — the instance reports `Deleting` and stays
/// addressable with the members it carried — and the removal only happens when the clock
/// makes it due, after which the id names no instance at all.
#[tokio::test]
async fn an_accepted_deletion_is_observable_before_the_record_goes() {
    let emulator = Emulator::new();
    let case = "the deletion window";
    let created = emulator
        .create(
            case,
            json!({
                "instanceName": "deletion-window",
                "instanceDescription": "the description before the delete",
                "webAppDnsDomain": "window",
            }),
        )
        .await;
    let id = string_of(case, &created, "instanceId");
    emulator.wait_until_active(case, &id).await;

    let (status, body) = emulator.delete(&id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteInstance expected 200, got {status} with {body}"
    );
    let accepted = instance_of(case, &body);
    assert_eq!(
        string_of(case, &accepted, "state"),
        STATE_DELETING,
        "case {case:?}: the accepted deletion must report Deleting, got {accepted}"
    );

    // Still addressable, in `Deleting`, with every member it carried before the delete.
    let (status, body) = emulator.get(&id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a Deleting instance must still be readable, got {status} with {body}"
    );
    let deleting = instance_of(case, &body);
    for member in ["instanceName", "instanceDescription", "webAppDnsDomain"] {
        assert_eq!(
            string_of(case, &deleting, member),
            string_of(case, &created, member),
            "case {case:?}: {member} must survive the accepted deletion, got {deleting}"
        );
    }
    assert_eq!(
        string_of(case, &deleting, "state"),
        STATE_DELETING,
        "case {case:?}: the stored state must be Deleting, got {deleting}"
    );

    // `DeleteInstance` is `@idempotent`: repeating it while the deletion settles reports
    // the same record rather than a second acceptance or a refusal.
    let (status, body) = emulator.delete(&id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a repeated DeleteInstance expected 200, got {status} with {body}"
    );
    assert_eq!(
        string_of(case, &instance_of(case, &body), "state"),
        STATE_DELETING,
        "case {case:?}: a repeated delete must report the same Deleting record, got {body}"
    );

    // It is also still listed while it settles: the record exists, so a listing reports it.
    let (status, body) = emulator.send("GET", "/api/instance?maxResults=20", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListInstances expected 200, got {status} with {body}"
    );
    let listed: Vec<String> = body["instances"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no `instances` list in {body}"))
        .iter()
        .map(|instance| string_of(case, instance, "instanceId"))
        .collect();
    assert!(
        listed.contains(&id),
        "case {case:?}: a Deleting instance must still be listed, got {listed:?}"
    );

    // The removal is owed by the clock, so the record and its id go without anybody asking.
    emulator.wait_until_deleted(case, &id).await;
    let (status, body) = emulator.get(&id).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (404, "ResourceNotFoundException"),
        "case {case:?}: the settled deletion must leave no instance, got {status} with {body}"
    );
    assert_eq!(
        error_message(&body),
        INSTANCE_NOT_FOUND_SENTENCE,
        "case {case:?}: the read must carry the fixed not-found sentence, got {body}"
    );
}

/// RULE: a mutation of an instance is served only while the instance is `Active` — in every
/// other state it is refused `ValidationException` (400) naming the state requirement, and
/// the instance keeps the members it had.
#[tokio::test]
async fn a_mutation_is_served_only_while_the_instance_is_active() {
    let emulator = Emulator::new();
    let description = "the description a refused update must not change";

    // Rows differ along the state the instance is in when the mutation is issued: the one
    // a create leaves it in, the one the clock moves it to, and the one an accepted
    // deletion moves it to. Each row reads the state back first, so what is asserted is
    // the mapping from STATE to answer rather than from a span of the clock.
    for case in ["straight after the create", "once Active", "while Deleting"] {
        let created = emulator
            .create(
                case,
                json!({ "instanceName": "state-gate", "instanceDescription": description }),
            )
            .await;
        let id = string_of(case, &created, "instanceId");
        match case {
            "once Active" => emulator.wait_until_active(case, &id).await,
            "while Deleting" => {
                emulator.wait_until_active(case, &id).await;
                let (status, body) = emulator.delete(&id).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: DeleteInstance expected 200, got {status} with {body}"
                );
            }
            // "straight after the create": the instance is read as it is, with no wait.
            _ => {}
        }

        let state = emulator
            .state(&id)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the instance must still be addressable"));
        let (status, body) = emulator
            .update(&id, json!({ "instanceDescription": "mutated" }))
            .await;
        if state == STATE_ACTIVE {
            assert_eq!(
                status, 200,
                "case {case:?}: a mutation of an Active instance is served, got {status} with \
                 {body}"
            );
            assert_eq!(
                string_of(case, &instance_of(case, &body), "instanceDescription"),
                "mutated",
                "case {case:?}: the mutation must be reported, got {body}"
            );
            continue;
        }

        assert!(
            state == STATE_INITIALIZING || state == STATE_DELETING,
            "case {case:?}: expected one of the non-Active modelled states, got {state:?}"
        );
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a mutation in state {state:?} must be refused by the state gate, \
             got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            NOT_ACTIVE_SENTENCE,
            "case {case:?}: the refusal must name the state requirement, got {body}"
        );

        // The refused call changed nothing: the stored description is the one the create
        // persisted.
        let (status, body) = emulator.get(&id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the instance must still be readable, got {status} with {body}"
        );
        assert_eq!(
            string_of(case, &instance_of(case, &body), "instanceDescription"),
            description,
            "case {case:?}: a refused mutation must not change the stored description, got {body}"
        );
    }
}
