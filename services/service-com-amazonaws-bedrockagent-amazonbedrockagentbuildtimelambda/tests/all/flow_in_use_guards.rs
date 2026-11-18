// SPDX-License-Identifier: CC0-1.0

//! The in-use pre-condition the `skipResourceInUseCheck` member governs, at
//! `DeleteFlow`, end to end over HTTP: with the check in force — the member
//! absent or false — a delete of a flow an alias still references is refused with
//! the modelled `ConflictException` (409) naming the referencing alias, and the
//! flow survives; with the check skipped, and once nothing references the flow,
//! the same delete is served.
//!
//! The table below spans what the rule generalises over: how many aliases
//! reference the flow (none at all included), which version each of them routes
//! to, and every value `skipResourceInUseCheck` can take — including the absent
//! member, which the model defaults to false.
//!
//! Every assertion here goes through the router, so it exercises the real wire
//! path (route matching, model constraint validation, serialization) rather than
//! calling a handler directly.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state::service_state;
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the
/// routing key this service claims on, and the source of the region its ARNs
/// carry.
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

    /// The `id` member of every reported summary of an alias a CALLER created, in
    /// the order the response listed them: the flow's reserved test alias, which
    /// the service creates with the flow and which holds the flow back from no
    /// delete, is not one of the references these rows are about.
    fn listed_ids(&self) -> Vec<String> {
        self.member(&["flowAliasSummaries"])
            .as_array()
            .unwrap_or_else(|| panic!("flowAliasSummaries is not an array in {:?}", self.body))
            .iter()
            .filter(|summary| summary["id"].as_str() != Some(TEST_ALIAS_ID))
            .map(|summary| summary["id"].as_str().unwrap_or_default().to_string())
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

    /// A prepared flow cut into two numbered versions: the state the aliases
    /// below are created against. Answers the flow's id.
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

        for expected in 1..=2 {
            let cut = self
                .call("POST", &format!("/flows/{flow_id}/versions"), Some(json!({})))
                .await;
            assert_eq!(
                cut.status, 201,
                "case {case}: expected CreateFlowVersion -> 201, got {cut:?}"
            );
            assert_eq!(
                cut.string(&["version"]),
                expected.to_string(),
                "case {case}: expected version {expected}, got {cut:?}"
            );
        }
        flow_id
    }

    async fn create_flow_alias(
        &self,
        case: &str,
        flow_id: &str,
        name: &str,
        flow_version: &str,
    ) -> String {
        let created = self
            .call(
                "POST",
                &format!("/flows/{flow_id}/aliases"),
                Some(json!({
                    "name": name,
                    "routingConfiguration": [{ "flowVersion": flow_version }],
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
        self.call(
            "DELETE",
            &format!("/flows/{flow_id}/aliases/{alias_id}"),
            None,
        )
        .await
    }

    async fn get_flow(&self, flow_id: &str) -> Answer {
        self.call("GET", &format!("/flows/{flow_id}/"), None).await
    }

    async fn list_flow_aliases(&self, flow_id: &str) -> Answer {
        self.call("GET", &format!("/flows/{flow_id}/aliases"), None)
            .await
    }

    /// `DeleteFlow`, with `skipResourceInUseCheck` carried in the query string
    /// exactly when the row supplies it (the member is `httpQuery`-bound and
    /// defaults to false).
    async fn delete_flow(&self, flow_id: &str, skip_in_use_check: Option<bool>) -> Answer {
        let query = skip_in_use_check
            .map(|skip| format!("?skipResourceInUseCheck={skip}"))
            .unwrap_or_default();
        self.call("DELETE", &format!("/flows/{flow_id}/{query}"), None)
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

/// The refusal a delete answers while an alias still references the resource the
/// request names. The whole sentence is fixed text for that error class except
/// the operation the caller invoked and the referencing alias's id.
fn in_use_message(operation: &str, alias_id: &str) -> String {
    format!(
        "Couldn't perform {operation} operation. The Flow that you specified is currently \
         in use by the following alias: {alias_id}. Retry your request after the alias has \
         finished using the Flow."
    )
}

/// One row of the in-use table.
struct Row {
    case: &'static str,
    /// The version each alias created before the delete routes to: one entry per
    /// alias, so the row's length is how many aliases reference the flow.
    routed_to: &'static [&'static str],
    /// The `skipResourceInUseCheck` the request carries, or `None` when it leaves
    /// the member to its modelled default.
    skip_in_use_check: Option<bool>,
    /// Whether the pre-condition holds the delete back.
    blocked: bool,
}

/// Rows spanning the aliases that reference the flow — none, one routing to a
/// numbered version, one routing to the flow's DRAFT version, and several routing
/// to different versions — crossed with every value `skipResourceInUseCheck` can
/// take, the absent member included.
const ROWS: &[Row] = &[
    Row {
        case: "no alias at all, the member absent",
        routed_to: &[],
        skip_in_use_check: None,
        blocked: false,
    },
    Row {
        case: "no alias at all, the check in force",
        routed_to: &[],
        skip_in_use_check: Some(false),
        blocked: false,
    },
    Row {
        case: "one alias routing to a numbered version, the member absent",
        routed_to: &["1"],
        skip_in_use_check: None,
        blocked: true,
    },
    Row {
        case: "one alias routing to a numbered version, the check in force",
        routed_to: &["2"],
        skip_in_use_check: Some(false),
        blocked: true,
    },
    Row {
        case: "one alias routing to the DRAFT version, the check in force",
        routed_to: &["DRAFT"],
        skip_in_use_check: Some(false),
        blocked: true,
    },
    Row {
        case: "three aliases routing to different versions, the member absent",
        routed_to: &["1", "2", "DRAFT"],
        skip_in_use_check: None,
        blocked: true,
    },
    Row {
        case: "one alias routing to a numbered version, the check skipped",
        routed_to: &["1"],
        skip_in_use_check: Some(true),
        blocked: false,
    },
    Row {
        case: "three aliases routing to different versions, the check skipped",
        routed_to: &["1", "2", "DRAFT"],
        skip_in_use_check: Some(true),
        blocked: false,
    },
];

/// The rule: while an alias references the flow and the request leaves the
/// in-use check in force, `DeleteFlow` is refused 409 `ConflictException` naming
/// the referencing alias and the flow stays readable with every alias it had; a
/// caller that asks for the check to be skipped, or that deletes a flow no alias
/// references, is served.
#[tokio::test]
async fn a_flow_an_alias_references_is_held_back_unless_the_check_is_skipped() {
    for row in ROWS {
        let case = row.case;
        let emulator = Emulator::new();
        let flow_id = emulator.flow(case, "flow-delete-in-use").await;

        let mut alias_ids = Vec::new();
        for (at, flow_version) in row.routed_to.iter().enumerate() {
            // The modelled `Name` is `pattern ^([0-9a-zA-Z][_-]?){1,100}$`, and a
            // name is unique among the aliases of one flow.
            let name = format!("in-use-alias-{at}");
            alias_ids.push(
                emulator
                    .create_flow_alias(case, &flow_id, &name, flow_version)
                    .await,
            );
        }

        let answer = emulator.delete_flow(&flow_id, row.skip_in_use_check).await;
        if row.blocked {
            assert_eq!(
                answer.status, 409,
                "case {case}: expected DeleteFlow -> 409, got {answer:?}"
            );
            assert_eq!(
                answer.error_type.as_deref(),
                Some("ConflictException"),
                "case {case}: expected ConflictException, got {answer:?}"
            );
            // The refusal names an alias that really references the flow — the
            // first one created, so the same refusal names the same alias every
            // time.
            assert_eq!(
                answer.string(&["message"]),
                in_use_message("DeleteFlow", &alias_ids[0]),
                "case {case}: expected the refusal to name the alias still using the \
                 flow, got {answer:?}"
            );
            // Nothing was removed: the flow and every alias on it stand.
            let read = emulator.get_flow(&flow_id).await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected the held-back flow to still be served, got {read:?}"
            );
            assert_eq!(
                emulator.list_flow_aliases(&flow_id).await.listed_ids(),
                alias_ids,
                "case {case}: the refused delete left every alias on the flow"
            );
            continue;
        }

        assert_eq!(
            answer.status, 200,
            "case {case}: expected DeleteFlow -> 200, got {answer:?}"
        );
        assert_eq!(
            answer.body,
            json!({ "id": flow_id }),
            "case {case}: expected DeleteFlow to report only the deleted flow's id, got {:?}",
            answer.body
        );
        let read = emulator.get_flow(&flow_id).await;
        assert_eq!(
            read.status, 404,
            "case {case}: expected the deleted flow to be unreadable, got {read:?}"
        );
    }
}

/// The reference the pre-condition is about is the alias's, so deleting the
/// aliases unblocks the delete that they held back a moment earlier — which is
/// the teardown order the refusal asks the caller for.
#[tokio::test]
async fn deleting_the_referencing_aliases_unblocks_the_flow_delete() {
    let case = "two aliases, then deleted one at a time";
    let emulator = Emulator::new();
    let flow_id = emulator.flow(case, "flow-delete-unblocked").await;
    let first = emulator
        .create_flow_alias(case, &flow_id, "unblocking-alias-1", "1")
        .await;
    let second = emulator
        .create_flow_alias(case, &flow_id, "unblocking-alias-2", "DRAFT")
        .await;

    // While both aliases hold, the refusal names the first one created.
    let blocked = emulator.delete_flow(&flow_id, None).await;
    assert_eq!(
        blocked.string(&["message"]),
        in_use_message("DeleteFlow", &first),
        "case {case}: expected the refusal to name the earliest alias, got {blocked:?}"
    );

    // One alias gone still leaves the other referencing the flow, so the delete
    // is still refused — now naming the alias that remains.
    let deleted_alias = emulator.delete_flow_alias(&flow_id, &first).await;
    assert_eq!(
        deleted_alias.status, 200,
        "case {case}: expected DeleteFlowAlias -> 200, got {deleted_alias:?}"
    );
    let still_blocked = emulator.delete_flow(&flow_id, None).await;
    assert_eq!(
        still_blocked.status, 409,
        "case {case}: expected DeleteFlow -> 409 while one alias remains, got \
         {still_blocked:?}"
    );
    assert_eq!(
        still_blocked.string(&["message"]),
        in_use_message("DeleteFlow", &second),
        "case {case}: expected the refusal to name the alias that remains, got \
         {still_blocked:?}"
    );

    // With the last alias gone nothing references the flow, so the same request
    // is served.
    let deleted_alias = emulator.delete_flow_alias(&flow_id, &second).await;
    assert_eq!(
        deleted_alias.status, 200,
        "case {case}: expected DeleteFlowAlias -> 200, got {deleted_alias:?}"
    );
    let served = emulator.delete_flow(&flow_id, None).await;
    assert_eq!(
        served.status, 200,
        "case {case}: expected DeleteFlow -> 200 once no alias references the flow, got \
         {served:?}"
    );
    let read = emulator.get_flow(&flow_id).await;
    assert_eq!(
        read.status, 404,
        "case {case}: expected the deleted flow to be unreadable, got {read:?}"
    );
}

/// The pre-condition is about the flow the request names: an alias on ANOTHER
/// flow holds nothing back here.
#[tokio::test]
async fn an_alias_of_another_flow_holds_this_flow_back_from_nothing() {
    let case = "an alias on a bystander flow";
    let emulator = Emulator::new();
    let flow_id = emulator.flow(case, "flow-delete-target").await;
    let bystander_id = emulator.flow(case, "flow-delete-bystander").await;
    let bystander_alias = emulator
        .create_flow_alias(case, &bystander_id, "bystander-alias", "1")
        .await;

    let served = emulator.delete_flow(&flow_id, None).await;
    assert_eq!(
        served.status, 200,
        "case {case}: another flow's alias references this flow not at all, got {served:?}"
    );

    // And the bystander flow, which its own alias does reference, is still held
    // back.
    let blocked = emulator.delete_flow(&bystander_id, None).await;
    assert_eq!(
        blocked.status, 409,
        "case {case}: expected DeleteFlow -> 409 for the referenced flow, got {blocked:?}"
    );
    assert_eq!(
        blocked.string(&["message"]),
        in_use_message("DeleteFlow", &bystander_alias),
        "case {case}: expected the refusal to name the bystander's own alias, got \
         {blocked:?}"
    );
}
