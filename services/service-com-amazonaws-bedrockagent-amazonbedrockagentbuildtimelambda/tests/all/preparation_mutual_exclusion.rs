// SPDX-License-Identifier: CC0-1.0

//! Mutual exclusion over the preparation of ONE resource: `PrepareFlow` and
//! `PrepareAgent`, end to end over HTTP.
//!
//! A resource is prepared by one preparation at a time, so of any number of
//! callers asking to prepare the same resource while a preparation it already
//! accepted is unfinished, exactly one is accepted (202, the transient preparing
//! status) and every other is refused the modelled `ConflictException` (409)
//! carrying the service's concurrent-request sentence. Once nothing is in flight
//! the next caller is accepted again: the refusal is a serialization point, not a
//! permanent one.
//!
//! The tables span the class rather than one race: rows differ in the NUMBER of
//! racers, in which modelled identifier form each racer addresses the resource by
//! (a bare id and an ARN both name the same flow), in the outcome the one
//! preparation reaches (`Prepared` and `Failed`, with and without findings), and
//! in how the flow left the in-flight state before it is prepared again.
//!
//! Every assertion goes through the router, so it exercises the real wire path
//! (route matching, model constraint validation, serialization) rather than
//! calling a handler directly.
//!
//! Nothing here waits on the wall clock: the handler STORES the instant the
//! preparation it accepted comes due, so a test moves that instant into the past
//! instead of sleeping out the span.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state::service_state;
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the
/// routing key this service claims on.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// The account id every emulated ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// A role ARN matching the modelled `FlowExecutionRoleArn` pattern.
const EXECUTION_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForFlows";

/// The sentence a refused concurrent claim carries. Its only variable part is the
/// refused operation's name; every other word is fixed, and nothing in it comes
/// from the request or from state.
fn concurrent_conflict_message(operation: &str) -> String {
    format!(
        "Couldn't perform {operation} operation. The resource is being updated by a \
         concurrent request. Retry your request after the other request has completed."
    )
}

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The restJson1 error shape name, from `x-amzn-errortype` or the body's
    /// `__type`; `None` on a success.
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    fn member(&self, path: &[&str]) -> &Value {
        let mut at = &self.body;
        for key in path {
            at = &at[*key];
        }
        at
    }

    fn string(&self, path: &[&str]) -> String {
        self.member(path)
            .as_str()
            .unwrap_or_else(|| panic!("{path:?} is not a string in {:?}", self.body))
            .to_string()
    }

    /// The `message` of a refusal.
    fn message(&self) -> String {
        self.string(&["message"])
    }
}

/// The emulator under test: one router over one shared engine, exactly as the
/// production router mounts this service.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    /// The emulator as an EXISTING customer's account sees it: the account's
    /// prior usage of the agent surface is recorded, because that surface is
    /// closed to an account with none (`tests/all/agents_classic_maintenance_mode.rs`
    /// is that rule's suite) and the agent race below drives an agent that
    /// exists.
    fn new() -> Arc<Self> {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        service_state(&*engine)
            .expect("the bedrock-agent store is reachable through the shared engine")
            .lock()
            .agents_classic_usage
            .record(ACCOUNT);
        Arc::new(Self { router, engine })
    }

    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let bytes = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("serialize request body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "bedrock-agent.eu-west-1.amazonaws.com")
            .header("authorization", AUTHORIZATION)
            .header("content-type", "application/json")
            .body(bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_error = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let raw = response.into_body();
        let body: Value = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or_else(|e| {
                panic!(
                    "{method} {uri} -> {status} body is not JSON ({e}): {}",
                    String::from_utf8_lossy(&raw)
                )
            })
        };
        let error_type = header_error.or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        });
        Answer {
            status,
            error_type,
            body,
        }
    }

    /// `CreateFlow`, answering the whole response so a case can take the flow's
    /// id and its ARN — the two modelled `FlowIdentifier` forms.
    async fn create_flow(&self, case: &str, request: Value) -> Answer {
        let created = self.call("POST", "/flows/", Some(request)).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreateFlow -> 201, got {created:?}"
        );
        created
    }

    async fn update_flow(&self, identifier: &str, request: Value) -> Answer {
        let escaped = escape(identifier);
        self.call("PUT", &format!("/flows/{escaped}/"), Some(request))
            .await
    }

    async fn prepare_flow(&self, identifier: &str) -> Answer {
        let escaped = escape(identifier);
        self.call("POST", &format!("/flows/{escaped}/"), Some(json!({})))
            .await
    }

    async fn get_flow(&self, identifier: &str) -> Answer {
        let escaped = escape(identifier);
        self.call("GET", &format!("/flows/{escaped}/"), None).await
    }

    async fn create_agent(&self, case: &str, request: Value) -> Answer {
        let created = self.call("PUT", "/agents/", Some(request)).await;
        assert_eq!(
            created.status, 202,
            "case {case}: expected CreateAgent -> 202, got {created:?}"
        );
        created
    }

    async fn prepare_agent(&self, agent_id: &str) -> Answer {
        self.call("POST", &format!("/agents/{agent_id}/"), Some(json!({})))
            .await
    }

    async fn get_agent(&self, agent_id: &str) -> Answer {
        self.call("GET", &format!("/agents/{agent_id}/"), None).await
    }

    /// Whether the store still owes this flow a preparation.
    fn flow_preparation_in_flight(&self, flow_id: &str) -> bool {
        let state = service_state(&*self.engine).expect("the bedrock-agent store");
        let state = state.lock();
        state
            .flows
            .get(flow_id)
            .expect("the stored flow")
            .pending_preparation
            .is_some()
    }

    /// Move the instant the flow's pending preparation comes due into the past,
    /// so the next caller observes its outcome without the test sleeping.
    fn settle_flow_preparation(&self, case: &str, flow_id: &str) {
        let state = service_state(&*self.engine).expect("the bedrock-agent store");
        let mut state = state.lock();
        let pending = state
            .flows
            .get_mut(flow_id)
            .expect("the stored flow")
            .pending_preparation
            .as_mut()
            .unwrap_or_else(|| panic!("case {case}: expected a preparation in flight"));
        pending.due = SystemTime::now() - Duration::from_secs(1);
    }

    /// The same for an agent: the status the clock owes it comes due at once.
    fn settle_agent_preparation(&self, case: &str, agent_id: &str) {
        let state = service_state(&*self.engine).expect("the bedrock-agent store");
        let mut state = state.lock();
        let record = state
            .agents
            .get_mut(agent_id)
            .expect("the stored agent");
        let Some((due, _)) = record.pending_status.as_mut() else {
            panic!("case {case}: expected a preparation in flight");
        };
        *due = SystemTime::now() - Duration::from_secs(1);
    }
}

/// An `@httpLabel` value as a request line carries it.
fn escape(identifier: &str) -> String {
    identifier.replace(':', "%3A").replace('/', "%2F")
}

/// An `instruction` long enough for the member's modelled `length 40..4000`.
fn instruction() -> String {
    "You are a booking assistant. Answer the customer precisely and cite your sources."
        .to_string()
}

/// A passthrough flow definition: an `Input` node wired straight to an `Output`
/// node. The node types, the union keys and the IO data types are the modelled
/// `FlowNodeType` / `FlowNodeConfiguration` / `FlowNodeIODataType` values.
fn passthrough_definition() -> Value {
    json!({
        "nodes": [
            {
                "name": "FlowInput",
                "type": "Input",
                "configuration": { "input": {} },
                "outputs": [{ "name": "document", "type": "String" }],
            },
            {
                "name": "FlowOutput",
                "type": "Output",
                "configuration": { "output": {} },
                "inputs": [{ "name": "document", "type": "String", "expression": "$.data" }],
            },
        ],
        "connections": [
            {
                "name": "InputToOutput",
                "type": "Data",
                "source": "FlowInput",
                "target": "FlowOutput",
                "configuration": {
                    "data": { "sourceOutput": "document", "targetInput": "document" }
                },
            },
        ],
    })
}

/// The same `Input` fanned out to two `Output` nodes: a valid definition that
/// differs from [`passthrough_definition`] in node and connection COUNT.
fn fan_out_definition() -> Value {
    json!({
        "nodes": [
            {
                "name": "FlowInput",
                "type": "Input",
                "configuration": { "input": {} },
                "outputs": [{ "name": "document", "type": "Object" }],
            },
            {
                "name": "LeftOutput",
                "type": "Output",
                "configuration": { "output": {} },
                "inputs": [{ "name": "document", "type": "Object", "expression": "$.data.left" }],
            },
            {
                "name": "RightOutput",
                "type": "Output",
                "configuration": { "output": {} },
                "inputs": [{ "name": "document", "type": "Object", "expression": "$.data.right" }],
            },
        ],
        "connections": [
            {
                "name": "InputToLeft",
                "type": "Data",
                "source": "FlowInput",
                "target": "LeftOutput",
                "configuration": {
                    "data": { "sourceOutput": "document", "targetInput": "document" }
                },
            },
            {
                "name": "InputToRight",
                "type": "Data",
                "source": "FlowInput",
                "target": "RightOutput",
                "configuration": {
                    "data": { "sourceOutput": "document", "targetInput": "document" }
                },
            },
        ],
    })
}

/// A definition with a starting node and nothing to end at, so the one
/// preparation that runs reaches `Failed` and publishes findings.
fn input_only_definition() -> Value {
    json!({
        "nodes": [
            {
                "name": "FlowInput",
                "type": "Input",
                "configuration": { "input": {} },
                "outputs": [{ "name": "document", "type": "String" }],
            },
        ],
        "connections": [],
    })
}

/// One race for the preparation of one flow.
struct FlowRace {
    /// What makes this row different from the others.
    name: &'static str,
    /// How many callers ask to prepare the same flow at once.
    racers: usize,
    /// The `definition` member `CreateFlow` is given, or `None` to omit it.
    definition: Option<Value>,
    /// The `FlowStatus` the ONE preparation that ran reaches.
    outcome: &'static str,
    /// Whether that outcome publishes findings in `validations`.
    publishes_findings: bool,
}

/// The table: rows differ in the number of racers, in the size of the definition
/// being packaged, in whether a definition is stored at all, and in the outcome
/// the single preparation reaches.
fn flow_races() -> Vec<FlowRace> {
    vec![
        FlowRace {
            name: "two racers over a passthrough definition",
            racers: 2,
            definition: Some(passthrough_definition()),
            outcome: "Prepared",
            publishes_findings: false,
        },
        FlowRace {
            name: "three racers over a three-node definition",
            racers: 3,
            definition: Some(fan_out_definition()),
            outcome: "Prepared",
            publishes_findings: false,
        },
        FlowRace {
            name: "five racers over a definition with no ending node",
            racers: 5,
            definition: Some(input_only_definition()),
            outcome: "Failed",
            publishes_findings: true,
        },
        FlowRace {
            name: "four racers over a flow carrying no definition at all",
            racers: 4,
            definition: None,
            outcome: "Failed",
            publishes_findings: true,
        },
    ]
}

/// Of N callers racing to prepare one flow, exactly ONE is accepted and every
/// other is refused `ConflictException` (409) with the service's
/// concurrent-request sentence — after which the flow settles at the outcome of
/// the single preparation that ran.
#[tokio::test]
async fn concurrent_prepare_flow_admits_exactly_one_claim() {
    for race in flow_races() {
        let case = race.name;
        let emulator = Emulator::new();

        let mut request = json!({ "name": "racedFlow", "executionRoleArn": EXECUTION_ROLE });
        if let Some(definition) = race.definition.clone() {
            request["definition"] = definition;
        }
        let created = emulator.create_flow(case, request).await;
        let flow_id = created.string(&["id"]);
        let flow_arn = created.string(&["arn"]);

        // Each racer addresses the flow by one of the two modelled
        // `FlowIdentifier` forms, which name the same flow: the winner is decided
        // by the flow's state, not by how the request spelled it.
        let mut running = Vec::new();
        for racer in 0..race.racers {
            let emulator = Arc::clone(&emulator);
            let identifier = if racer % 2 == 0 {
                flow_id.clone()
            } else {
                flow_arn.clone()
            };
            running.push(tokio::spawn(async move {
                emulator.prepare_flow(&identifier).await
            }));
        }

        let mut winners = 0usize;
        for (racer, handle) in running.into_iter().enumerate() {
            let answer = handle.await.expect("the racing PrepareFlow task completed");
            match answer.status {
                202 => {
                    winners += 1;
                    assert_eq!(
                        answer.string(&["status"]),
                        "Preparing",
                        "case {case}, racer {racer}: an accepted claim reports the flow being \
                         prepared, got {answer:?}"
                    );
                    assert_eq!(
                        answer.string(&["id"]),
                        flow_id,
                        "case {case}, racer {racer}: the accepted claim names the flow it \
                         prepares, got {answer:?}"
                    );
                }
                409 => {
                    assert_eq!(
                        answer.error_type.as_deref(),
                        Some("ConflictException"),
                        "case {case}, racer {racer}: a losing claim is refused \
                         ConflictException, got {answer:?}"
                    );
                    assert_eq!(
                        answer.message(),
                        concurrent_conflict_message("PrepareFlow"),
                        "case {case}, racer {racer}: expected the concurrent-request \
                         sentence, got {answer:?}"
                    );
                }
                other => panic!(
                    "case {case}, racer {racer}: expected 202 or 409, got {other} in {answer:?}"
                ),
            }
        }
        assert_eq!(
            winners, 1,
            "case {case}: expected exactly 1 of {} racers to start the preparation, {winners} did",
            race.racers
        );

        // Exactly one preparation is in flight for the flow, and the flow reports
        // it.
        let during = emulator.get_flow(&flow_id).await;
        assert_eq!(
            during.string(&["status"]),
            "Preparing",
            "case {case}: the flow reports the one preparation in flight, got {:?}",
            during.body
        );

        // The one claim that was accepted is the one that settles: after its
        // outcome is due the flow owes nothing more, so no loser left a second
        // preparation behind.
        emulator.settle_flow_preparation(case, &flow_id);
        let after = emulator.get_flow(&flow_id).await;
        assert_eq!(
            after.string(&["status"]),
            race.outcome,
            "case {case}: expected the flow to settle at {}, got {:?}",
            race.outcome,
            after.body
        );
        assert!(
            !emulator.flow_preparation_in_flight(&flow_id),
            "case {case}: only one preparation ran, so none may still be owed"
        );
        let findings = after
            .member(&["validations"])
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: expected a validations list in {:?}", after.body))
            .len();
        assert_eq!(
            findings > 0,
            race.publishes_findings,
            "case {case}: expected publishes_findings={}, got {findings} findings in {:?}",
            race.publishes_findings,
            after.member(&["validations"])
        );
    }
}

/// The refusal serializes preparations; it does not end them. Once no preparation
/// is in flight the next `PrepareFlow` is accepted again — however the flow left
/// the in-flight state.
#[tokio::test]
async fn prepare_flow_is_accepted_again_once_nothing_is_in_flight() {
    /// How a row takes the flow out of the in-flight state.
    #[derive(Debug, Clone, Copy)]
    enum Leave {
        /// The preparation ran to its outcome.
        Settled,
        /// `UpdateFlow` replaced the definition being prepared, which cancels
        /// the preparation of the definition it replaced.
        Updated,
    }

    // Rows differ in how the flow left the in-flight state and in the outcome the
    // first preparation was heading for.
    let rows: Vec<(&str, Leave, Option<Value>)> = vec![
        (
            "settled at Prepared",
            Leave::Settled,
            Some(passthrough_definition()),
        ),
        (
            "settled at Failed",
            Leave::Settled,
            Some(input_only_definition()),
        ),
        (
            "cancelled by an update",
            Leave::Updated,
            Some(passthrough_definition()),
        ),
    ];

    for (case, leave, definition) in rows {
        let emulator = Emulator::new();
        let mut request = json!({ "name": "reprepared", "executionRoleArn": EXECUTION_ROLE });
        if let Some(definition) = definition.clone() {
            request["definition"] = definition;
        }
        let created = emulator.create_flow(case, request).await;
        let flow_id = created.string(&["id"]);

        let first = emulator.prepare_flow(&flow_id).await;
        assert_eq!(
            first.status, 202,
            "case {case}: expected the first PrepareFlow -> 202, got {first:?}"
        );

        // While that one is in flight, a second claim is refused.
        let refused = emulator.prepare_flow(&flow_id).await;
        assert_eq!(
            refused.status, 409,
            "case {case}: expected a second claim -> 409, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            concurrent_conflict_message("PrepareFlow"),
            "case {case}: expected the concurrent-request sentence, got {refused:?}"
        );

        match leave {
            Leave::Settled => emulator.settle_flow_preparation(case, &flow_id),
            Leave::Updated => {
                let mut update =
                    json!({ "name": "reprepared", "executionRoleArn": EXECUTION_ROLE });
                if let Some(definition) = definition.clone() {
                    update["definition"] = definition;
                }
                let updated = emulator.update_flow(&flow_id, update).await;
                assert_eq!(
                    updated.status, 200,
                    "case {case}: expected UpdateFlow -> 200, got {updated:?}"
                );
            }
        }

        let again = emulator.prepare_flow(&flow_id).await;
        assert_eq!(
            again.status, 202,
            "case {case}: once nothing is in flight the flow prepares again, got {again:?}"
        );
        assert_eq!(
            again.string(&["status"]),
            "Preparing",
            "case {case}: the accepted claim reports the flow being prepared, got {again:?}"
        );
    }
}

/// The same rule over the agent's preparation: of N callers racing to prepare one
/// agent, exactly one is accepted and every other is refused
/// `ConflictException` (409) with the sentence naming `PrepareAgent`; once the
/// preparation has finished the agent prepares again.
#[tokio::test]
async fn concurrent_prepare_agent_admits_exactly_one_claim() {
    // Rows differ in how many callers race for the one agent.
    for racers in [2usize, 3, 5] {
        let case = format!("{racers} racing PrepareAgent calls");
        let emulator = Emulator::new();
        let created = emulator
            .create_agent(
                &case,
                json!({
                    "agentName": "racedAgent",
                    "instruction": instruction(),
                    "foundationModel": "anthropic.claude-v2",
                }),
            )
            .await;
        let agent_id = created.string(&["agent", "agentId"]);

        let mut running = Vec::new();
        for _ in 0..racers {
            let emulator = Arc::clone(&emulator);
            let agent_id = agent_id.clone();
            running.push(tokio::spawn(async move {
                emulator.prepare_agent(&agent_id).await
            }));
        }

        let mut winners = 0usize;
        for (racer, handle) in running.into_iter().enumerate() {
            let answer = handle.await.expect("the racing PrepareAgent task completed");
            match answer.status {
                202 => {
                    winners += 1;
                    assert_eq!(
                        answer.string(&["agentStatus"]),
                        "PREPARING",
                        "case {case}, racer {racer}: an accepted claim reports the agent being \
                         prepared, got {answer:?}"
                    );
                }
                409 => {
                    assert_eq!(
                        answer.error_type.as_deref(),
                        Some("ConflictException"),
                        "case {case}, racer {racer}: a losing claim is refused \
                         ConflictException, got {answer:?}"
                    );
                    assert_eq!(
                        answer.message(),
                        concurrent_conflict_message("PrepareAgent"),
                        "case {case}, racer {racer}: expected the concurrent-request \
                         sentence, got {answer:?}"
                    );
                }
                other => panic!(
                    "case {case}, racer {racer}: expected 202 or 409, got {other} in {answer:?}"
                ),
            }
        }
        assert_eq!(
            winners, 1,
            "case {case}: expected exactly 1 of {racers} racers to start the preparation, \
             {winners} did"
        );

        // The single preparation settles, and the agent prepares again after it.
        emulator.settle_agent_preparation(&case, &agent_id);
        let settled = emulator.get_agent(&agent_id).await;
        assert_eq!(
            settled.string(&["agent", "agentStatus"]),
            "PREPARED",
            "case {case}: expected the agent to settle at PREPARED, got {settled:?}"
        );

        let again = emulator.prepare_agent(&agent_id).await;
        assert_eq!(
            again.status, 202,
            "case {case}: once nothing is in flight the agent prepares again, got {again:?}"
        );
    }
}
