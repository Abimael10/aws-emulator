// SPDX-License-Identifier: CC0-1.0

//! The reserved test alias every flow carries, end to end over HTTP.
//!
//! The rule: a flow carries the alias id the modelled `FlowAliasId`
//! (`pattern ^(TSTALIASID|[0-9a-zA-Z]{10})$`) reserves from the moment it is
//! created, with no caller action — it routes to the flow's DRAFT version, its
//! ARN is the flow's own ARN with that alias segment appended (which the
//! modelled `FlowAliasArn` admits), and every alias reader reports it like any
//! other alias of the flow. It is the service's own alias, so it is never handed
//! out as a minted alias id and it does not hold the flow back from a delete.
//!
//! The tables span what the rule generalises over: the shape of the flow created
//! (with and without a definition, with and without a description, with an
//! idempotency token), how many flows the store holds, and which read path
//! reports the alias (`ListFlowAliases`, `GetFlowAlias` by the reserved token and
//! by the alias ARN, over both forms the modelled `FlowIdentifier` admits).
//!
//! Every assertion here goes through the router, so it exercises the real wire
//! path (route matching, model constraint validation, serialization) rather than
//! calling a handler directly.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
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

/// The alias id the modelled `FlowAliasId` reserves, which is also the only
/// `aliasIdentifier` form the modelled `FlowAliasIdentifier` admits beside a
/// minted id and an alias ARN.
const TEST_ALIAS_ID: &str = "TSTALIASID";

/// The version every stored flow has, as the modelled
/// `FlowAliasRoutingConfigurationListItem$flowVersion`
/// (`pattern ^(DRAFT|[0-9]{0,4}[1-9][0-9]{0,4})$`) spells it.
const DRAFT_VERSION: &str = "DRAFT";

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

    fn summaries(&self) -> Vec<Value> {
        self.member(&["flowAliasSummaries"])
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("flowAliasSummaries is not an array in {:?}", self.body))
    }

    /// The `id` member of every reported alias summary, in the order the
    /// response listed them.
    fn listed_ids(&self) -> Vec<String> {
        self.summaries()
            .iter()
            .map(|summary| summary["id"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// The one reported summary under the reserved alias id, or `None` when the
    /// listing carries none.
    fn test_alias_summary(&self) -> Option<Value> {
        self.summaries()
            .into_iter()
            .find(|summary| summary["id"].as_str() == Some(TEST_ALIAS_ID))
    }
}

/// The emulator under test: one router over one shared engine, exactly as the
/// production router mounts this service.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
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

    /// The `@httpLabel` spelling of an identifier: the `:` and `/` an ARN
    /// carries are percent-encoded, which is how a client sends a label holding
    /// them.
    fn escape(identifier: &str) -> String {
        identifier.replace(':', "%3A").replace('/', "%2F")
    }

    async fn create_flow(&self, case: &str, request: Value) -> Answer {
        let created = self.call("POST", "/flows/", Some(request)).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreateFlow -> 201, got {created:?}"
        );
        created
    }

    async fn get_flow_alias(&self, flow_identifier: &str, alias_identifier: &str) -> Answer {
        let flow = Self::escape(flow_identifier);
        let alias = Self::escape(alias_identifier);
        self.call("GET", &format!("/flows/{flow}/aliases/{alias}"), None)
            .await
    }

    async fn list_flow_aliases(&self, identifier: &str) -> Answer {
        let escaped = Self::escape(identifier);
        self.call("GET", &format!("/flows/{escaped}/aliases"), None)
            .await
    }

    async fn create_flow_alias(&self, identifier: &str, request: Value) -> Answer {
        let escaped = Self::escape(identifier);
        self.call("POST", &format!("/flows/{escaped}/aliases"), Some(request))
            .await
    }

    async fn update_flow_alias(
        &self,
        flow_identifier: &str,
        alias_identifier: &str,
        request: Value,
    ) -> Answer {
        let flow = Self::escape(flow_identifier);
        let alias = Self::escape(alias_identifier);
        self.call(
            "PUT",
            &format!("/flows/{flow}/aliases/{alias}"),
            Some(request),
        )
        .await
    }

    async fn delete_flow_alias(&self, flow_identifier: &str, alias_identifier: &str) -> Answer {
        let flow = Self::escape(flow_identifier);
        let alias = Self::escape(alias_identifier);
        self.call("DELETE", &format!("/flows/{flow}/aliases/{alias}"), None)
            .await
    }

    async fn delete_flow(&self, identifier: &str) -> Answer {
        let escaped = Self::escape(identifier);
        self.call("DELETE", &format!("/flows/{escaped}/"), None).await
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

/// The refusal an operation answers when a resource it was asked to resolve does
/// not exist. The fixed text is this error class's; the operation name and the
/// resource type are its only variable parts.
fn not_found_message(operation: &str, resource_type: &str) -> String {
    format!(
        "Couldn't perform {operation} operation. Failed to retrieve resource type \
         {resource_type} because it doesn't exist. Check the resource ID and retry \
         your request."
    )
}

/// The `CreateFlow` requests the rows below create a flow with: the modelled
/// `CreateFlowRequest` requires `name` and `executionRoleArn` alone, so the rows
/// span what a caller may leave out — the definition, the description — and the
/// idempotency token, because none of them is what makes the flow carry its test
/// alias.
fn creation_rows() -> Vec<(&'static str, Value)> {
    vec![
        (
            "the required members alone",
            json!({ "name": "test-alias-bare-flow", "executionRoleArn": EXECUTION_ROLE }),
        ),
        (
            "a flow with a definition",
            json!({
                "name": "test-alias-defined-flow",
                "executionRoleArn": EXECUTION_ROLE,
                "definition": passthrough_definition(),
            }),
        ),
        (
            "a flow with a description",
            json!({
                "name": "test-alias-described-flow",
                "executionRoleArn": EXECUTION_ROLE,
                "description": "a flow created with every optional member set",
                "definition": passthrough_definition(),
            }),
        ),
        (
            "a flow created under an idempotency token",
            json!({
                "name": "test-alias-token-flow",
                "executionRoleArn": EXECUTION_ROLE,
                // Inside the modelled `ClientToken` (len 33..256, pattern
                // `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`).
                "clientToken": "token-for-the-flow-with-a-test-alias",
            }),
        ),
    ]
}

/// Assert `summary` is the flow's reserved test alias as every projection of it
/// reports it: the reserved token as its id, name and description, the flow's
/// ARN with the alias segment, the addressed flow, and the DRAFT routing.
fn assert_is_the_test_alias(case: &str, summary: &Value, flow_id: &str, flow_arn: &str) {
    for member in ["id", "name", "description"] {
        assert_eq!(
            summary[member].as_str(),
            Some(TEST_ALIAS_ID),
            "case {case}: expected the test alias's {member} to be the reserved token, \
             got {summary:?}"
        );
    }
    assert_eq!(
        summary["flowId"].as_str(),
        Some(flow_id),
        "case {case}: expected the test alias to name the flow it belongs to, got {summary:?}"
    );
    assert_eq!(
        summary["arn"].as_str(),
        Some(format!("{flow_arn}/alias/{TEST_ALIAS_ID}").as_str()),
        "case {case}: expected the flow's ARN with the reserved alias segment, \
         got {summary:?}"
    );
    assert_eq!(
        summary["routingConfiguration"],
        json!([{ "flowVersion": DRAFT_VERSION }]),
        "case {case}: expected the test alias to route to the flow's DRAFT version, \
         got {summary:?}"
    );
    for member in ["createdAt", "updatedAt"] {
        let at = summary[member].as_str().unwrap_or_default();
        assert!(
            at.contains('T') && at.ends_with('Z'),
            "case {case}: expected {member} in the modelled date-time wire form, got {at:?}"
        );
    }
}

/// The rule: a flow carries the reserved test alias from creation, with no
/// caller action, and every alias reader reports it — the listing of a freshly
/// created flow, a `GetFlowAlias` addressing it by the reserved token, and one
/// addressing it by its own ARN, over both admitted `FlowIdentifier` forms.
#[tokio::test]
async fn every_flow_carries_the_reserved_test_alias_of_its_draft_version() {
    for (case, request) in creation_rows() {
        let emulator = Emulator::new();
        let flow = emulator.create_flow(case, request).await;
        let flow_id = flow.string(&["id"]);
        let flow_arn = flow.string(&["arn"]);

        // The listing of a flow nothing has created an alias on carries exactly
        // the reserved test alias.
        let listed = emulator.list_flow_aliases(&flow_id).await;
        assert_eq!(
            listed.status, 200,
            "case {case}: expected ListFlowAliases -> 200, got {listed:?}"
        );
        assert_eq!(
            listed.listed_ids(),
            vec![TEST_ALIAS_ID.to_string()],
            "case {case}: expected a fresh flow to enumerate its test alias alone, \
             got {listed:?}"
        );
        let summary = listed
            .test_alias_summary()
            .unwrap_or_else(|| panic!("case {case}: expected a test-alias summary in {listed:?}"));
        assert_is_the_test_alias(case, &summary, &flow_id, &flow_arn);

        // Rows: every pair of identifier forms that names this alias.
        let alias_arn = format!("{flow_arn}/alias/{TEST_ALIAS_ID}");
        for (form, flow_identifier, alias_identifier) in [
            ("bare ids", flow_id.as_str(), TEST_ALIAS_ID),
            ("a flow ARN", flow_arn.as_str(), TEST_ALIAS_ID),
            ("an alias ARN", flow_id.as_str(), alias_arn.as_str()),
        ] {
            let read = emulator.get_flow_alias(flow_identifier, alias_identifier).await;
            assert_eq!(
                read.status, 200,
                "case {case} ({form}): expected GetFlowAlias -> 200, got {read:?}"
            );
            assert_is_the_test_alias(case, &read.body, &flow_id, &flow_arn);
        }

        // The same flow read through its ARN enumerates the same alias.
        let by_arn = emulator.list_flow_aliases(&flow_arn).await;
        assert_eq!(
            by_arn.listed_ids(),
            vec![TEST_ALIAS_ID.to_string()],
            "case {case}: both identifier forms enumerate the same aliases, got {by_arn:?}"
        );
    }
}

/// How many flows the store holds in the row below: each one carries its own
/// test alias, under its own ARN.
const FLOW_COUNTS: &[usize] = &[1, 2, 4];

/// The test alias belongs to the flow it was created with: with several flows in
/// one store each reports a test alias of its own, no flow reports another's, and
/// a flow created after a delete carries a fresh one under its own ARN.
#[tokio::test]
async fn each_flow_carries_its_own_test_alias() {
    for flows in FLOW_COUNTS {
        let case = format!("{flows} flow(s) in one store");
        let case = case.as_str();
        let emulator = Emulator::new();

        let mut created: Vec<(String, String)> = Vec::new();
        for at in 0..*flows {
            let flow = emulator
                .create_flow(
                    case,
                    json!({
                        "name": format!("test-alias-flow-{at}"),
                        "executionRoleArn": EXECUTION_ROLE,
                        "definition": passthrough_definition(),
                    }),
                )
                .await;
            created.push((flow.string(&["id"]), flow.string(&["arn"])));
        }

        for (flow_id, flow_arn) in &created {
            let read = emulator.get_flow_alias(flow_id, TEST_ALIAS_ID).await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected GetFlowAlias -> 200 for {flow_id}, got {read:?}"
            );
            assert_is_the_test_alias(case, &read.body, flow_id, flow_arn);
        }

        // One flow's test alias is not reachable through another flow's ARN: the
        // alias ARN carries the flow it belongs to.
        if created.len() > 1 {
            let (first_id, _) = &created[0];
            let (_, second_arn) = &created[1];
            let foreign = emulator
                .get_flow_alias(first_id, &format!("{second_arn}/alias/{TEST_ALIAS_ID}"))
                .await;
            assert_eq!(
                foreign.status, 404,
                "case {case}: one flow does not report another's test alias, got {foreign:?}"
            );
            assert_eq!(
                foreign.error_type.as_deref(),
                Some("ResourceNotFoundException"),
                "case {case}: expected ResourceNotFoundException, got {foreign:?}"
            );
            assert_eq!(
                foreign.string(&["message"]),
                not_found_message("GetFlowAlias", "FlowAlias"),
                "case {case}: the refusal names the resource type it could not resolve, \
                 got {foreign:?}"
            );
        }

        // The test alias goes with its flow, and a flow created afterwards gets
        // one of its own: the reserved id is the same, the ARN is not.
        let (deleted_id, deleted_arn) = created[0].clone();
        let deleted = emulator.delete_flow(&deleted_id).await;
        assert_eq!(
            deleted.status, 200,
            "case {case}: a flow holding only its test alias is deleted without the \
             in-use check being skipped, got {deleted:?}"
        );
        let gone = emulator.get_flow_alias(&deleted_id, TEST_ALIAS_ID).await;
        assert_eq!(
            gone.status, 404,
            "case {case}: the test alias went with the flow, got {gone:?}"
        );
        assert_eq!(
            gone.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: expected ResourceNotFoundException, got {gone:?}"
        );
        assert_eq!(
            gone.string(&["message"]),
            not_found_message("GetFlowAlias", "Flow"),
            "case {case}: the refusal names the flow that is gone, got {gone:?}"
        );

        let replacement = emulator
            .create_flow(
                case,
                json!({
                    "name": "test-alias-replacement-flow",
                    "executionRoleArn": EXECUTION_ROLE,
                }),
            )
            .await;
        let replacement_id = replacement.string(&["id"]);
        let replacement_arn = replacement.string(&["arn"]);
        assert_ne!(
            replacement_arn, deleted_arn,
            "case {case}: a flow created after the delete is a new flow"
        );
        let read = emulator
            .get_flow_alias(&replacement_id, TEST_ALIAS_ID)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case}: the new flow carries a test alias of its own, got {read:?}"
        );
        assert_is_the_test_alias(case, &read.body, &replacement_id, &replacement_arn);
    }
}

/// How many aliases the row below creates: enough draws that an id minted from
/// the reserved token would show up.
const ALIASES_CREATED: usize = 16;

/// The reserved token is out of the caller-settable alias namespace: a burst of
/// creates is minted ids of the modelled `^[0-9a-zA-Z]{10}$` form and never the
/// reserved one, and each created alias is enumerated beside the test alias
/// rather than in place of it.
#[tokio::test]
async fn the_reserved_token_is_never_minted_for_a_caller_created_alias() {
    let case = "a burst of caller-created aliases";
    let emulator = Emulator::new();
    let flow = emulator
        .create_flow(
            case,
            json!({
                "name": "test-alias-namespace-flow",
                "executionRoleArn": EXECUTION_ROLE,
                "definition": passthrough_definition(),
            }),
        )
        .await;
    let flow_id = flow.string(&["id"]);

    let mut minted: Vec<String> = Vec::new();
    for at in 0..ALIASES_CREATED {
        let created = emulator
            .create_flow_alias(
                &flow_id,
                json!({
                    "name": format!("namespace-alias-{at:02}"),
                    "routingConfiguration": [{ "flowVersion": DRAFT_VERSION }],
                }),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreateFlowAlias -> 201, got {created:?}"
        );
        let alias_id = created.string(&["id"]);
        assert_ne!(
            alias_id, TEST_ALIAS_ID,
            "case {case}: the reserved token is never minted for a caller's alias"
        );
        assert!(
            alias_id.len() == 10
                && alias_id
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()),
            "case {case}: expected a minted alias id of the modelled form, got {alias_id:?}"
        );
        minted.push(alias_id);
    }

    // Every created alias is reported, and so is the test alias: the reserved
    // alias is one of the flow's aliases, not a slot a caller can take.
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let escaped = Emulator::escape(&flow_id);
        let uri = match &token {
            Some(token) => format!("/flows/{escaped}/aliases?nextToken={token}"),
            None => format!("/flows/{escaped}/aliases"),
        };
        let page = emulator.call("GET", &uri, None).await;
        assert_eq!(
            page.status, 200,
            "case {case}: expected ListFlowAliases -> 200, got {page:?}"
        );
        walked.extend(page.listed_ids());
        token = page
            .member(&["nextToken"])
            .as_str()
            .map(ToString::to_string);
        if token.is_none() {
            break;
        }
    }

    let mut expected = minted.clone();
    expected.push(TEST_ALIAS_ID.to_string());
    expected.sort();
    let mut reported = walked.clone();
    reported.sort();
    assert_eq!(
        reported, expected,
        "case {case}: expected the listing to report every alias the flow holds, \
         the reserved one included, got {walked:?}"
    );
}

/// The reserved alias is a STORED alias of the flow, so the operations that
/// address an alias by identifier reach it as the record it is rather than
/// reporting it missing: it is read, updated and removed through the same
/// routes, under the same id, and what each of them leaves behind is what the
/// next read reports.
///
/// The rows span the admitted `flowIdentifier` forms, because which form
/// addresses the flow decides nothing about the alias it holds.
#[tokio::test]
async fn the_reserved_alias_is_addressed_by_identifier_like_any_other_alias() {
    for by_arn in [false, true] {
        let case = format!("addressed by {}", if by_arn { "the flow ARN" } else { "the flow id" });
        let case = case.as_str();
        let emulator = Emulator::new();
        let flow = emulator
            .create_flow(
                case,
                json!({
                    "name": "test-alias-addressing-flow",
                    "executionRoleArn": EXECUTION_ROLE,
                    "definition": passthrough_definition(),
                }),
            )
            .await;
        let flow_id = flow.string(&["id"]);
        let flow_arn = flow.string(&["arn"]);
        let identifier = if by_arn { flow_arn.clone() } else { flow_id.clone() };

        // An update addressed at the reserved id writes to that record: the
        // members the request carries are replaced and the alias keeps its
        // identity — the reserved id and the ARN built from it.
        let updated = emulator
            .update_flow_alias(
                &identifier,
                TEST_ALIAS_ID,
                json!({
                    "name": "retargeted",
                    "description": "the reserved alias as an update left it",
                    "routingConfiguration": [{ "flowVersion": DRAFT_VERSION }],
                }),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {case}: expected UpdateFlowAlias -> 200 on the reserved alias, \
             got {updated:?}"
        );
        assert_eq!(
            updated.string(&["id"]),
            TEST_ALIAS_ID,
            "case {case}: the update keeps the alias's identity, got {updated:?}"
        );
        assert_eq!(
            updated.string(&["arn"]),
            format!("{flow_arn}/alias/{TEST_ALIAS_ID}"),
            "case {case}: the update keeps the alias's ARN, got {updated:?}"
        );
        assert_eq!(
            updated.string(&["name"]),
            "retargeted",
            "case {case}: the update replaces the name it carries, got {updated:?}"
        );
        let read = emulator.get_flow_alias(&identifier, TEST_ALIAS_ID).await;
        assert_eq!(
            read.body, updated.body,
            "case {case}: the stored alias is reported as the update left it, got {read:?}"
        );

        // And a delete addressed at it removes that record, which the listing
        // and the read both report.
        let deleted = emulator.delete_flow_alias(&identifier, TEST_ALIAS_ID).await;
        assert_eq!(
            deleted.status, 200,
            "case {case}: expected DeleteFlowAlias -> 200 on the reserved alias, \
             got {deleted:?}"
        );
        assert_eq!(
            deleted.body,
            json!({ "flowId": flow_id, "id": TEST_ALIAS_ID }),
            "case {case}: the delete reports the alias it removed, got {:?}",
            deleted.body
        );
        assert_eq!(
            emulator.list_flow_aliases(&identifier).await.listed_ids(),
            Vec::<String>::new(),
            "case {case}: the removed alias is enumerated no more"
        );
        let gone = emulator.get_flow_alias(&identifier, TEST_ALIAS_ID).await;
        assert_eq!(
            gone.status, 404,
            "case {case}: the removed alias is readable no more, got {gone:?}"
        );
        assert_eq!(
            gone.string(&["message"]),
            not_found_message("GetFlowAlias", "FlowAlias"),
            "case {case}: the refusal names the alias it could not resolve, got {gone:?}"
        );
    }
}
