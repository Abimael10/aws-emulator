// SPDX-License-Identifier: CC0-1.0

//! The flow deployment plane end to end over HTTP — prepare, version, alias,
//! list — with no wait between the calls.
//!
//! Three rules are asserted here.
//!
//! A numbered version is cut from a DRAFT whose preparation SUCCEEDED, and
//! preparation is decided when `PrepareFlow` accepts the call: only the
//! REPORTING of the outcome is asynchronous, so the `CreateFlowVersion` that
//! follows a `PrepareFlow` immediately is served 201 and cuts version `1`. The
//! table spans the gap a client leaves between the two calls — none at all, and
//! long enough for the outcome to be reported — across the two forms the
//! modelled `FlowIdentifier` admits and across the numbered versions cut in
//! sequence, because what decides the cut is the flow's own preparation and not
//! the clock.
//!
//! An alias whose single routing entry names a version the flow holds is created
//! and reported back: `arn` is the flow's own ARN with the alias segment
//! appended, `flowId` is the addressed flow, `name`/`description`/
//! `routingConfiguration` are the request's, and `id` is a freshly minted alias
//! id.
//!
//! A `CreateFlowAlias` whose routing entry names a version that cannot be
//! resolved is refused 404 `ResourceNotFoundException` naming the resource type
//! `FlowVersion` — the version it pinned — whether the flow holds no such
//! version or is not stored at all, and nothing is written.
//!
//! Every assertion here goes through the router, so it exercises the real wire
//! path (route matching, model constraint validation, serialization) rather than
//! calling a handler directly. No test here waits for the clock: the point of
//! the first rule is that nothing has to be waited for.

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
/// `FlowAliasId` (`pattern ^(TSTALIASID|[0-9a-zA-Z]{10})$`) admits it. The rows
/// here are about the aliases a caller creates, so the helpers below report
/// those.
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

    fn summaries(&self) -> Vec<Value> {
        self.member(&["flowAliasSummaries"])
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("flowAliasSummaries is not an array in {:?}", self.body))
    }

    /// The `(name, id)` of every reported summary of an alias a CALLER created.
    fn caller_aliases(&self) -> Vec<(String, String)> {
        self.summaries()
            .iter()
            .filter(|summary| summary["id"].as_str() != Some(TEST_ALIAS_ID))
            .map(|summary| {
                (
                    summary["name"].as_str().unwrap_or_default().to_string(),
                    summary["id"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    /// The `id` member of every reported alias summary, the caller's and the
    /// reserved test alias alike.
    fn listed_ids(&self) -> Vec<String> {
        self.summaries()
            .iter()
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

    /// The `@httpLabel` spelling of an identifier: the `:` and `/` an ARN
    /// carries are percent-encoded, which is how a client sends a label holding
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

    async fn prepare_flow(&self, case: &str, identifier: &str) -> Answer {
        let escaped = Self::escape(identifier);
        let prepared = self
            .call("POST", &format!("/flows/{escaped}/"), Some(json!({})))
            .await;
        assert_eq!(
            prepared.status, 202,
            "case {case}: expected PrepareFlow -> 202, got {prepared:?}"
        );
        prepared
    }

    async fn get_flow(&self, identifier: &str) -> Answer {
        let escaped = Self::escape(identifier);
        self.call("GET", &format!("/flows/{escaped}/"), None).await
    }

    async fn create_flow_version(&self, identifier: &str) -> Answer {
        let escaped = Self::escape(identifier);
        self.call(
            "POST",
            &format!("/flows/{escaped}/versions"),
            Some(json!({})),
        )
        .await
    }

    async fn create_flow_alias(&self, identifier: &str, request: Value) -> Answer {
        let escaped = Self::escape(identifier);
        self.call("POST", &format!("/flows/{escaped}/aliases"), Some(request))
            .await
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

    async fn delete_flow_version(&self, identifier: &str, version: &str) -> Answer {
        let escaped = Self::escape(identifier);
        self.call(
            "DELETE",
            &format!("/flows/{escaped}/versions/{version}/"),
            None,
        )
        .await
    }

    /// Retire the reporting of a preparation already accepted, by moving the
    /// instant it comes due into the past.
    ///
    /// A flow is prepared by ONE preparation at a time, so a row taking a second
    /// prepare/version round has to let the first preparation's outcome be
    /// reported first; the flow stores that instant rather than sleeping, so the
    /// row moves it instead of waiting out the span. Nothing a row asserts about
    /// the CUT is reached this way: every cut below is taken on the request right
    /// after its own `PrepareFlow`.
    fn retire_pending_preparation(&self, case: &str, flow_id: &str) {
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

/// Whether a value is shaped as an id this service MINTS: the modelled `Id`
/// pattern `^[0-9a-zA-Z]{10}$`, which a `flowAliasId` carries beside the
/// reserved test-alias token.
fn is_minted_id(value: &str) -> bool {
    value.len() == 10 && value.chars().all(|character| character.is_ascii_alphanumeric())
}

/// How a row addresses the flow it deploys: the two forms the modelled
/// `FlowIdentifier` admits (pattern
/// `^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:flow/[0-9a-zA-Z]{10})|([0-9a-zA-Z]{10})$`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Address {
    /// The bare `flowId`.
    BareId,
    /// The flow's own ARN, as the create reported it.
    Arn,
}

/// How many numbered versions a row cuts in sequence, each one right after the
/// `PrepareFlow` that made it cuttable.
const CUTS: &[u32] = &[1, 3];

/// The rule: a flow the service has accepted a sound preparation for is
/// versioned by the IMMEDIATELY following call, which answers 201 and cuts the
/// flow's next version number — the outcome is decided when `PrepareFlow`
/// accepts the call, so nothing is left for a caller to wait out. The rows span
/// the two admitted identifier forms and the number of prepare/version rounds
/// taken in sequence.
#[tokio::test]
async fn a_prepared_flow_is_versioned_by_the_immediately_following_call() {
    for address in [Address::BareId, Address::Arn] {
        for cuts in CUTS {
            let case = format!("addressed by {address:?}, {cuts} prepare/version round(s)");
            let case = case.as_str();
            let emulator = Emulator::new();
            let flow = emulator.create_flow(case, "deploy-plane-flow").await;
            let flow_id = flow.string(&["id"]);
            let flow_arn = flow.string(&["arn"]);
            let identifier = match address {
                Address::BareId => flow_id.clone(),
                Address::Arn => flow_arn.clone(),
            };

            for expected in 1..=*cuts {
                // A flow is prepared by one preparation at a time, so a later
                // round lets the previous outcome be reported before asking for
                // another preparation.
                if expected > 1 {
                    emulator.retire_pending_preparation(case, &flow_id);
                }
                let prepared = emulator.prepare_flow(case, &identifier).await;
                assert_eq!(
                    prepared.string(&["id"]),
                    flow_id,
                    "case {case}: PrepareFlow answers for the flow it addressed, \
                     got {prepared:?}"
                );

                // No wait, no second call in between: the very next request is
                // the cut.
                let cut = emulator.create_flow_version(&identifier).await;
                assert_eq!(
                    cut.status, 201,
                    "case {case}: expected CreateFlowVersion -> 201 right after \
                     PrepareFlow, got {cut:?}"
                );
                assert_eq!(
                    cut.string(&["version"]),
                    expected.to_string(),
                    "case {case}: expected version {expected}, got {cut:?}"
                );
                assert_eq!(
                    cut.string(&["id"]),
                    flow_id,
                    "case {case}: the version is cut from the addressed flow, got {cut:?}"
                );
                assert_eq!(
                    cut.string(&["arn"]),
                    flow_arn,
                    "case {case}: the version reports its flow's ARN, got {cut:?}"
                );
                assert_eq!(
                    cut.string(&["executionRoleArn"]),
                    EXECUTION_ROLE,
                    "case {case}: the version snapshots the flow's role, got {cut:?}"
                );
                assert_eq!(
                    cut.string(&["name"]),
                    "deploy-plane-flow",
                    "case {case}: the version snapshots the flow's name, got {cut:?}"
                );
                // A numbered version is the snapshot of a DRAFT whose preparation
                // succeeded, so it reports that outcome and never a transient
                // status: there is nothing about an immutable version still being
                // decided.
                assert_eq!(
                    cut.string(&["status"]),
                    "Prepared",
                    "case {case}: expected the version to report the preparation's \
                     outcome, got {cut:?}"
                );
                let created_at = cut.string(&["createdAt"]);
                assert!(
                    created_at.contains('T') && created_at.ends_with('Z'),
                    "case {case}: expected createdAt in the modelled date-time wire \
                     form, got {created_at:?}"
                );
            }

            // And the version the last round cut is what an alias can be routed
            // to, with no wait either.
            let deployed = emulator
                .create_flow_alias(
                    &identifier,
                    json!({
                        "name": "deployed",
                        "description": "the alias the deploy path creates",
                        "routingConfiguration": [{ "flowVersion": cuts.to_string() }],
                    }),
                )
                .await;
            assert_eq!(
                deployed.status, 201,
                "case {case}: expected CreateFlowAlias -> 201 for the version just cut, \
                 got {deployed:?}"
            );
            assert_eq!(
                deployed.member(&["routingConfiguration"]),
                &json!([{ "flowVersion": cuts.to_string() }]),
                "case {case}: the alias routes to the version asked for, got {deployed:?}"
            );
        }
    }
}

/// The rule: an alias whose single routing entry names a version the flow holds
/// is created and reported back member for member off the stored record — the
/// ARN is the flow's own with the alias segment appended, the flow id is the
/// addressed flow's, the id is freshly minted, and `name`, `description` and
/// `routingConfiguration` are the request's. Read back through `GetFlowAlias`
/// and enumerated by `ListFlowAliases` beside the flow's reserved test alias.
///
/// The rows span the routing targets a flow holds after the deploy path above:
/// each numbered version and the DRAFT version every flow has.
#[tokio::test]
async fn an_alias_routing_to_a_version_the_flow_holds_is_created_and_reported() {
    for flow_version in ["1", "2", "DRAFT"] {
        let case = format!("routing to {flow_version}");
        let case = case.as_str();
        let emulator = Emulator::new();
        let flow = emulator.create_flow(case, "alias-target-flow").await;
        let flow_id = flow.string(&["id"]);
        let flow_arn = flow.string(&["arn"]);

        // Two versions, each cut right after the preparation that made it
        // cuttable.
        for expected in 1..=2 {
            if expected > 1 {
                emulator.retire_pending_preparation(case, &flow_id);
            }
            emulator.prepare_flow(case, &flow_id).await;
            let cut = emulator.create_flow_version(&flow_id).await;
            assert_eq!(
                cut.string(&["version"]),
                expected.to_string(),
                "case {case}: expected version {expected}, got {cut:?}"
            );
        }

        let created = emulator
            .create_flow_alias(
                &flow_id,
                json!({
                    "name": "release",
                    "description": "the deployment the alias names",
                    "routingConfiguration": [{ "flowVersion": flow_version }],
                }),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreateFlowAlias -> 201, got {created:?}"
        );
        let alias_id = created.string(&["id"]);
        assert!(
            is_minted_id(&alias_id) && alias_id != TEST_ALIAS_ID,
            "case {case}: expected a freshly minted alias id, got {alias_id:?}"
        );
        assert_eq!(
            created.string(&["flowId"]),
            flow_id,
            "case {case}: the alias names the flow it belongs to, got {created:?}"
        );
        assert_eq!(
            created.string(&["arn"]),
            format!("{flow_arn}/alias/{alias_id}"),
            "case {case}: the alias ARN is its flow's ARN with the alias segment, \
             got {created:?}"
        );
        assert_eq!(
            created.string(&["name"]),
            "release",
            "case {case}: the alias carries the name asked for, got {created:?}"
        );
        assert_eq!(
            created.string(&["description"]),
            "the deployment the alias names",
            "case {case}: the alias carries the description asked for, got {created:?}"
        );
        assert_eq!(
            created.member(&["routingConfiguration"]),
            &json!([{ "flowVersion": flow_version }]),
            "case {case}: the alias routes where the request said, got {created:?}"
        );
        for member in ["createdAt", "updatedAt"] {
            let at = created.string(&[member]);
            assert!(
                at.contains('T') && at.ends_with('Z'),
                "case {case}: expected {member} in the modelled date-time wire form, \
                 got {at:?}"
            );
        }

        // The round trip: the stored alias is what every reader reports.
        let read = emulator.get_flow_alias(&flow_id, &alias_id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected GetFlowAlias -> 200, got {read:?}"
        );
        assert_eq!(
            read.body, created.body,
            "case {case}: the stored alias is reported as it was created, got {read:?}"
        );

        let listed = emulator.list_flow_aliases(&flow_id).await;
        assert_eq!(
            listed.status, 200,
            "case {case}: expected ListFlowAliases -> 200, got {listed:?}"
        );
        assert_eq!(
            listed.caller_aliases(),
            vec![("release".to_string(), alias_id.clone())],
            "case {case}: the flow enumerates the alias it was given, got {listed:?}"
        );
        assert!(
            listed.listed_ids().contains(&TEST_ALIAS_ID.to_string()),
            "case {case}: the reserved test alias is enumerated beside it, got {listed:?}"
        );
    }
}

/// How a row makes the routing target unresolvable, and the `flowVersion` token
/// it pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unresolvable {
    /// A flow the store does not hold, so no version of it resolves either.
    UnknownFlow,
    /// A flow that is stored, which holds no version under the token.
    VersionTheFlowDoesNotHold,
    /// A flow that is stored, whose version under the token was cut and then
    /// deleted.
    VersionDeletedAfterItWasCut,
}

/// Rows spanning the ways a `CreateFlowAlias` routing entry fails to resolve: an
/// unknown flow addressed with each version form (the DRAFT token every stored
/// flow has, and a number), a stored flow asked for numbers it does not hold —
/// including `99999`, the widest the modelled `NumericalVersion`
/// (`pattern ^[0-9]{1,5}$`) admits — and a stored flow whose cut version has
/// since been deleted.
const UNRESOLVABLE_ROWS: &[(&str, Unresolvable, &str)] = &[
    ("an unknown flow, routed to its DRAFT", Unresolvable::UnknownFlow, "DRAFT"),
    ("an unknown flow, routed to version 1", Unresolvable::UnknownFlow, "1"),
    (
        "a stored flow, routed to the version after the ones cut",
        Unresolvable::VersionTheFlowDoesNotHold,
        "2",
    ),
    (
        "a stored flow, routed to a number far past the ones cut",
        Unresolvable::VersionTheFlowDoesNotHold,
        "999",
    ),
    (
        "a stored flow, routed to the widest version number there is",
        Unresolvable::VersionTheFlowDoesNotHold,
        "99999",
    ),
    (
        "a stored flow, routed to a version deleted after it was cut",
        Unresolvable::VersionDeletedAfterItWasCut,
        "1",
    ),
];

/// The rule: when a `CreateFlowAlias` cannot resolve the version its
/// `routingConfiguration` names, the refusal is 404 `ResourceNotFoundException`
/// naming resource type `FlowVersion` — the version it pinned is what failed to
/// resolve, whether the flow holds no such version or is not stored at all — and
/// nothing is written.
#[tokio::test]
async fn an_unresolvable_routing_target_is_refused_as_the_missing_flow_version() {
    for (case, unresolvable, flow_version) in UNRESOLVABLE_ROWS {
        let emulator = Emulator::new();
        // A flow id of the modelled form that nothing created, for the rows
        // addressing an unknown flow.
        let unknown_flow = "0123456789".to_string();
        let flow = emulator.create_flow(case, "unresolved-routing-flow").await;
        let stored_flow_id = flow.string(&["id"]);
        if *unresolvable == Unresolvable::VersionDeletedAfterItWasCut {
            emulator.prepare_flow(case, &stored_flow_id).await;
            let cut = emulator.create_flow_version(&stored_flow_id).await;
            assert_eq!(
                cut.string(&["version"]),
                "1",
                "case {case}: expected version 1 to be cut, got {cut:?}"
            );
            let deleted = emulator.delete_flow_version(&stored_flow_id, "1").await;
            assert_eq!(
                deleted.status, 200,
                "case {case}: expected DeleteFlowVersion -> 200, got {deleted:?}"
            );
        }
        let identifier = if *unresolvable == Unresolvable::UnknownFlow {
            unknown_flow
        } else {
            stored_flow_id.clone()
        };

        let refused = emulator
            .create_flow_alias(
                &identifier,
                json!({
                    "name": "unresolved",
                    "routingConfiguration": [{ "flowVersion": flow_version }],
                }),
            )
            .await;
        assert_eq!(
            refused.status, 404,
            "case {case}: expected CreateFlowAlias -> 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: expected ResourceNotFoundException, got {refused:?}"
        );
        assert_eq!(
            refused.string(&["message"]),
            not_found_message("CreateFlowAlias", "FlowVersion"),
            "case {case}: expected the refusal to name the operation and the flow \
             version it could not resolve, got {refused:?}"
        );

        // Nothing was written: the stored flow holds no caller-created alias,
        // and it is still the flow it was.
        let listed = emulator.list_flow_aliases(&stored_flow_id).await;
        assert_eq!(
            listed.caller_aliases(),
            Vec::<(String, String)>::new(),
            "case {case}: the refused create wrote no alias, got {listed:?}"
        );
        assert_eq!(
            emulator.get_flow(&stored_flow_id).await.status,
            200,
            "case {case}: the refusal left the flow readable"
        );
    }
}
