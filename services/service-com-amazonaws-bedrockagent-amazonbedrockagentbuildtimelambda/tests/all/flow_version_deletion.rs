// SPDX-License-Identifier: CC0-1.0

//! `DeleteFlowVersion`, end to end over HTTP and read back through
//! `GetFlowVersion` / `ListFlowVersions`.
//!
//! The rule pinned here: a delete this service accepts takes the numbered
//! version it addresses OUT of the store, so every later read of that version
//! answers the modelled `ResourceNotFoundException` (404, the error type in
//! `x-amzn-errortype`, and the sentence naming resource type `FlowVersion`
//! because the flow itself still resolves), while the flow and the versions cut
//! either side of it are left exactly as they were.
//!
//! The tests drive TABLES whose rows differ along what the rule generalises
//! over: which position in the flow's version sequence is deleted (the first,
//! a middle one, the last), the `FlowIdentifier` form the delete addresses the
//! flow by, which of the two addressed resources a refusal cannot resolve, and
//! — for the in-use pre-condition the `skipResourceInUseCheck` member governs —
//! whether an alias still routes to the version and whether the caller asked for
//! the check to be skipped.
//!
//! Every assertion goes through the router, so it exercises the real wire path
//! (route matching, model constraint validation, serialization) rather than
//! calling a handler directly.
//!
//! The one thing these tests reach into the store for is the INSTANT a pending
//! preparation comes due: `PrepareFlow` stores it rather than sleeping, so a
//! test moves it into the past instead of waiting out the preparation span.

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
    fn string(&self, member: &str) -> String {
        self.body[member]
            .as_str()
            .unwrap_or_else(|| panic!("{member} is not a string in {:?}", self.body))
            .to_string()
    }

    /// The `version` member of every reported `ListFlowVersions` summary, in the
    /// order the response listed them.
    fn listed_versions(&self) -> Vec<String> {
        self.body["flowVersionSummaries"]
            .as_array()
            .unwrap_or_else(|| panic!("flowVersionSummaries is not an array in {:?}", self.body))
            .iter()
            .map(|summary| {
                summary["version"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a summary carries no version: {summary:?}"))
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

    /// The `@httpLabel` spelling of an identifier: the `:` and `/` an ARN
    /// carries are percent-encoded, which is how a client sends a label carrying
    /// them.
    fn escape(identifier: &str) -> String {
        identifier.replace(':', "%3A").replace('/', "%2F")
    }

    async fn create_flow(&self, case: &str, name: &str) -> Answer {
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
        created
    }

    /// `PrepareFlow`, with the outcome the clock owes moved into the past so the
    /// flow stands at `Prepared` — the one status a version is cut from —
    /// without the test sleeping for the span.
    async fn prepare_and_settle(&self, case: &str, flow_id: &str) {
        let prepared = self
            .call("POST", &format!("/flows/{flow_id}/"), Some(json!({})))
            .await;
        assert_eq!(
            prepared.status, 202,
            "case {case}: expected PrepareFlow -> 202, got {prepared:?}"
        );
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

    async fn create_flow_version(&self, case: &str, flow_id: &str) -> Answer {
        let cut = self
            .call(
                "POST",
                &format!("/flows/{flow_id}/versions"),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            cut.status, 201,
            "case {case}: expected CreateFlowVersion -> 201, got {cut:?}"
        );
        cut
    }

    /// A flow prepared and cut into `versions` numbered versions, answered as
    /// the `CreateFlow` response: the state a delete is taken against.
    async fn flow_with_versions(&self, case: &str, name: &str, versions: u32) -> Answer {
        let flow = self.create_flow(case, name).await;
        let flow_id = flow.string("id");
        self.prepare_and_settle(case, &flow_id).await;
        for expected in 1..=versions {
            let cut = self.create_flow_version(case, &flow_id).await;
            assert_eq!(
                cut.string("version"),
                expected.to_string(),
                "case {case}: expected version {expected}, got {cut:?}"
            );
        }
        flow
    }

    async fn delete_flow_version(
        &self,
        identifier: &str,
        version: &str,
        skip_resource_in_use_check: bool,
    ) -> Answer {
        let query = if skip_resource_in_use_check {
            "?skipResourceInUseCheck=true"
        } else {
            ""
        };
        self.call(
            "DELETE",
            &format!(
                "/flows/{}/versions/{version}/{query}",
                Self::escape(identifier)
            ),
            None,
        )
        .await
    }

    async fn get_flow_version(&self, identifier: &str, version: &str) -> Answer {
        self.call(
            "GET",
            &format!(
                "/flows/{}/versions/{version}/",
                Self::escape(identifier)
            ),
            None,
        )
        .await
    }

    async fn list_flow_versions(&self, identifier: &str) -> Answer {
        self.call(
            "GET",
            &format!("/flows/{}/versions", Self::escape(identifier)),
            None,
        )
        .await
    }

    async fn get_flow(&self, identifier: &str) -> Answer {
        self.call("GET", &format!("/flows/{}/", Self::escape(identifier)), None)
            .await
    }

    async fn create_flow_alias(&self, case: &str, flow_id: &str, request: Value) -> Answer {
        let created = self
            .call("POST", &format!("/flows/{flow_id}/aliases"), Some(request))
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreateFlowAlias -> 201, got {created:?}"
        );
        created
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

/// The refusal a delete answers while an alias still routes to the resource it
/// names. The fixed text is this error class's; the operation name and the
/// referencing alias id are its only variable parts.
fn in_use_message(operation: &str, alias_id: &str) -> String {
    format!(
        "Couldn't perform {operation} operation. The Flow that you specified is currently \
         in use by the following alias: {alias_id}. Retry your request after the alias has \
         finished using the Flow."
    )
}

/// How many numbered versions the flow each deletion row is taken against
/// holds.
const VERSIONS_CUT: u32 = 3;

/// Which position in the flow's version sequence a row deletes, and which
/// `FlowIdentifier` form it addresses the flow by: the rows span the first
/// version cut, one in the middle and the last, through both forms the modelled
/// `FlowIdentifier` admits.
const DELETION_CASES: &[(&str, &str, bool)] = &[
    ("the first version cut, addressed by flow id", "1", false),
    ("a version in the middle, addressed by flow id", "2", false),
    ("the latest version cut, addressed by flow id", "3", false),
    ("the first version cut, addressed by flow ARN", "1", true),
    ("the latest version cut, addressed by flow ARN", "3", true),
];

/// The rule: an accepted delete removes the numbered version, so the read of it
/// answers the modelled `ResourceNotFoundException` naming `FlowVersion` — the
/// flow still resolves — while every other version of the flow, and the flow's
/// own DRAFT, are untouched.
#[tokio::test]
async fn a_deleted_flow_version_is_no_longer_served() {
    for (case, version, by_arn) in DELETION_CASES {
        let emulator = Emulator::new();
        let flow = emulator
            .flow_with_versions(case, "version-deletes", VERSIONS_CUT)
            .await;
        let flow_id = flow.string("id");
        let flow_arn = flow.string("arn");
        let identifier = if *by_arn { &flow_arn } else { &flow_id };

        // Every version the flow holds, and the response the cut gave for it, so
        // the survivors can be compared member for member after the delete.
        let mut before = Vec::new();
        for held in 1..=VERSIONS_CUT {
            let held = held.to_string();
            let read = emulator.get_flow_version(&flow_id, &held).await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected GetFlowVersion({held}) -> 200 before the delete, \
                 got {read:?}"
            );
            before.push((held, read.body.clone()));
        }

        let deleted = emulator.delete_flow_version(identifier, version, false).await;
        assert_eq!(
            deleted.status, 200,
            "case {case}: expected DeleteFlowVersion -> 200, got {deleted:?}"
        );
        assert_eq!(
            deleted.string("id"),
            flow_id,
            "case {case}: the delete names the flow the version belonged to, got {deleted:?}"
        );
        assert_eq!(
            deleted.string("version"),
            *version,
            "case {case}: the delete names the version it removed, got {deleted:?}"
        );

        // The version is gone: the read of it is the modelled refusal, and it
        // names FlowVersion rather than Flow because the flow still resolves.
        let refused = emulator.get_flow_version(identifier, version).await;
        assert_eq!(
            refused.status, 404,
            "case {case}: expected GetFlowVersion -> 404 after the delete, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: expected ResourceNotFoundException, got {refused:?}"
        );
        assert_eq!(
            refused.string("message"),
            not_found_message("GetFlowVersion", "FlowVersion"),
            "case {case}: expected the refusal to name the FlowVersion it could not \
             resolve, got {refused:?}"
        );

        // A second delete of the same version has nothing to remove, and says so
        // as the same missing resource.
        let again = emulator.delete_flow_version(identifier, version, false).await;
        assert_eq!(
            again.status, 404,
            "case {case}: expected a repeated DeleteFlowVersion -> 404, got {again:?}"
        );
        assert_eq!(
            again.string("message"),
            not_found_message("DeleteFlowVersion", "FlowVersion"),
            "case {case}: expected the repeated delete to name the FlowVersion it could \
             not resolve, got {again:?}"
        );

        // Every other version is exactly as it was, and the listing reports the
        // survivors beside the DRAFT version every flow has.
        let survivors: Vec<String> = before
            .iter()
            .map(|(held, _)| held.clone())
            .filter(|held| held != version)
            .collect();
        for (held, body) in &before {
            if held == version {
                continue;
            }
            let read = emulator.get_flow_version(&flow_id, held).await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected the surviving version {held} to still be served, \
                 got {read:?}"
            );
            assert_eq!(
                &read.body, body,
                "case {case}: expected version {held} to be untouched by the delete, \
                 got {read:?}"
            );
        }
        let listed = emulator.list_flow_versions(&flow_id).await;
        assert_eq!(
            listed.status, 200,
            "case {case}: expected ListFlowVersions -> 200, got {listed:?}"
        );
        let mut expected = vec!["DRAFT".to_string()];
        expected.extend(survivors);
        assert_eq!(
            listed.listed_versions(),
            expected,
            "case {case}: expected the listing to drop the deleted version alone, got {:?}",
            listed.listed_versions()
        );

        // The flow itself is untouched: its DRAFT still reads, and the next cut
        // takes the next number rather than the one the deleted version held,
        // because the flow's numbering is a high-water mark.
        let draft = emulator.get_flow(&flow_id).await;
        assert_eq!(
            draft.status, 200,
            "case {case}: expected the flow to survive its version's delete, got {draft:?}"
        );
        assert_eq!(
            draft.string("version"),
            "DRAFT",
            "case {case}: expected the flow to still report its DRAFT version, got {draft:?}"
        );
        let next = emulator.create_flow_version(case, &flow_id).await;
        assert_eq!(
            next.string("version"),
            (VERSIONS_CUT + 1).to_string(),
            "case {case}: expected the next cut to take the next number, got {next:?}"
        );
    }
}

/// Which of the two resources a delete addresses a row leaves unresolvable.
#[derive(Debug, Clone, Copy)]
enum Missing {
    /// The identifier names no stored flow.
    Flow,
    /// The flow is stored, but holds no version cut under the number asked for.
    Version,
}

/// Rows spanning what a delete can fail to resolve over a flow holding one
/// version, including the published bounds of the modelled `NumericalVersion`
/// (`^[0-9]{1,5}$`): the lowest value the pattern admits and the widest.
const UNRESOLVABLE_CASES: &[(&str, Missing, &str)] = &[
    ("a flow nothing created", Missing::Flow, "1"),
    (
        "a version number above the one version cut",
        Missing::Version,
        "2",
    ),
    (
        "version 0, which numbering starting from 1 never reaches",
        Missing::Version,
        "0",
    ),
    (
        "the widest NumericalVersion, which this flow was never cut at",
        Missing::Version,
        "99999",
    ),
];

/// The rule's refusal half: a delete that resolves to no stored version is the
/// modelled `ResourceNotFoundException` (404) naming which of the two addressed
/// resources is missing, and it removes nothing.
#[tokio::test]
async fn a_delete_of_a_version_nothing_cut_is_refused_as_a_missing_resource() {
    let emulator = Emulator::new();
    let flow = emulator
        .flow_with_versions("the refusal fixture", "version-delete-refusals", 1)
        .await;
    let flow_id = flow.string("id");
    // Shaped as the modelled `FlowId` (`^[0-9a-zA-Z]{10}$`) but minted by
    // nothing, so it resolves to no stored flow.
    let unknown_flow = "Zz0123456z";
    assert_ne!(
        unknown_flow, flow_id,
        "the fixture must not collide with the id nothing minted"
    );

    for (case, missing, version) in UNRESOLVABLE_CASES {
        let (identifier, resource_type) = match missing {
            Missing::Flow => (unknown_flow, "Flow"),
            Missing::Version => (flow_id.as_str(), "FlowVersion"),
        };
        let refused = emulator.delete_flow_version(identifier, version, false).await;
        assert_eq!(
            refused.status, 404,
            "case {case}: expected DeleteFlowVersion -> 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: expected ResourceNotFoundException, got {refused:?}"
        );
        assert_eq!(
            refused.string("message"),
            not_found_message("DeleteFlowVersion", resource_type),
            "case {case}: expected the refusal to name the {resource_type} it could not \
             resolve, got {refused:?}"
        );
    }

    // None of the refusals touched the version the flow does hold.
    let served = emulator.get_flow_version(&flow_id, "1").await;
    assert_eq!(
        served.status, 200,
        "case \"the version the flow holds\": expected GetFlowVersion -> 200 after the \
         refusals, got {served:?}"
    );
}

/// A delete whose path value satisfies no modelled constraint is a BAD REQUEST —
/// the modelled `ValidationException` (400) — not a missing resource: the value
/// names no version of any flow, so there is nothing to look for.
///
/// The rows span the version one past the widest `NumericalVersion`
/// (`^[0-9]{1,5}$`), the DRAFT token — which is no `NumericalVersion` at all,
/// and names a version no delete addresses — and an identifier of neither
/// `FlowIdentifier` form.
#[tokio::test]
async fn a_delete_addressed_outside_the_modelled_constraints_is_a_bad_request() {
    let emulator = Emulator::new();
    let flow = emulator
        .flow_with_versions("the bad-request fixture", "version-delete-badrequests", 1)
        .await;
    let flow_id = flow.string("id");

    let cases: &[(&str, &str, &str)] = &[
        (
            "a version one digit past the widest NumericalVersion",
            &flow_id,
            "100000",
        ),
        (
            "the DRAFT token, which is no NumericalVersion",
            &flow_id,
            "DRAFT",
        ),
        (
            "an identifier of neither FlowIdentifier form",
            "not-a-flow-identifier",
            "1",
        ),
    ];

    for (case, identifier, version) in cases {
        let refused = emulator.delete_flow_version(identifier, version, false).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected DeleteFlowVersion -> 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case}: expected ValidationException, got {refused:?}"
        );
    }

    // The version the flow holds is still there: a bad request removed nothing.
    let served = emulator.get_flow_version(&flow_id, "1").await;
    assert_eq!(
        served.status, 200,
        "case \"the version the flow holds\": expected GetFlowVersion -> 200 after the bad \
         requests, got {served:?}"
    );
}

/// Rows spanning the in-use pre-condition the `skipResourceInUseCheck` member
/// governs, over a flow holding two versions with one alias created on it: the
/// version the alias routes to and the version it does not, each deleted with
/// the check in force and with it skipped, plus an alias routing to the flow's
/// DRAFT version, which is no numbered version and so holds no numbered version
/// back.
const IN_USE_CASES: &[(&str, &str, &str, bool, bool)] = &[
    (
        "the version the alias routes to, with the check in force",
        "1",
        "1",
        false,
        true,
    ),
    (
        "the version the alias routes to, with the check skipped",
        "1",
        "1",
        true,
        false,
    ),
    (
        "a version no alias routes to, with the check in force",
        "1",
        "2",
        false,
        false,
    ),
    (
        "a version no alias routes to, with the check skipped",
        "1",
        "2",
        true,
        false,
    ),
    (
        "the DRAFT version an alias routes to holds no numbered version back",
        "DRAFT",
        "1",
        false,
        false,
    ),
];

/// The rule's pre-condition: while an alias routes to the numbered version, the
/// delete is refused with the modelled `ConflictException` (409) naming that
/// alias and the version stays readable; a caller that asks for the check to be
/// skipped, or that deletes a version no alias routes to, is served and the
/// version goes.
#[tokio::test]
async fn a_version_an_alias_routes_to_is_held_back_unless_the_check_is_skipped() {
    for (case, routed_to, version, skip, blocked) in IN_USE_CASES {
        let emulator = Emulator::new();
        let flow = emulator
            .flow_with_versions(case, "version-delete-in-use", 2)
            .await;
        let flow_id = flow.string("id");
        let alias = emulator
            .create_flow_alias(
                case,
                &flow_id,
                json!({
                    // The modelled `Name` is `pattern ^([0-9a-zA-Z][_-]?){1,100}$`.
                    "name": "in-use-alias",
                    "routingConfiguration": [{ "flowVersion": routed_to }],
                }),
            )
            .await;
        let alias_id = alias.string("id");

        let answer = emulator.delete_flow_version(&flow_id, version, *skip).await;
        if *blocked {
            assert_eq!(
                answer.status, 409,
                "case {case}: expected DeleteFlowVersion -> 409, got {answer:?}"
            );
            assert_eq!(
                answer.error_type.as_deref(),
                Some("ConflictException"),
                "case {case}: expected ConflictException, got {answer:?}"
            );
            assert_eq!(
                answer.string("message"),
                in_use_message("DeleteFlowVersion", &alias_id),
                "case {case}: expected the refusal to name the alias still using the \
                 version, got {answer:?}"
            );
            // Nothing was removed: the version is still served.
            let served = emulator.get_flow_version(&flow_id, version).await;
            assert_eq!(
                served.status, 200,
                "case {case}: expected the held-back version to still be served, got \
                 {served:?}"
            );
            continue;
        }

        assert_eq!(
            answer.status, 200,
            "case {case}: expected DeleteFlowVersion -> 200, got {answer:?}"
        );
        let refused = emulator.get_flow_version(&flow_id, version).await;
        assert_eq!(
            refused.status, 404,
            "case {case}: expected the deleted version to be unreadable, got {refused:?}"
        );
        assert_eq!(
            refused.string("message"),
            not_found_message("GetFlowVersion", "FlowVersion"),
            "case {case}: expected the refusal to name the FlowVersion it could not \
             resolve, got {refused:?}"
        );
    }
}

/// The reference the pre-condition is about is the alias's, so removing the
/// alias unblocks the delete that the same alias held back a moment earlier.
#[tokio::test]
async fn deleting_the_referencing_alias_unblocks_the_version_delete() {
    let case = "an alias routing to version 1, then deleted";
    let emulator = Emulator::new();
    let flow = emulator
        .flow_with_versions(case, "version-delete-unblocked", 1)
        .await;
    let flow_id = flow.string("id");
    let alias = emulator
        .create_flow_alias(
            case,
            &flow_id,
            json!({
                "name": "unblocking-alias",
                "routingConfiguration": [{ "flowVersion": "1" }],
            }),
        )
        .await;
    let alias_id = alias.string("id");

    let blocked = emulator.delete_flow_version(&flow_id, "1", false).await;
    assert_eq!(
        blocked.status, 409,
        "case {case}: expected DeleteFlowVersion -> 409 while the alias routes there, got \
         {blocked:?}"
    );

    // The alias's routing configuration is re-pointed at the flow's DRAFT
    // version, which is no numbered version: the reference the pre-condition
    // checks is gone, so the delete is served.
    let updated = emulator
        .call(
            "PUT",
            &format!("/flows/{flow_id}/aliases/{alias_id}"),
            Some(json!({
                "name": "unblocking-alias",
                "routingConfiguration": [{ "flowVersion": "DRAFT" }],
            })),
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "case {case}: expected UpdateFlowAlias -> 200, got {updated:?}"
    );

    let served = emulator.delete_flow_version(&flow_id, "1", false).await;
    assert_eq!(
        served.status, 200,
        "case {case}: expected DeleteFlowVersion -> 200 once nothing routes to the \
         version, got {served:?}"
    );
    let refused = emulator.get_flow_version(&flow_id, "1").await;
    assert_eq!(
        refused.status, 404,
        "case {case}: expected the deleted version to be unreadable, got {refused:?}"
    );
}
