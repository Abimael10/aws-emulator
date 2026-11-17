// SPDX-License-Identifier: CC0-1.0

//! `GetFlowVersion`, end to end over HTTP: the FIDELITY of the flow definition a
//! numbered version reports.
//!
//! The rule pinned here: a numbered flow version reports the `FlowDefinition`
//! the cut snapshotted, structurally identical to the definition its flow
//! carried at that instant — every node, connection, nested configuration union
//! and optional member, in the order the definition declares them — whatever
//! shape that definition has and however the flow's DRAFT is rebuilt afterwards.
//!
//! Each test drives a TABLE whose rows differ along what the rule generalises
//! over: the node TYPES a definition is built from (and so which arm of the
//! modelled `FlowNodeConfiguration` union each node carries), the connection
//! types wiring them, the nesting depth of a node's configuration, the IO data
//! types, and the COUNT of nodes and connections.
//!
//! `flow_version_reads.rs` covers the reader itself (both `FlowIdentifier`
//! forms, the `includedData` scopes, the refusals) and `flow_version_snapshots.rs`
//! what `CreateFlowVersion` stores; this suite covers what the definition a
//! version packages looks like when it is read back. Every assertion goes
//! through the router, so it exercises the real wire path (route matching, model
//! constraint validation, serialization) rather than calling a handler directly.
//!
//! The one thing these tests reach into the store for is the INSTANT a pending
//! preparation comes due: `PrepareFlow` stores it rather than sleeping, so a
//! test moves it into the past instead of waiting out
//! [`FLOW_PREPARATION_SPAN`](service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state::FLOW_PREPARATION_SPAN).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state::service_state;
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the
/// routing key this service claims on, and the source of the region its ARNs
/// carry.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// A role ARN of the service-linked form the modelled `FlowExecutionRoleArn`
/// pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/(service-role/)?.+$`
/// admits.
const EXECUTION_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForFlows";

/// The model id a `Prompt` node's inline source configuration names. Source: the
/// modelled `PromptModelIdentifier` member of
/// `PromptFlowNodeInlineConfiguration`.
const PROMPT_MODEL: &str = "anthropic.claude-3-haiku-20240307-v1:0";

/// The fallback condition of a condition node. Source: the doc of
/// `FlowCondition$name`, which names the condition taken when no other condition
/// is satisfied.
const DEFAULT_CONDITION: &str = "default";

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    fn string(&self, member: &str) -> String {
        self.body[member]
            .as_str()
            .unwrap_or_else(|| panic!("{member} is not a string in {:?}", self.body))
            .to_string()
    }

    /// The `definition` member, which the caller asserts against the definition
    /// it submitted.
    fn definition(&self) -> &Value {
        &self.body["definition"]
    }

    /// The response body as an object, so two whole answers can be compared
    /// member for member.
    fn object(&self) -> Map<String, Value> {
        self.body
            .as_object()
            .cloned()
            .unwrap_or_else(|| panic!("the body is not an object: {:?}", self.body))
    }
}

/// The emulator under test: one router over one shared engine, exactly as the
/// production router mounts this service.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
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
        Answer { status, body }
    }

    async fn create_flow(&self, case: &str, name: &str, definition: &Value) -> Answer {
        let created = self
            .call(
                "POST",
                "/flows/",
                Some(json!({
                    "name": name,
                    "executionRoleArn": EXECUTION_ROLE,
                    "definition": definition,
                })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreateFlow -> 201, got {created:?}"
        );
        created
    }

    async fn update_flow(
        &self,
        case: &str,
        flow_id: &str,
        name: &str,
        definition: &Value,
    ) -> Answer {
        let updated = self
            .call(
                "PUT",
                &format!("/flows/{flow_id}/"),
                Some(json!({
                    "name": name,
                    "executionRoleArn": EXECUTION_ROLE,
                    "definition": definition,
                })),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {case}: expected UpdateFlow -> 200, got {updated:?}"
        );
        updated
    }

    async fn get_flow(&self, flow_id: &str) -> Answer {
        self.call("GET", &format!("/flows/{flow_id}/"), None).await
    }

    async fn create_flow_version(&self, case: &str, flow_id: &str) -> Answer {
        let cut = self
            .call("POST", &format!("/flows/{flow_id}/versions"), Some(json!({})))
            .await;
        assert_eq!(
            cut.status, 201,
            "case {case}: expected CreateFlowVersion -> 201, got {cut:?}"
        );
        cut
    }

    async fn get_flow_version(
        &self,
        flow_id: &str,
        version: &str,
        included_data: Option<&str>,
    ) -> Answer {
        let query = included_data.map_or_else(String::new, |value| format!("?includedData={value}"));
        self.call(
            "GET",
            &format!("/flows/{flow_id}/versions/{version}/{query}"),
            None,
        )
        .await
    }

    /// `PrepareFlow`, then the instant its outcome comes due moved into the
    /// past, so the flow is `Prepared` for the next caller without the test
    /// waiting out the preparation span. A version is cut from a prepared DRAFT,
    /// which is the state every cut below starts from.
    async fn prepare(&self, case: &str, flow_id: &str) {
        let prepared = self
            .call("POST", &format!("/flows/{flow_id}/"), Some(json!({})))
            .await;
        assert_eq!(
            prepared.status, 202,
            "case {case}: expected PrepareFlow -> 202, got {prepared:?}"
        );
        {
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
        let read = self.get_flow(flow_id).await;
        assert_eq!(
            read.string("status"),
            "Prepared",
            "case {case}: expected the flow to reach Prepared, got {read:?}"
        );
    }
}

/// An `Input` node wired straight to an `Output` node, the two data types
/// carried end to end. The node types, the union keys and the IO data types are
/// the modelled `FlowNodeType` / `FlowNodeConfiguration` / `FlowNodeIODataType`
/// values.
fn passthrough_definition(data_type: &str) -> Value {
    json!({
        "nodes": [
            {
                "name": "FlowInput",
                "type": "Input",
                "configuration": { "input": {} },
                "outputs": [{ "name": "document", "type": data_type }],
            },
            {
                "name": "FlowOutput",
                "type": "Output",
                "configuration": { "output": {} },
                "inputs": [{ "name": "document", "type": data_type, "expression": "$.data" }],
            },
        ],
        "connections": [
            {
                "name": "InputToOutput",
                "type": "Data",
                "source": "FlowInput",
                "target": "FlowOutput",
                "configuration": {
                    "data": { "sourceOutput": "document", "targetInput": "document" },
                },
            },
        ],
    })
}

/// One `Input` node feeding two `Output` nodes: a definition whose node and
/// connection counts differ from the passthrough's.
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
                    "data": { "sourceOutput": "document", "targetInput": "document" },
                },
            },
            {
                "name": "InputToRight",
                "type": "Data",
                "source": "FlowInput",
                "target": "RightOutput",
                "configuration": {
                    "data": { "sourceOutput": "document", "targetInput": "document" },
                },
            },
        ],
    })
}

/// A definition whose middle node is a `Prompt` node: the deepest nesting a
/// definition reaches here, down through the modelled
/// `PromptFlowNodeSourceConfiguration` / `PromptFlowNodeInlineConfiguration` /
/// `PromptTemplateConfiguration` / `PromptModelInferenceConfiguration` members,
/// and the only row carrying numeric and list members inside a node
/// configuration.
fn prompt_definition() -> Value {
    json!({
        "nodes": [
            {
                "name": "FlowInput",
                "type": "Input",
                "configuration": { "input": {} },
                "outputs": [{ "name": "document", "type": "String" }],
            },
            {
                "name": "Generate",
                "type": "Prompt",
                "configuration": {
                    "prompt": {
                        "sourceConfiguration": {
                            "inline": {
                                "templateType": "TEXT",
                                "templateConfiguration": {
                                    "text": {
                                        "text": "Summarise {{document}}",
                                        "inputVariables": [{ "name": "document" }],
                                    },
                                },
                                "modelId": PROMPT_MODEL,
                                // `Temperature` and `TopP` are modelled `float`,
                                // so the values here are ones 32-bit width holds
                                // exactly; `MaxTokens` is an integer.
                                "inferenceConfiguration": {
                                    "text": {
                                        "temperature": 0.5,
                                        "topP": 0.75,
                                        "maxTokens": 512,
                                        "stopSequences": ["STOP", "HALT"],
                                    },
                                },
                            },
                        },
                    },
                },
                "inputs": [{ "name": "document", "type": "String", "expression": "$.data" }],
                "outputs": [{ "name": "modelCompletion", "type": "String" }],
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
                "name": "InputToPrompt",
                "type": "Data",
                "source": "FlowInput",
                "target": "Generate",
                "configuration": {
                    "data": { "sourceOutput": "document", "targetInput": "document" },
                },
            },
            {
                "name": "PromptToOutput",
                "type": "Data",
                "source": "Generate",
                "target": "FlowOutput",
                "configuration": {
                    "data": { "sourceOutput": "modelCompletion", "targetInput": "document" },
                },
            },
        ],
    })
}

/// A definition routing its input through a `Condition` node: the only row whose
/// connections include a `Conditional` one, and whose node configuration holds a
/// LIST of structures — one of them (the fallback) with its optional
/// `expression` member left out entirely.
fn conditional_definition() -> Value {
    json!({
        "nodes": [
            {
                "name": "FlowInput",
                "type": "Input",
                "configuration": { "input": {} },
                "outputs": [{ "name": "document", "type": "String" }],
            },
            {
                "name": "Check",
                "type": "Condition",
                "configuration": {
                    "condition": {
                        "conditions": [
                            { "name": "Answered", "expression": "answer != \"\"" },
                            { "name": DEFAULT_CONDITION },
                        ],
                    },
                },
                "inputs": [{ "name": "answer", "type": "String", "expression": "$.data" }],
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
                "name": "InputToCheck",
                "type": "Data",
                "source": "FlowInput",
                "target": "Check",
                "configuration": {
                    "data": { "sourceOutput": "document", "targetInput": "answer" },
                },
            },
            {
                "name": "CheckToOutput",
                "type": "Conditional",
                "source": "Check",
                "target": "FlowOutput",
                "configuration": { "conditional": { "condition": "Answered" } },
            },
            {
                "name": "InputToOutput",
                "type": "Data",
                "source": "FlowInput",
                "target": "FlowOutput",
                "configuration": {
                    "data": { "sourceOutput": "document", "targetInput": "document" },
                },
            },
        ],
    })
}

/// A definition whose input is carried through `length` `Collector` nodes in a
/// chain before reaching the ending node: the row that varies the COUNT of nodes
/// and connections, and the order they are declared in.
fn chain_definition(length: usize) -> Value {
    // Node names matching the modelled `FlowNodeName` pattern
    // `^[a-zA-Z]([_]?[0-9a-zA-Z]){1,50}$`.
    let names: Vec<String> = (1..=length).map(|at| format!("Carry{at}")).collect();

    let mut nodes = vec![json!({
        "name": "FlowInput",
        "type": "Input",
        "configuration": { "input": {} },
        "outputs": [{ "name": "document", "type": "Array" }],
    })];
    for name in &names {
        nodes.push(json!({
            "name": name,
            "type": "Collector",
            "configuration": { "collector": {} },
            "inputs": [{ "name": "carried", "type": "Array", "expression": "$.data" }],
            "outputs": [{ "name": "carried", "type": "Array" }],
        }));
    }
    nodes.push(json!({
        "name": "FlowOutput",
        "type": "Output",
        "configuration": { "output": {} },
        "inputs": [{ "name": "document", "type": "Array", "expression": "$.data" }],
    }));

    let mut connections = vec![json!({
        "name": "InputToChain",
        "type": "Data",
        "source": "FlowInput",
        "target": names[0],
        "configuration": { "data": { "sourceOutput": "document", "targetInput": "carried" } },
    })];
    for pair in names.windows(2) {
        connections.push(json!({
            "name": format!("{}To{}", pair[0], pair[1]),
            "type": "Data",
            "source": pair[0],
            "target": pair[1],
            "configuration": { "data": { "sourceOutput": "carried", "targetInput": "carried" } },
        }));
    }
    connections.push(json!({
        "name": "ChainToOutput",
        "type": "Data",
        "source": names[names.len() - 1],
        "target": "FlowOutput",
        "configuration": { "data": { "sourceOutput": "carried", "targetInput": "document" } },
    }));

    json!({ "nodes": nodes, "connections": connections })
}

/// The definition every row's DRAFT is rebuilt with after its version has been
/// cut, so no row's version can report the DRAFT and still pass.
fn replacement_definition() -> Value {
    json!({
        "nodes": [
            {
                "name": "FlowInput",
                "type": "Input",
                "configuration": { "input": {} },
                "outputs": [{ "name": "document", "type": "Boolean" }],
            },
            {
                "name": "RebuiltOutput",
                "type": "Output",
                "configuration": { "output": {} },
                "inputs": [{ "name": "document", "type": "Boolean", "expression": "$.data" }],
            },
        ],
        "connections": [
            {
                "name": "RebuiltInputToOutput",
                "type": "Data",
                "source": "FlowInput",
                "target": "RebuiltOutput",
                "configuration": {
                    "data": { "sourceOutput": "document", "targetInput": "document" },
                },
            },
        ],
    })
}

/// Rows spanning the definition shapes the rule generalises over: the node types
/// each is built from, the connection types, the nesting depth of a node
/// configuration, the IO data types and the node and connection counts.
fn definition_cases() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "an Input node wired straight to an Output node",
            "fidelity-passthrough",
            passthrough_definition("String"),
        ),
        (
            "one Input node feeding two Output nodes",
            "fidelity-fanout",
            fan_out_definition(),
        ),
        (
            "a Prompt node carrying a nested inline inference configuration",
            "fidelity-prompt",
            prompt_definition(),
        ),
        (
            "a Condition node reached by a Conditional connection",
            // The one row whose flow name uses the underscore separator the
            // modelled `FlowName` pattern `^([0-9a-zA-Z][_-]?){1,100}$` admits
            // beside the hyphen every other row carries.
            "fidelity_conditional",
            conditional_definition(),
        ),
        (
            "a chain of one Collector node",
            "fidelity-chain-1",
            chain_definition(1),
        ),
        (
            "a chain of three Collector nodes",
            "fidelity-chain-3",
            chain_definition(3),
        ),
    ]
}

/// The `includedData` values that report the definition. Source: the modelled
/// `IncludedData` enum (`ALL_DATA | METADATA_ONLY`) and the doc of the member
/// itself, which equates omitting it with `ALL_DATA`.
const SCOPES_REPORTING_THE_DEFINITION: &[(&str, Option<&str>)] = &[
    ("includedData omitted", None),
    ("includedData=ALL_DATA", Some("ALL_DATA")),
];

/// The rule: whatever shape a flow's definition has, the version cut from it
/// reports that definition structurally unchanged — and goes on reporting it
/// after the DRAFT has been rebuilt around a different one.
#[tokio::test]
async fn a_version_reports_the_definition_it_was_cut_from_whatever_its_shape() {
    let emulator = Emulator::new();
    let replacement = replacement_definition();

    for (case, name, definition) in definition_cases() {
        assert_ne!(
            definition, replacement,
            "case {case}: the row's definition must differ from the one the DRAFT is \
             rebuilt with, or the immutability assertions below prove nothing"
        );

        // The definition survives the create, which is the first leg of the
        // round trip: a loss here would hide a loss later.
        let created = emulator.create_flow(case, name, &definition).await;
        let flow_id = created.string("id");
        assert_eq!(
            created.definition(),
            &definition,
            "case {case}: expected CreateFlow to report the definition submitted, got {created:?}"
        );

        emulator.prepare(case, &flow_id).await;
        let cut = emulator.create_flow_version(case, &flow_id).await;
        assert_eq!(
            cut.string("version"),
            "1",
            "case {case}: the first version cut is numbered 1, got {cut:?}"
        );
        assert_eq!(
            cut.definition(),
            &definition,
            "case {case}: expected the cut to package the definition submitted, got {cut:?}"
        );

        // The DRAFT is rebuilt around a definition of another shape, so a read
        // that reported the flow record rather than the snapshot would answer
        // the replacement.
        let updated = emulator
            .update_flow(case, &flow_id, name, &replacement)
            .await;
        assert_eq!(
            updated.definition(),
            &replacement,
            "case {case}: expected UpdateFlow to report the rebuilt definition, got {updated:?}"
        );

        for (scope, included_data) in SCOPES_REPORTING_THE_DEFINITION {
            let read = emulator
                .get_flow_version(&flow_id, "1", *included_data)
                .await;
            assert_eq!(
                read.status, 200,
                "case {case} ({scope}): expected GetFlowVersion -> 200, got {read:?}"
            );
            assert_eq!(
                read.definition(),
                &definition,
                "case {case} ({scope}): expected the version to keep the definition it was \
                 cut from, got {read:?}"
            );
            assert_eq!(
                read.object(),
                cut.object(),
                "case {case} ({scope}): expected the read to report the version the cut \
                 stored, got {read:?}"
            );
        }

        // The scope that reports metadata only leaves the definition out, and
        // moves nothing else about the version.
        let metadata_only = emulator
            .get_flow_version(&flow_id, "1", Some("METADATA_ONLY"))
            .await;
        assert_eq!(
            metadata_only.status, 200,
            "case {case}: expected GetFlowVersion(METADATA_ONLY) -> 200, got {metadata_only:?}"
        );
        assert!(
            metadata_only.body.get("definition").is_none(),
            "case {case}: expected METADATA_ONLY to leave the definition out, got \
             {metadata_only:?}"
        );
        let mut expected = cut.object();
        let mut actual = metadata_only.object();
        expected.remove("definition");
        actual.remove("definition");
        assert_eq!(
            actual, expected,
            "case {case}: expected METADATA_ONLY to move the definition alone, got \
             {metadata_only:?}"
        );

        // And the DRAFT reports the rebuilt definition, so the two really are
        // different resources.
        let draft = emulator.get_flow(&flow_id).await;
        assert_eq!(
            draft.definition(),
            &replacement,
            "case {case}: expected the DRAFT to report the rebuilt definition, got {draft:?}"
        );
    }
}

/// The same rule where a flow is cut SEVERAL times as its definition is rebuilt
/// between cuts: each numbered version reports the definition that was in place
/// at its own cut, and reading one leaves every other one as it was.
#[tokio::test]
async fn each_version_of_one_flow_keeps_its_own_definition() {
    let case = "one flow cut at a version per definition shape";
    let emulator = Emulator::new();
    let shapes = definition_cases();

    let (_, name, first) = &shapes[0];
    let created = emulator.create_flow(case, name, first).await;
    let flow_id = created.string("id");
    emulator.prepare(case, &flow_id).await;

    // Version N is cut from the Nth row's definition, so the store holds one
    // version per definition shape under one flow.
    let mut cuts: Vec<(String, Value)> = Vec::new();
    for (at, (shape, name, definition)) in shapes.iter().enumerate() {
        if at > 0 {
            emulator
                .update_flow(shape, &flow_id, name, definition)
                .await;
            emulator.prepare(shape, &flow_id).await;
        }
        let cut = emulator.create_flow_version(shape, &flow_id).await;
        let version = cut.string("version");
        assert_eq!(
            version,
            (at + 1).to_string(),
            "case {case} ({shape}): expected the next version to take the next number, got {cut:?}"
        );
        assert_eq!(
            cut.definition(),
            definition,
            "case {case} ({shape}): expected the cut to package the definition in place, \
             got {cut:?}"
        );
        cuts.push((version, definition.clone()));
    }

    // Read in reverse, so a read of a later version cannot be what makes an
    // earlier one answer correctly.
    for (version, definition) in cuts.iter().rev() {
        let read = emulator.get_flow_version(&flow_id, version, None).await;
        assert_eq!(
            read.status, 200,
            "case {case} (version {version}): expected GetFlowVersion -> 200, got {read:?}"
        );
        assert_eq!(
            read.definition(),
            definition,
            "case {case} (version {version}): expected the version's own definition, got {read:?}"
        );
    }

    // Every version is still readable and unchanged after all of those reads: a
    // read of one version moves nothing a later caller sees.
    for (version, definition) in &cuts {
        let reread = emulator.get_flow_version(&flow_id, version, None).await;
        assert_eq!(
            reread.definition(),
            definition,
            "case {case} (version {version}): expected a repeated read to report the same \
             definition, got {reread:?}"
        );
    }
}
