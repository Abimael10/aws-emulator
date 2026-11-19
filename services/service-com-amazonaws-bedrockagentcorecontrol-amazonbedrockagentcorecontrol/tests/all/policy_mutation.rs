// SPDX-License-Identifier: CC0-1.0

//! `UpdatePolicy` end to end over HTTP: the change an accepted request applies to a stored
//! policy, and the requests this control plane refuses.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint walk and the handler are all exercised against the one shared engine.
//!
//! The rule under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="UpdatePolicy,GetPolicy,CreatePolicy")` and
//! `smithy_query(query="shape",shape="UpdatedDescription,PolicyDefinition,EnforcementMode")`):
//! an accepted `UpdatePolicy` replaces exactly the modifiable members the request carries —
//! the definition ("This replaces the existing policy definition with new logic while
//! maintaining the policy's identity"), the enforcement mode ("If you omit this field, the
//! policy's existing enforcement mode is unchanged") and the description under the
//! `UpdatedDescription` PATCH rule ("When present in an update request, the description is
//! replaced with optionalValue. When absent, the description is left unchanged. To unset the
//! description, include the wrapper with optionalValue not specified") — leaves the
//! policy's identity (its id, its name, its ARN, the engine that holds it, `createdAt`) as
//! the create stored it, records the instant of the change in `updatedAt`, reports the
//! policy `UPDATING` with the transition back to `ACTIVE` owed by the clock and not by the
//! call ("This is an asynchronous operation. Use the GetPolicy operation to poll the status
//! field to track completion"), and reads back through `GetPolicy` exactly as the update
//! reported it; a policy whose own asynchronous work is still under way — so one that is
//! not `ACTIVE` — refuses a further change the modelled `ConflictException` (HTTP 409) and
//! keeps what it held; and an engine or policy id this account does not hold is the
//! modelled `ResourceNotFoundException` (HTTP 404) and changes nothing.
//!
//! The rows differ along what the rule generalises over: which members the request carries
//! at all, which member of the `definition` union each side of the change uses, the
//! enforcement mode before and after, the form of the description wrapper, the length of a
//! replacement description at both ends of `Description`'s 1..4096 `@length`, and the
//! validation mode the request names.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The Region the rows sign for.
const REGION: &str = "us-east-1";

/// The account every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// How long a row waits for a clock-owed transition before it gives up. The span itself is
/// the policy plane's own; this only bounds a hang, so it is generous.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll loop waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// `Description`'s `@length` maximum, which `UpdatedDescription$optionalValue` targets
/// (`smithy_query(query="shape",shape="UpdatedDescription")`).
const MAX_DESCRIPTION_LENGTH: usize = 4096;

/// The members `UpdatePolicyResponse` and `GetPolicyResponse` both declare, so a round trip
/// compares the whole envelope rather than the members one row happens to set
/// (`smithy_query(query="operation",operation="UpdatePolicy,GetPolicy")`).
const POLICY_MEMBERS: [&str; 11] = [
    "policyId",
    "name",
    "policyEngineId",
    "policyArn",
    "status",
    "statusReasons",
    "definition",
    "description",
    "enforcementMode",
    "createdAt",
    "updatedAt",
];

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service, which is what the router routes on.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One answer: the HTTP status and the parsed body.
struct Answer {
    status: u16,
    body: Value,
}

async fn send(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<&Value>) -> Answer {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        },
    }
}

/// The error code `restJson1` puts in the body's `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

async fn create_policy_engine(router: &MultiServiceRouter, body: &Value) -> Answer {
    send(router, "POST", "/policy-engines", Some(body)).await
}

async fn create_policy(router: &MultiServiceRouter, policy_engine_id: &str, body: &Value) -> Answer {
    send(
        router,
        "POST",
        &format!("/policy-engines/{policy_engine_id}/policies"),
        Some(body),
    )
    .await
}

async fn get_policy(router: &MultiServiceRouter, policy_engine_id: &str, policy_id: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/policy-engines/{policy_engine_id}/policies/{policy_id}"),
        None,
    )
    .await
}

async fn update_policy(
    router: &MultiServiceRouter,
    policy_engine_id: &str,
    policy_id: &str,
    body: &Value,
) -> Answer {
    send(
        router,
        "PATCH",
        &format!("/policy-engines/{policy_engine_id}/policies/{policy_id}"),
        Some(body),
    )
    .await
}

/// A Cedar definition: `CedarPolicy$statement` is 35..10000 characters
/// (`smithy_query(query="shape",shape="CedarPolicy")`).
fn cedar_definition(action: &str) -> Value {
    json!({"cedar": {
        "statement": format!("permit(principal, action == Action::\"{action}\", resource);"),
    }})
}

/// A Dogwood definition, the other statement-carrying member of the union
/// (`smithy_query(query="shape",shape="PolicyDefinition")`).
fn dogwood_definition(tool: &str) -> Value {
    json!({"policy": {
        "statement": format!("forbid(principal, action, resource) when {{ tool == \"{tool}\" }};"),
    }})
}

/// The sentence this plane refuses a change with when the policy's own asynchronous work is
/// still under way. The text around the id is the service's; the only variable part is the
/// id the request's path carried.
fn policy_concurrent_modification_message(policy_id: &str) -> String {
    format!(
        "Concurrent modification on policy with ID {policy_id} detected, \
         please try again once the policy status is ACTIVE"
    )
}

/// The sentence this plane words a missing engine with.
fn engine_not_found_message(policy_engine_id: &str) -> String {
    format!("Policy engine with ID {policy_engine_id} not found")
}

/// The sentence this plane words a missing policy with.
fn policy_not_found_message(policy_id: &str) -> String {
    format!("Policy with ID {policy_id} not found")
}

/// A `DateTimestamp` is an RFC 3339 string on this wire (`@timestampFormat("date-time")`),
/// which is what a caller parses: a date, a `T`, a time and a `Z` offset.
fn is_date_time(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str).is_some_and(|rendered| {
        rendered.len() >= "2026-01-01T00:00:00Z".len()
            && rendered.as_bytes()[4] == b'-'
            && rendered.as_bytes()[10] == b'T'
            && rendered.ends_with('Z')
    })
}

/// Look again until the policy reports `ACTIVE`, which is what a caller polling the status
/// field does. The deadline only catches a policy that never settles at all.
async fn poll_until_active(
    router: &MultiServiceRouter,
    policy_engine_id: &str,
    policy_id: &str,
    case: &str,
) -> Answer {
    let started = Instant::now();
    loop {
        let read = get_policy(router, policy_engine_id, policy_id).await;
        if read.status == 200 && text(&read.body, "status") == "ACTIVE" {
            return read;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the policy never reached ACTIVE, last answer {} ({})",
            read.status,
            read.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create an engine and a policy in it, and wait out both creates' clock-owed transitions,
/// so a row starts from a policy that is `ACTIVE` and can therefore take a change.
async fn active_policy(
    router: &MultiServiceRouter,
    engine_name: &str,
    policy_body: &Value,
    case: &str,
) -> (String, String) {
    let created_engine = create_policy_engine(router, &json!({"name": engine_name})).await;
    assert_eq!(
        created_engine.status, 202,
        "case {case:?}: the engine must be created, got {} ({})",
        created_engine.status, created_engine.body
    );
    let policy_engine_id = text(&created_engine.body, "policyEngineId");
    let created = create_policy(router, &policy_engine_id, policy_body).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: the policy must be created, got {} ({})",
        created.status, created.body
    );
    let policy_id = text(&created.body, "policyId");
    poll_until_active(router, &policy_engine_id, &policy_id, case).await;
    (policy_engine_id, policy_id)
}

/// Which form of the `UpdatedDescription` wrapper a row sends.
#[derive(Debug, Clone, Copy)]
enum Patch {
    /// No wrapper at all, so the description is left unchanged.
    NoWrapper,
    /// The wrapper with no `optionalValue`, so the description is unset.
    EmptyWrapper,
    /// The wrapper carrying a replacement value.
    Value(&'static str),
    /// The wrapper carrying the longest value `Description` admits, built at run time
    /// rather than written out here.
    LongestValue,
}

impl Patch {
    /// What this form contributes to the `UpdatePolicy` body.
    fn member(self, longest: &str) -> Option<Value> {
        match self {
            Patch::NoWrapper => None,
            Patch::EmptyWrapper => Some(json!({})),
            Patch::Value(value) => Some(json!({"optionalValue": value})),
            Patch::LongestValue => Some(json!({"optionalValue": longest})),
        }
    }

    /// The description the policy holds after this form was applied to a policy holding
    /// `before`.
    fn expected(self, before: Option<&str>, longest: &str) -> Option<String> {
        match self {
            Patch::NoWrapper => before.map(str::to_string),
            Patch::EmptyWrapper => None,
            Patch::Value(value) => Some(value.to_string()),
            Patch::LongestValue => Some(longest.to_string()),
        }
    }
}

/// One row of the change table.
struct Row {
    case: &'static str,
    /// The policy's name (`PolicyName` is `^[A-Za-z][A-Za-z0-9_]*$`, 1..48).
    name: &'static str,
    /// The definition the create stored.
    definition_before: Value,
    /// The description the create stored, if any.
    description_before: Option<&'static str>,
    /// The enforcement mode the create named, if any; `None` takes the model's `ACTIVE`
    /// default (`smithy_query(query="shape",shape="EnforcementMode")`).
    mode_before: Option<&'static str>,
    /// The definition the change carries, if any.
    definition_after: Option<Value>,
    /// The enforcement mode the change carries, if any.
    mode_after: Option<&'static str>,
    patch: Patch,
    /// The `validationMode` the change names, if any; `None` takes the model's
    /// `FAIL_ON_ANY_FINDINGS` default.
    validation_mode: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "every modifiable member at once: definition, mode and description",
            name: "Change_everything",
            definition_before: cedar_definition("invokeTool"),
            description_before: Some("the first description"),
            mode_before: None,
            definition_after: Some(dogwood_definition("search")),
            mode_after: Some("LOG_ONLY"),
            patch: Patch::Value("the second description"),
            validation_mode: Some("IGNORE_ALL_FINDINGS"),
        },
        Row {
            case: "a definition on its own, Cedar replacing Cedar",
            name: "Only_the_definition",
            definition_before: cedar_definition("invokeTool"),
            description_before: Some("left exactly as it was"),
            mode_before: Some("LOG_ONLY"),
            definition_after: Some(cedar_definition("listTools")),
            mode_after: None,
            patch: Patch::NoWrapper,
            validation_mode: None,
        },
        Row {
            case: "a mode on its own, LOG_ONLY to ACTIVE",
            name: "Only_the_mode",
            definition_before: dogwood_definition("search"),
            description_before: None,
            mode_before: Some("LOG_ONLY"),
            definition_after: None,
            mode_after: Some("ACTIVE"),
            patch: Patch::NoWrapper,
            validation_mode: Some("FAIL_ON_ANY_FINDINGS"),
        },
        Row {
            case: "the empty wrapper unsets the description and nothing else changes",
            name: "Clear_the_description",
            definition_before: cedar_definition("invokeTool"),
            description_before: Some("about to be cleared"),
            mode_before: None,
            definition_after: None,
            mode_after: None,
            patch: Patch::EmptyWrapper,
            validation_mode: None,
        },
        Row {
            case: "a policy with no description takes one, Dogwood replacing Cedar",
            name: "A",
            definition_before: cedar_definition("invokeTool"),
            description_before: None,
            mode_before: None,
            definition_after: Some(dogwood_definition("invoke")),
            mode_after: None,
            patch: Patch::Value("x"),
            validation_mode: None,
        },
        Row {
            case: "an empty body changes nothing but the status and updatedAt",
            name: "Policy_with_a_long_but_valid_name_for_account",
            definition_before: dogwood_definition("search"),
            description_before: Some("survives an empty change"),
            mode_before: Some("LOG_ONLY"),
            definition_after: None,
            mode_after: None,
            patch: Patch::NoWrapper,
            validation_mode: None,
        },
        Row {
            case: "the longest Description the model admits, Cedar replacing Dogwood",
            name: "Longest_description",
            definition_before: dogwood_definition("search"),
            description_before: Some("x"),
            mode_before: None,
            definition_after: Some(cedar_definition("invokeTool")),
            mode_after: Some("LOG_ONLY"),
            patch: Patch::LongestValue,
            validation_mode: None,
        },
    ]
}

impl Row {
    /// The `CreatePolicy` body this row starts from.
    fn create_body(&self) -> Value {
        let mut body = json!({"name": self.name, "definition": self.definition_before});
        if let Some(description) = self.description_before {
            body["description"] = json!(description);
        }
        if let Some(mode) = self.mode_before {
            body["enforcementMode"] = json!(mode);
        }
        body
    }

    /// The `UpdatePolicy` body this row sends.
    fn update_body(&self, longest: &str) -> Value {
        let mut body = json!({});
        if let Some(definition) = &self.definition_after {
            body["definition"] = definition.clone();
        }
        if let Some(mode) = self.mode_after {
            body["enforcementMode"] = json!(mode);
        }
        if let Some(wrapper) = self.patch.member(longest) {
            body["description"] = wrapper;
        }
        if let Some(validation_mode) = self.validation_mode {
            body["validationMode"] = json!(validation_mode);
        }
        body
    }

    /// The definition the policy holds after the change: the one the change carried, or the
    /// one the create stored when it carried none.
    fn expected_definition(&self) -> Value {
        self.definition_after
            .clone()
            .unwrap_or_else(|| self.definition_before.clone())
    }

    /// The enforcement mode the policy holds after the change: the one the change carried,
    /// or the one the create stored (itself the model's `ACTIVE` default when the create
    /// named none).
    fn expected_mode(&self) -> &'static str {
        self.mode_after
            .or(self.mode_before)
            .unwrap_or("ACTIVE")
    }
}

/// An accepted change replaces exactly the members it carries, leaves the policy's identity
/// alone, reports `UPDATING`, reads back, and survives the clock-owed transition to
/// `ACTIVE`.
#[tokio::test]
async fn an_accepted_change_replaces_only_the_members_it_carries() {
    let longest = "d".repeat(MAX_DESCRIPTION_LENGTH);
    let longest = longest.as_str();
    for row in rows() {
        let router = router();
        let (policy_engine_id, policy_id) =
            active_policy(&router, "Mutation_engine", &row.create_body(), row.case).await;
        let before = get_policy(&router, &policy_engine_id, &policy_id).await;

        let updated = update_policy(
            &router,
            &policy_engine_id,
            &policy_id,
            &row.update_body(longest),
        )
        .await;
        assert_eq!(
            updated.status, 202,
            "case {:?}: expected HTTP 202 from UpdatePolicy, got {} ({})",
            row.case, updated.status, updated.body
        );
        let expected_description = row.patch.expected(row.description_before, longest);
        assert_eq!(
            updated
                .body
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            expected_description,
            "case {:?}: expected the description {expected_description:?}, got {}",
            row.case,
            updated.body
        );
        assert_eq!(
            updated.body.get("definition"),
            Some(&row.expected_definition()),
            "case {:?}: expected the definition {}, got {}",
            row.case,
            row.expected_definition(),
            updated.body
        );
        assert_eq!(
            text(&updated.body, "enforcementMode"),
            row.expected_mode(),
            "case {:?}: expected the enforcement mode {}, got {}",
            row.case,
            row.expected_mode(),
            updated.body
        );
        assert_eq!(
            text(&updated.body, "status"),
            "UPDATING",
            "case {:?}: an accepted change reports UPDATING, got {}",
            row.case,
            updated.body
        );
        assert_eq!(
            updated.body.get("statusReasons").cloned(),
            Some(json!([])),
            "case {:?}: nothing failed, so the status carries no reason, got {}",
            row.case,
            updated.body
        );
        for member in ["createdAt", "updatedAt"] {
            assert!(
                is_date_time(updated.body.get(member)),
                "case {:?}: {member} is an RFC 3339 string on this wire, got {}",
                row.case,
                updated.body
            );
        }
        // The policy's identity is not modifiable by this operation: the id the create
        // minted, the name, the ARN, the engine that holds it and the creation instant are
        // reported unchanged.
        for member in [
            "policyId",
            "name",
            "policyEngineId",
            "policyArn",
            "createdAt",
        ] {
            assert_eq!(
                updated.body.get(member),
                before.body.get(member),
                "case {:?}: the change leaves the {member} the create stored, got {}",
                row.case,
                updated.body
            );
        }
        assert_eq!(
            text(&updated.body, "policyArn"),
            format!(
                "arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:policy-engine/\
                 {policy_engine_id}/policy/{policy_id}"
            ),
            "case {:?}: the ARN still nests the policy under the engine that evaluates it, \
             got {}",
            row.case,
            updated.body
        );
        assert_ne!(
            updated.body.get("updatedAt"),
            before.body.get("updatedAt"),
            "case {:?}: the change records when the policy was last modified, got {}",
            row.case,
            updated.body
        );

        // The round trip: the read reports the record the change wrote.
        let read = get_policy(&router, &policy_engine_id, &policy_id).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: the policy must read back, got {} ({})",
            row.case, read.status, read.body
        );
        for member in POLICY_MEMBERS {
            // `status` is read at the instant of the answer, so it is compared through the
            // settled read below rather than between the two envelopes.
            if member == "status" {
                continue;
            }
            assert_eq!(
                read.body.get(member),
                updated.body.get(member),
                "case {:?}: the read reports the {member} the change did, got {}",
                row.case,
                read.body
            );
        }

        // The clock, not the call, moves the policy back to `ACTIVE`, and what the change
        // applied is still there when it does.
        let settled = poll_until_active(&router, &policy_engine_id, &policy_id, row.case).await;
        assert_eq!(
            settled
                .body
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            expected_description,
            "case {:?}: the settled policy still holds the description the change applied, \
             got {}",
            row.case,
            settled.body
        );
        assert_eq!(
            settled.body.get("definition"),
            Some(&row.expected_definition()),
            "case {:?}: the settled policy still holds the definition the change applied, \
             got {}",
            row.case,
            settled.body
        );
        assert_eq!(
            settled.body.get("updatedAt"),
            updated.body.get("updatedAt"),
            "case {:?}: settling is not a modification, so updatedAt is the change's, got {}",
            row.case,
            settled.body
        );
    }
}

/// `UpdatePolicy` is `@idempotent`: the same change applied again to the settled policy
/// leaves it holding exactly what the first call stored.
#[tokio::test]
async fn the_same_change_applied_again_stores_the_same_policy() {
    let changes = [
        (
            "a definition and a mode",
            json!({"definition": dogwood_definition("search"), "enforcementMode": "LOG_ONLY"}),
        ),
        (
            "a replacement description",
            json!({"description": {"optionalValue": "repeated"}}),
        ),
        ("the empty wrapper", json!({"description": {}})),
        ("an empty body", json!({})),
    ];
    for (case, change) in changes {
        let router = router();
        let (policy_engine_id, policy_id) = active_policy(
            &router,
            "Repeated_engine",
            &json!({
                "name": "Repeated_change",
                "definition": cedar_definition("invokeTool"),
                "description": "as created",
            }),
            case,
        )
        .await;

        let first = update_policy(&router, &policy_engine_id, &policy_id, &change).await;
        assert_eq!(
            first.status, 202,
            "case {case:?}: the first change is accepted, got {} ({})",
            first.status, first.body
        );
        poll_until_active(&router, &policy_engine_id, &policy_id, case).await;

        let second = update_policy(&router, &policy_engine_id, &policy_id, &change).await;
        assert_eq!(
            second.status, 202,
            "case {case:?}: the repeat is accepted once the policy is ACTIVE again, got {} ({})",
            second.status, second.body
        );
        for member in [
            "policyId",
            "name",
            "policyEngineId",
            "policyArn",
            "createdAt",
            "definition",
            "description",
            "enforcementMode",
        ] {
            assert_eq!(
                second.body.get(member),
                first.body.get(member),
                "case {case:?}: the repeat reports the {member} the first call did, got {}",
                second.body
            );
        }
    }
}

/// A policy whose own asynchronous work is still under way refuses a further change the
/// modelled conflict, in this plane's own sentence, and keeps what it held — so the refused
/// call changed nothing.
#[tokio::test]
async fn a_policy_that_is_not_active_refuses_a_further_change() {
    // The changes a refusal is checked for: one carrying every modifiable member, one
    // carrying only a description, and an empty one.
    let changes = [
        (
            "every modifiable member",
            json!({
                "definition": dogwood_definition("never_applied"),
                "enforcementMode": "LOG_ONLY",
                "description": {"optionalValue": "never applied"},
            }),
        ),
        (
            "only a description",
            json!({"description": {"optionalValue": "never applied"}}),
        ),
        ("an empty body", json!({})),
    ];

    for (case, change) in &changes {
        // While the CREATE is still under way: the policy is `CREATING`, so a caller acting
        // straight after the create meets the conflict.
        let router = router();
        let created_engine =
            create_policy_engine(&router, &json!({"name": "Not_active_engine"})).await;
        let policy_engine_id = text(&created_engine.body, "policyEngineId");
        let created = create_policy(
            &router,
            &policy_engine_id,
            &json!({
                "name": "Still_creating",
                "definition": cedar_definition("invokeTool"),
                "description": "as created",
            }),
        )
        .await;
        let policy_id = text(&created.body, "policyId");
        assert_eq!(
            text(&created.body, "status"),
            "CREATING",
            "case {case:?}: the create reports CREATING, got {}",
            created.body
        );
        let refused = update_policy(&router, &policy_engine_id, &policy_id, change).await;
        assert_eq!(
            (
                refused.status,
                error_code(&refused.body),
                text(&refused.body, "message")
            ),
            (
                409,
                "ConflictException".to_string(),
                policy_concurrent_modification_message(&policy_id)
            ),
            "case {case:?}/CREATING: expected HTTP 409 ConflictException naming the policy \
             the path carried and what to wait for, got {} ({})",
            refused.status,
            refused.body
        );
        let unchanged = get_policy(&router, &policy_engine_id, &policy_id).await;
        assert_eq!(
            (
                unchanged.status,
                text(&unchanged.body, "description"),
                unchanged.body.get("definition").cloned(),
            ),
            (
                200,
                "as created".to_string(),
                Some(cedar_definition("invokeTool")),
            ),
            "case {case:?}/CREATING: the refused change changed nothing, got {} ({})",
            unchanged.status,
            unchanged.body
        );

        // And while an accepted UPDATE is still under way: the policy is `UPDATING`, which
        // is the concurrent change the sentence is about.
        let (settled_engine_id, settled_policy_id) = active_policy(
            &router,
            "Still_updating_engine",
            &json!({
                "name": "Still_updating",
                "definition": cedar_definition("invokeTool"),
                "description": "as created",
            }),
            case,
        )
        .await;
        let accepted = update_policy(
            &router,
            &settled_engine_id,
            &settled_policy_id,
            &json!({"description": {"optionalValue": "the change that won"}}),
        )
        .await;
        assert_eq!(
            (accepted.status, text(&accepted.body, "status")),
            (202, "UPDATING".to_string()),
            "case {case:?}: the first change is accepted and leaves the policy UPDATING, got \
             {} ({})",
            accepted.status,
            accepted.body
        );
        let second = update_policy(&router, &settled_engine_id, &settled_policy_id, change).await;
        assert_eq!(
            (
                second.status,
                error_code(&second.body),
                text(&second.body, "message")
            ),
            (
                409,
                "ConflictException".to_string(),
                policy_concurrent_modification_message(&settled_policy_id)
            ),
            "case {case:?}/UPDATING: a concurrent change is the modelled conflict, got {} ({})",
            second.status,
            second.body
        );
        let won =
            poll_until_active(&router, &settled_engine_id, &settled_policy_id, case).await;
        assert_eq!(
            text(&won.body, "description"),
            "the change that won",
            "case {case:?}: the refused change left the accepted one's description in place, \
             got {}",
            won.body
        );
    }
}

/// An engine or a policy id this account does not hold refuses the change with the modelled
/// 404 and the plane's own sentence — and brings no resource into being by being written to.
#[tokio::test]
async fn an_unknown_engine_or_policy_is_refused() {
    let router = router();
    // Every id satisfies `ResourceId`'s `^[A-Za-z][A-Za-z0-9_]*-[a-z0-9_]{10}$`, so the
    // refusal is the handler's lookup and not the boundary constraint walk: the shortest id
    // the pattern admits, one carrying underscores, one at the top of the 12..59 length
    // range, and one whose suffix is all underscores.
    let unknown = [
        "A-0123456789",
        "Missing_resource-abcdefghij",
        "Engine_with_a_long_but_valid_name_for_account-zz00zz00zz",
        "Underscores-__________",
    ];
    let changes = [
        (
            "a definition",
            json!({"definition": cedar_definition("invokeTool")}),
        ),
        (
            "a description",
            json!({"description": {"optionalValue": "anything"}}),
        ),
        ("an empty body", json!({})),
    ];

    // An id naming no engine of this account: the engine is reported missing in its own
    // sentence, before the policy is looked at.
    for policy_engine_id in unknown {
        for (case, change) in &changes {
            let refused =
                update_policy(&router, policy_engine_id, "AnyPolicy-0123456789", change).await;
            assert_eq!(
                (
                    refused.status,
                    error_code(&refused.body),
                    text(&refused.body, "message")
                ),
                (
                    404,
                    "ResourceNotFoundException".to_string(),
                    engine_not_found_message(policy_engine_id)
                ),
                "case {policy_engine_id:?}/{case}: expected HTTP 404 \
                 ResourceNotFoundException naming the engine the path carried, got {} ({})",
                refused.status,
                refused.body
            );
            assert!(
                refused.body.get("policyArn").is_none(),
                "case {policy_engine_id:?}/{case}: a refused change answers no success \
                 envelope, got {}",
                refused.body
            );
        }
    }

    // And an id naming no policy of an engine that DOES exist: the policy is reported
    // missing in its own sentence.
    let created_engine = create_policy_engine(&router, &json!({"name": "Existing_engine"})).await;
    let policy_engine_id = text(&created_engine.body, "policyEngineId");
    for policy_id in unknown {
        for (case, change) in &changes {
            let refused = update_policy(&router, &policy_engine_id, policy_id, change).await;
            assert_eq!(
                (
                    refused.status,
                    error_code(&refused.body),
                    text(&refused.body, "message")
                ),
                (
                    404,
                    "ResourceNotFoundException".to_string(),
                    policy_not_found_message(policy_id)
                ),
                "case {policy_id:?}/{case}: expected HTTP 404 ResourceNotFoundException \
                 naming the policy the path carried, got {} ({})",
                refused.status,
                refused.body
            );
            let read = get_policy(&router, &policy_engine_id, policy_id).await;
            assert_eq!(
                (read.status, text(&read.body, "message")),
                (404, policy_not_found_message(policy_id)),
                "case {policy_id:?}/{case}: the refused change left no policy behind, got {} \
                 ({})",
                read.status,
                read.body
            );
        }
    }
}
