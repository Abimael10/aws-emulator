// SPDX-License-Identifier: CC0-1.0

//! An alias's NAME is the flow's to hold once, end to end over HTTP: a
//! `CreateFlowAlias` under a name an alias of the same flow already holds, and an
//! `UpdateFlowAlias` that would rename an alias onto a sibling's name, are
//! refused with the modelled `ConflictException` (409) naming the name asked for
//! and the id of the alias already holding it — and nothing is written, so the
//! flow still reports exactly one alias under that name.
//!
//! The scope the name is unique in is ONE flow: the same name on another flow is
//! accepted, a name that differs at all is accepted, re-asserting the name an
//! alias already carries is no conflict, and a name a delete freed is free
//! again.
//!
//! The table below spans the forms the modelled `Name`
//! (`pattern ^([0-9a-zA-Z][_-]?){1,100}$`) admits: a single character, a name
//! mixing both separators the pattern allows, the widest name it admits, and a
//! pair differing only in case.
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

    /// The `(name, id)` of every reported CALLER-created alias summary, in the
    /// order the response listed them: the flow's reserved test alias, which the
    /// service creates with the flow, is not one a caller named, so these rows
    /// about the caller-settable alias namespace leave it out.
    fn listed(&self) -> Vec<(String, String)> {
        self.member(&["flowAliasSummaries"])
            .as_array()
            .unwrap_or_else(|| panic!("flowAliasSummaries is not an array in {:?}", self.body))
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

    async fn create_flow_alias(&self, flow_id: &str, name: &str) -> Answer {
        self.call(
            "POST",
            &format!("/flows/{flow_id}/aliases"),
            Some(json!({
                "name": name,
                "routingConfiguration": [{ "flowVersion": "1" }],
            })),
        )
        .await
    }

    async fn update_flow_alias(&self, flow_id: &str, alias_id: &str, name: &str) -> Answer {
        self.call(
            "PUT",
            &format!("/flows/{flow_id}/aliases/{alias_id}"),
            Some(json!({
                "name": name,
                "routingConfiguration": [{ "flowVersion": "1" }],
            })),
        )
        .await
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

/// The refusal a create or an update answers when the alias name it asks for is
/// one an alias of the same flow already holds. The sentence is fixed text for
/// that error class; its variable parts are the operation the caller invoked, the
/// name the request asked for and the id of the alias holding it.
fn name_conflict_message(operation: &str, name: &str, alias_id: &str) -> String {
    format!(
        "Couldn't perform {operation} operation. The name {name} already exists for id \
         {alias_id}. Retry your request with a different name."
    )
}

/// The refusal a read answers for an alias that is not there.
fn not_found_message(operation: &str, resource_type: &str) -> String {
    format!(
        "Couldn't perform {operation} operation. Failed to retrieve resource type \
         {resource_type} because it doesn't exist. Check the resource ID and retry \
         your request."
    )
}

/// Assert `answer` is the modelled 409 for a name an alias of the flow already
/// holds, worded as this error class words it.
fn assert_name_conflict(
    case: &str,
    operation: &str,
    name: &str,
    holder: &str,
    answer: &Answer,
) {
    assert_eq!(
        answer.status, 409,
        "case {case}: expected {operation} -> 409, got {answer:?}"
    );
    assert_eq!(
        answer.error_type.as_deref(),
        Some("ConflictException"),
        "case {case}: expected ConflictException from {operation}, got {answer:?}"
    );
    assert_eq!(
        answer.string(&["message"]),
        name_conflict_message(operation, name, holder),
        "case {case}: expected the refusal to name the name asked for and the alias \
         holding it, got {:?}",
        answer.member(&["message"])
    );
}

/// One row of the name table: a name, and another name of the same form that no
/// alias holds.
struct Row {
    case: &'static str,
    name: String,
    free: String,
}

/// Rows spanning the forms the modelled `Name`
/// (`pattern ^([0-9a-zA-Z][_-]?){1,100}$`) admits: one character, a name using
/// both separators the pattern allows, the widest name it admits (100 groups of
/// one character), and a pair differing only in case — none of which the rule is
/// keyed to.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a single-character name",
            name: "a".to_string(),
            free: "b".to_string(),
        },
        Row {
            case: "a name using both separators the pattern admits",
            name: "A1_b-c2".to_string(),
            free: "A1_b-c3".to_string(),
        },
        Row {
            case: "the widest name the pattern admits",
            name: "a".repeat(100),
            free: format!("{}b", "a".repeat(99)),
        },
        Row {
            case: "two names differing only in case",
            name: "Promote".to_string(),
            free: "promote".to_string(),
        },
    ]
}

/// The rule, at `CreateFlowAlias`: a second create under a name an alias of the
/// same flow already holds is refused 409 `ConflictException` naming that name
/// and that alias's id, and mints nothing — while the same name on another flow,
/// and any name that differs at all, are accepted.
#[tokio::test]
async fn create_flow_alias_refuses_a_name_an_alias_of_the_flow_already_holds() {
    for row in rows() {
        let case = row.case;
        let name = row.name.as_str();
        let emulator = Emulator::new();
        let flow_id = emulator.flow(case, "alias-name-owner").await;
        let other_flow_id = emulator.flow(case, "alias-name-other").await;

        let first = emulator.create_flow_alias(&flow_id, name).await;
        assert_eq!(
            first.status, 201,
            "case {case}: expected CreateFlowAlias -> 201, got {first:?}"
        );
        let holder = first.string(&["id"]);

        let refused = emulator.create_flow_alias(&flow_id, name).await;
        assert_name_conflict(case, "CreateFlowAlias", name, &holder, &refused);

        // Nothing was minted: the flow holds exactly one alias, the first one.
        assert_eq!(
            emulator.list_flow_aliases(&flow_id).await.listed(),
            vec![(name.to_string(), holder.clone())],
            "case {case}: the refused create left the flow with one alias under the name"
        );

        // The scope the name is unique in is ONE flow: another flow may hold an
        // alias of the same name.
        let elsewhere = emulator.create_flow_alias(&other_flow_id, name).await;
        assert_eq!(
            elsewhere.status, 201,
            "case {case}: a name is unique within one flow, got {elsewhere:?}"
        );

        // A name that differs at all is free.
        let free = emulator.create_flow_alias(&flow_id, &row.free).await;
        assert_eq!(
            free.status, 201,
            "case {case} ({} vs {}): a different name is accepted, got {free:?}",
            row.free, name
        );
        assert_ne!(
            free.string(&["id"]),
            holder,
            "case {case}: the second name mints its own alias, got {free:?}"
        );

        // And a name a delete freed is free again: the delete releases it, and
        // the create that follows mints a NEW alias rather than resurrecting the
        // deleted one.
        let deleted = emulator.delete_flow_alias(&flow_id, &holder).await;
        assert_eq!(
            deleted.status, 200,
            "case {case}: expected DeleteFlowAlias -> 200, got {deleted:?}"
        );
        let again = emulator.create_flow_alias(&flow_id, name).await;
        assert_eq!(
            again.status, 201,
            "case {case}: a name the delete freed is free again, got {again:?}"
        );
        assert_ne!(
            again.string(&["id"]),
            holder,
            "case {case}: the re-created alias is a new alias, got {again:?}"
        );
    }
}

/// The rule, at `UpdateFlowAlias`: a rename onto a name a SIBLING alias of the
/// same flow holds is refused 409 `ConflictException` and writes nothing, while
/// re-asserting the name the alias already carries, and renaming onto a free
/// name, are served.
#[tokio::test]
async fn update_flow_alias_refuses_a_rename_onto_a_siblings_name() {
    for row in rows() {
        let case = row.case;
        let name = row.name.as_str();
        let emulator = Emulator::new();
        let flow_id = emulator.flow(case, "alias-rename-owner").await;

        let sibling = emulator.create_flow_alias(&flow_id, name).await;
        assert_eq!(
            sibling.status, 201,
            "case {case}: expected CreateFlowAlias -> 201, got {sibling:?}"
        );
        let holder = sibling.string(&["id"]);

        let renamed = emulator.create_flow_alias(&flow_id, &row.free).await;
        assert_eq!(
            renamed.status, 201,
            "case {case}: expected CreateFlowAlias -> 201, got {renamed:?}"
        );
        let renamed_id = renamed.string(&["id"]);

        let refused = emulator
            .update_flow_alias(&flow_id, &renamed_id, name)
            .await;
        assert_name_conflict(case, "UpdateFlowAlias", name, &holder, &refused);

        // Nothing was written: the alias still carries the name it had.
        let read = emulator.get_flow_alias(&flow_id, &renamed_id).await;
        assert_eq!(
            read.body, renamed.body,
            "case {case}: the refused rename wrote nothing, got {read:?}"
        );

        // An alias is not its own obstacle: re-asserting the name it carries is
        // served.
        let itself = emulator
            .update_flow_alias(&flow_id, &renamed_id, &row.free)
            .await;
        assert_eq!(
            itself.status, 200,
            "case {case}: re-asserting the alias's own name is no conflict, got {itself:?}"
        );

        // And once the sibling holding the name is deleted, the rename is served.
        let deleted = emulator.delete_flow_alias(&flow_id, &holder).await;
        assert_eq!(
            deleted.status, 200,
            "case {case}: expected DeleteFlowAlias -> 200, got {deleted:?}"
        );
        let served = emulator
            .update_flow_alias(&flow_id, &renamed_id, name)
            .await;
        assert_eq!(
            served.status, 200,
            "case {case}: the rename is served once nothing holds the name, got {served:?}"
        );
        assert_eq!(
            served.string(&["name"]),
            name,
            "case {case}: the alias carries the name the update gave it, got {served:?}"
        );
        assert_eq!(
            emulator
                .get_flow_alias(&flow_id, &renamed_id)
                .await
                .string(&["name"]),
            name,
            "case {case}: the rename is the stored alias's"
        );
    }
}

/// A `clientToken` replay is not a name conflict: the retry reports the alias the
/// token already created rather than colliding with it, and still only one alias
/// exists.
#[tokio::test]
async fn a_client_token_retry_replays_the_alias_rather_than_conflicting() {
    let case = "the same clientToken and name twice";
    let emulator = Emulator::new();
    let flow_id = emulator.flow(case, "alias-token-flow").await;

    // The modelled `ClientToken` is `length 33..256, pattern
    // ^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`.
    let token = "idempotency-token-for-the-named-alias";
    let request = json!({
        "name": "token-alias",
        "routingConfiguration": [{ "flowVersion": "1" }],
        "clientToken": token,
    });
    let created = emulator
        .call(
            "POST",
            &format!("/flows/{flow_id}/aliases"),
            Some(request.clone()),
        )
        .await;
    assert_eq!(
        created.status, 201,
        "case {case}: expected CreateFlowAlias -> 201, got {created:?}"
    );

    let retried = emulator
        .call("POST", &format!("/flows/{flow_id}/aliases"), Some(request))
        .await;
    assert_eq!(
        retried.body, created.body,
        "case {case}: the retry replays the alias the token created, got {retried:?}"
    );
    assert_eq!(
        emulator.list_flow_aliases(&flow_id).await.listed(),
        vec![("token-alias".to_string(), created.string(&["id"]))],
        "case {case}: the retry created no second alias"
    );

    // A DIFFERENT token under the same name is a second create, so it is the
    // conflict the rule owes.
    let conflicting = emulator
        .call(
            "POST",
            &format!("/flows/{flow_id}/aliases"),
            Some(json!({
                "name": "token-alias",
                "routingConfiguration": [{ "flowVersion": "1" }],
                "clientToken": "another-idempotency-token-for-the-alias",
            })),
        )
        .await;
    assert_name_conflict(
        case,
        "CreateFlowAlias",
        "token-alias",
        &created.string(&["id"]),
        &conflicting,
    );
}

/// `DeleteFlowAlias` removes the alias the request names and nothing else: the
/// alias is unreadable afterwards, a repeat is the modelled not-found, and the
/// flow, its versions and its other aliases stand.
#[tokio::test]
async fn delete_flow_alias_removes_the_alias_the_request_names() {
    let case = "two aliases on one flow";
    let emulator = Emulator::new();
    let flow_id = emulator.flow(case, "alias-delete-flow").await;

    let kept = emulator.create_flow_alias(&flow_id, "kept-alias").await;
    assert_eq!(
        kept.status, 201,
        "case {case}: expected CreateFlowAlias -> 201, got {kept:?}"
    );
    let kept_id = kept.string(&["id"]);
    let doomed = emulator.create_flow_alias(&flow_id, "doomed-alias").await;
    assert_eq!(
        doomed.status, 201,
        "case {case}: expected CreateFlowAlias -> 201, got {doomed:?}"
    );
    let doomed_id = doomed.string(&["id"]);

    let deleted = emulator.delete_flow_alias(&flow_id, &doomed_id).await;
    assert_eq!(
        deleted.status, 200,
        "case {case}: expected DeleteFlowAlias -> 200, got {deleted:?}"
    );
    // `DeleteFlowAliasResponse` models exactly two members: the flow the alias
    // belonged to and the alias's own id.
    assert_eq!(
        deleted.body,
        json!({ "flowId": flow_id, "id": doomed_id }),
        "case {case}: expected DeleteFlowAlias to report the flow and the alias, got {:?}",
        deleted.body
    );

    // The deleted alias is unreadable, and a repeat of the delete is the
    // modelled not-found rather than a second success.
    for (operation, answer) in [
        (
            "GetFlowAlias",
            emulator.get_flow_alias(&flow_id, &doomed_id).await,
        ),
        (
            "DeleteFlowAlias",
            emulator.delete_flow_alias(&flow_id, &doomed_id).await,
        ),
    ] {
        assert_eq!(
            answer.status, 404,
            "case {case} ({operation}): expected 404 for the deleted alias, got {answer:?}"
        );
        assert_eq!(
            answer.string(&["message"]),
            not_found_message(operation, "FlowAlias"),
            "case {case} ({operation}): the refusal names the resource type it could not \
             resolve, got {answer:?}"
        );
    }

    // The flow keeps everything else it held.
    assert_eq!(
        emulator.list_flow_aliases(&flow_id).await.listed(),
        vec![("kept-alias".to_string(), kept_id.clone())],
        "case {case}: the delete removed the alias it named alone"
    );
    assert_eq!(
        emulator
            .call("GET", &format!("/flows/{flow_id}/"), None)
            .await
            .status,
        200,
        "case {case}: the flow the alias belonged to stands"
    );
    assert_eq!(
        emulator
            .call("GET", &format!("/flows/{flow_id}/versions/1"), None)
            .await
            .status,
        200,
        "case {case}: the version the alias routed to stands"
    );
}

/// Rows: the identifier forms `DeleteFlowAlias` addresses an alias by — the two
/// the modelled `FlowIdentifier` admits crossed with the two the modelled
/// `FlowAliasIdentifier` admits — each naming the same alias; plus the pairs that
/// name no alias of the addressed flow, and the values of neither modelled form.
#[tokio::test]
async fn delete_flow_alias_resolves_every_modelled_identifier_form() {
    let case = "every identifier form";
    let emulator = Emulator::new();
    let flow_id = emulator.flow(case, "alias-delete-forms").await;
    let flow = emulator
        .call("GET", &format!("/flows/{flow_id}/"), None)
        .await;
    let flow_arn = flow.string(&["arn"]);

    // A flow id and an alias id go into the path as they are; an ARN's `:` and
    // `/` are percent-encoded, which is how a client sends a label carrying them.
    let escape = |identifier: &str| identifier.replace(':', "%3A").replace('/', "%2F");

    for (form, by_flow_arn, by_alias_arn) in [
        ("bare ids", false, false),
        ("a flow ARN", true, false),
        ("an alias ARN", false, true),
        ("both ARNs", true, true),
    ] {
        let created = emulator.create_flow_alias(&flow_id, "form-alias").await;
        assert_eq!(
            created.status, 201,
            "case {case} ({form}): expected CreateFlowAlias -> 201, got {created:?}"
        );
        let alias_id = created.string(&["id"]);
        let alias_arn = created.string(&["arn"]);

        let flow_identifier = if by_flow_arn { &flow_arn } else { &flow_id };
        let alias_identifier = if by_alias_arn { &alias_arn } else { &alias_id };
        let deleted = emulator
            .delete_flow_alias(&escape(flow_identifier), &escape(alias_identifier))
            .await;
        assert_eq!(
            deleted.status, 200,
            "case {case} ({form}): expected DeleteFlowAlias -> 200, got {deleted:?}"
        );
        assert_eq!(
            deleted.body,
            json!({ "flowId": flow_id, "id": alias_id }),
            "case {case} ({form}): every form names the same alias, got {:?}",
            deleted.body
        );
        assert_eq!(
            emulator.list_flow_aliases(&flow_id).await.listed(),
            Vec::<(String, String)>::new(),
            "case {case} ({form}): the flow holds no caller-created alias after the delete"
        );
    }

    // An alias of ANOTHER flow is a resource this flow does not hold, and an
    // unknown flow is the missing flow it is.
    let other_flow_id = emulator.flow(case, "alias-delete-bystander").await;
    let elsewhere = emulator
        .create_flow_alias(&other_flow_id, "bystander-alias")
        .await;
    let elsewhere_id = elsewhere.string(&["id"]);
    let elsewhere_arn = elsewhere.string(&["arn"]);
    for (form, flow_identifier, alias_identifier, resource) in [
        ("an unknown flow", "0123456789", elsewhere_id.as_str(), "Flow"),
        (
            "an alias id nothing created",
            flow_id.as_str(),
            "0123456789",
            "FlowAlias",
        ),
        (
            "another flow's alias id",
            flow_id.as_str(),
            elsewhere_id.as_str(),
            "FlowAlias",
        ),
        (
            "another flow's alias ARN",
            flow_id.as_str(),
            elsewhere_arn.as_str(),
            "FlowAlias",
        ),
    ] {
        let refused = emulator
            .delete_flow_alias(flow_identifier, &escape(alias_identifier))
            .await;
        assert_eq!(
            refused.status, 404,
            "case {case} ({form}): expected DeleteFlowAlias -> 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case} ({form}): expected ResourceNotFoundException, got {refused:?}"
        );
        assert_eq!(
            refused.string(&["message"]),
            not_found_message("DeleteFlowAlias", resource),
            "case {case} ({form}): the refusal names the resource type it could not \
             resolve, got {refused:?}"
        );
    }

    // Values of neither modelled form are bad requests, not missing resources.
    for (form, flow_identifier, alias_identifier) in [
        (
            "a flow name as the flow identifier",
            "alias-delete-forms",
            elsewhere_id.as_str(),
        ),
        (
            "a flow id one character short",
            "012345678",
            elsewhere_id.as_str(),
        ),
        (
            "an alias name as the alias identifier",
            other_flow_id.as_str(),
            "bystander-alias",
        ),
        (
            "an alias id one character long",
            other_flow_id.as_str(),
            "a",
        ),
    ] {
        let refused = emulator
            .delete_flow_alias(flow_identifier, alias_identifier)
            .await;
        assert_eq!(
            refused.status, 400,
            "case {case} ({form}): expected DeleteFlowAlias -> 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case} ({form}): expected ValidationException, got {refused:?}"
        );
    }

    // Every refusal above removed nothing.
    assert_eq!(
        emulator.list_flow_aliases(&other_flow_id).await.listed(),
        vec![("bystander-alias".to_string(), elsewhere_id.clone())],
        "case {case}: the bystander flow keeps the alias created on it"
    );
}
