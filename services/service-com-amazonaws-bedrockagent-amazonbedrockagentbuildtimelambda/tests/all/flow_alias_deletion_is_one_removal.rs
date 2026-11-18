// SPDX-License-Identifier: CC0-1.0

//! `DeleteFlowAlias` takes exactly ONE alias out of the store — the alias the
//! request addresses — and leaves every other resource of the flow byte for
//! byte as it was: the flow document, the version the alias routed to, the
//! flow's other aliases (the reserved test alias included) and the order the
//! listing reports them in.
//!
//! The rows below span what the rule generalises over: how many aliases the
//! flow holds when the delete arrives (one, two, five) and which position in
//! creation order the deleted one occupies (first, last, middle) — and then,
//! in a second test, the order the whole set is deleted in (creation order and
//! its reverse), one call removing one alias each time.
//!
//! Every assertion goes through the router, so it exercises the real wire path
//! (route matching, model constraint validation, serialization) rather than
//! calling the handler directly.

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

/// A role ARN matching the modelled `FlowExecutionRoleArn` pattern
/// `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/(service-role/)?.+$`.
const EXECUTION_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForFlows";

/// The reserved alias id every flow carries an alias under, as the modelled
/// `FlowAliasId` (`pattern ^(TSTALIASID|[0-9a-zA-Z]{10})$`) admits it.
const TEST_ALIAS_ID: &str = "TSTALIASID";

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

    /// The id of every reported alias summary, in the order the response listed
    /// them.
    fn listed_ids(&self) -> Vec<String> {
        self.member(&["flowAliasSummaries"])
            .as_array()
            .unwrap_or_else(|| panic!("flowAliasSummaries is not an array in {:?}", self.body))
            .iter()
            .map(|summary| {
                summary["id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a listed alias carries no id: {summary:?}"))
                    .to_string()
            })
            .collect()
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

    /// A prepared flow cut into one numbered version: the state an alias is
    /// created against. Answers the flow's id.
    async fn flow(&self, case: &str, name: &str) -> String {
        let created = self
            .call(
                "POST",
                "/flows/",
                Some(json!({
                    "name": name,
                    "executionRoleArn": EXECUTION_ROLE,
                    "definition": passthrough_definition(),
                })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreateFlow -> 201, got {created:?}"
        );
        let flow_id = created.string(&["id"]);

        let prepared = self
            .call("POST", &format!("/flows/{flow_id}/"), Some(json!({})))
            .await;
        assert_eq!(
            prepared.status, 202,
            "case {case}: expected PrepareFlow -> 202, got {prepared:?}"
        );
        // `PrepareFlow` stores the instant its outcome comes due rather than
        // sleeping, so the setup moves it into the past instead of waiting out
        // the span.
        {
            let state = service_state(&*self.engine).expect("the bedrock-agent store");
            let mut state = state.lock();
            let pending = state
                .flows
                .get_mut(&flow_id)
                .expect("the stored flow")
                .pending_preparation
                .as_mut()
                .unwrap_or_else(|| panic!("case {case}: expected a preparation in flight"));
            pending.due = SystemTime::now() - Duration::from_secs(1);
        }

        let cut = self
            .call("POST", &format!("/flows/{flow_id}/versions"), Some(json!({})))
            .await;
        assert_eq!(
            cut.status, 201,
            "case {case}: expected CreateFlowVersion -> 201, got {cut:?}"
        );
        flow_id
    }

    async fn create_flow_alias(&self, case: &str, flow_id: &str, name: &str) -> String {
        let created = self
            .call(
                "POST",
                &format!("/flows/{flow_id}/aliases"),
                Some(json!({
                    "name": name,
                    "routingConfiguration": [{ "flowVersion": "1" }],
                })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreateFlowAlias -> 201, got {created:?}"
        );
        created.string(&["id"])
    }

    async fn delete_flow_alias(&self, flow_id: &str, alias_id: &str) -> Answer {
        self.call("DELETE", &format!("/flows/{flow_id}/aliases/{alias_id}"), None)
            .await
    }

    async fn get_flow_alias(&self, flow_id: &str, alias_id: &str) -> Answer {
        self.call("GET", &format!("/flows/{flow_id}/aliases/{alias_id}"), None)
            .await
    }

    async fn list_flow_aliases(&self, flow_id: &str) -> Answer {
        self.call("GET", &format!("/flows/{flow_id}/aliases"), None)
            .await
    }

    async fn get_flow(&self, flow_id: &str) -> Answer {
        self.call("GET", &format!("/flows/{flow_id}/"), None).await
    }

    async fn get_flow_version(&self, flow_id: &str, version: &str) -> Answer {
        self.call("GET", &format!("/flows/{flow_id}/versions/{version}"), None)
            .await
    }
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
                "configuration": { "data": { "sourceOutput": "document", "targetInput": "document" } },
            },
        ],
    })
}

/// The refusal a read of an alias that is not there answers.
fn not_found_message(operation: &str, resource_type: &str) -> String {
    format!(
        "Couldn't perform {operation} operation. Failed to retrieve resource type \
         {resource_type} because it doesn't exist. Check the resource ID and retry \
         your request."
    )
}

/// One row of the table: how many aliases the flow holds, and the position (in
/// creation order) of the one the delete addresses.
struct Row {
    case: &'static str,
    aliases: usize,
    deleted: usize,
}

/// Rows spanning the counts a flow can hold aliases in and the position the
/// deleted one occupies — neither of which the rule is keyed to.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the only caller-created alias",
            aliases: 1,
            deleted: 0,
        },
        Row {
            case: "the first of two",
            aliases: 2,
            deleted: 0,
        },
        Row {
            case: "the last of two",
            aliases: 2,
            deleted: 1,
        },
        Row {
            case: "the first of five",
            aliases: 5,
            deleted: 0,
        },
        Row {
            case: "the middle of five",
            aliases: 5,
            deleted: 2,
        },
        Row {
            case: "the last of five",
            aliases: 5,
            deleted: 4,
        },
    ]
}

/// One call removes exactly the alias it addresses: the response reports that
/// alias and its flow, the alias is gone, and every OTHER resource of the flow
/// reads back exactly as it did before the call.
#[tokio::test]
async fn delete_flow_alias_removes_the_addressed_alias_and_nothing_else() {
    for Row {
        case,
        aliases,
        deleted,
    } in rows()
    {
        let emulator = Emulator::new();
        let flow_id = emulator.flow(case, "alias-delete-scope").await;

        let mut alias_ids = Vec::new();
        for index in 0..aliases {
            alias_ids.push(
                emulator
                    .create_flow_alias(case, &flow_id, &format!("alias-{index}"))
                    .await,
            );
        }
        let doomed = alias_ids[deleted].clone();

        // Everything the delete must not touch, as it reads before the call.
        let flow_before = emulator.get_flow(&flow_id).await;
        let version_before = emulator.get_flow_version(&flow_id, "1").await;
        let mut survivors_before = Vec::new();
        for alias_id in alias_ids
            .iter()
            .filter(|alias_id| **alias_id != doomed)
            .cloned()
            .chain([TEST_ALIAS_ID.to_string()])
        {
            let read = emulator.get_flow_alias(&flow_id, &alias_id).await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected GetFlowAlias -> 200 for {alias_id}, got {read:?}"
            );
            survivors_before.push((alias_id, read.body));
        }
        let listed_before = emulator.list_flow_aliases(&flow_id).await.listed_ids();

        let answer = emulator.delete_flow_alias(&flow_id, &doomed).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected DeleteFlowAlias -> 200, got {answer:?}"
        );
        // `DeleteFlowAliasResponse` models exactly two members: the flow the
        // alias belonged to and the alias's own id.
        assert_eq!(
            answer.body,
            json!({ "flowId": flow_id, "id": doomed }),
            "case {case}: expected DeleteFlowAlias to report the flow and the deleted alias, \
             got {:?}",
            answer.body
        );

        // The addressed alias is gone, and a repeat is the modelled not-found
        // rather than a second success.
        for (operation, read) in [
            ("GetFlowAlias", emulator.get_flow_alias(&flow_id, &doomed).await),
            (
                "DeleteFlowAlias",
                emulator.delete_flow_alias(&flow_id, &doomed).await,
            ),
        ] {
            assert_eq!(
                read.status, 404,
                "case {case} ({operation}): expected 404 for the deleted alias, got {read:?}"
            );
            assert_eq!(
                read.error_type.as_deref(),
                Some("ResourceNotFoundException"),
                "case {case} ({operation}): expected ResourceNotFoundException, got {read:?}"
            );
            assert_eq!(
                read.string(&["message"]),
                not_found_message(operation, "FlowAlias"),
                "case {case} ({operation}): expected the refusal to name the resource type it \
                 could not resolve, got {read:?}"
            );
        }

        // Every other alias of the flow, the reserved test alias included,
        // reads back unchanged.
        for (alias_id, before) in &survivors_before {
            let after = emulator.get_flow_alias(&flow_id, alias_id).await;
            assert_eq!(
                after.status, 200,
                "case {case}: expected GetFlowAlias -> 200 for the surviving {alias_id}, \
                 got {after:?}"
            );
            assert_eq!(
                &after.body, before,
                "case {case}: expected {alias_id} unchanged by the delete, got {:?}",
                after.body
            );
        }

        // The listing reports the complement, in the order it reported before.
        let listed_after = emulator.list_flow_aliases(&flow_id).await.listed_ids();
        let expected: Vec<String> = listed_before
            .iter()
            .filter(|alias_id| **alias_id != doomed)
            .cloned()
            .collect();
        assert_eq!(
            listed_after, expected,
            "case {case}: expected the listing to lose the deleted alias alone, got \
             {listed_after:?}"
        );

        // The flow the alias belonged to and the version it routed to stand
        // exactly as they were: an alias never owned either.
        let flow_after = emulator.get_flow(&flow_id).await;
        assert_eq!(
            flow_after.body, flow_before.body,
            "case {case}: expected the flow unchanged by the alias delete, got {:?}",
            flow_after.body
        );
        let version_after = emulator.get_flow_version(&flow_id, "1").await;
        assert_eq!(
            version_after.body, version_before.body,
            "case {case}: expected the version the alias routed to unchanged, got {:?}",
            version_after.body
        );
    }
}

/// Deleting a flow's whole caller-created alias set, one call at a time, removes
/// one alias per call whatever order the calls come in, and leaves the flow
/// holding only the alias the service created with it.
#[tokio::test]
async fn delete_flow_alias_removes_one_alias_per_call_in_any_order() {
    for (case, reverse) in [
        ("creation order", false),
        ("reverse creation order", true),
    ] {
        let emulator = Emulator::new();
        let flow_id = emulator.flow(case, "alias-delete-sweep").await;

        let mut alias_ids = Vec::new();
        for index in 0..4 {
            alias_ids.push(
                emulator
                    .create_flow_alias(case, &flow_id, &format!("sweep-{index}"))
                    .await,
            );
        }
        if reverse {
            alias_ids.reverse();
        }

        let mut remaining = emulator.list_flow_aliases(&flow_id).await.listed_ids().len();
        for alias_id in &alias_ids {
            let answer = emulator.delete_flow_alias(&flow_id, alias_id).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected DeleteFlowAlias -> 200 for {alias_id}, got {answer:?}"
            );
            assert_eq!(
                answer.body,
                json!({ "flowId": flow_id, "id": alias_id }),
                "case {case}: expected the response to report the deleted alias, got {:?}",
                answer.body
            );
            let listed = emulator.list_flow_aliases(&flow_id).await.listed_ids();
            assert_eq!(
                listed.len(),
                remaining - 1,
                "case {case}: expected one alias fewer after deleting {alias_id}, got {listed:?}"
            );
            assert!(
                !listed.contains(alias_id),
                "case {case}: expected {alias_id} gone from the listing, got {listed:?}"
            );
            remaining = listed.len();
        }

        // The flow keeps the alias the service created with it, and the flow
        // itself is still there to read.
        assert_eq!(
            emulator.list_flow_aliases(&flow_id).await.listed_ids(),
            vec![TEST_ALIAS_ID.to_string()],
            "case {case}: expected only the reserved test alias to remain"
        );
        let flow = emulator.get_flow(&flow_id).await;
        assert_eq!(
            flow.status, 200,
            "case {case}: expected the flow to outlive its aliases, got {flow:?}"
        );
    }
}
