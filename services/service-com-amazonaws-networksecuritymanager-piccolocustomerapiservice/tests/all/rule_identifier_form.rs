// SPDX-License-Identifier: CC0-1.0

//! The FORM a rule read or delete admits for its `ruleIdentifier`.
//!
//! RULE under test: `ruleIdentifier` is documented "The identifier of the rule. This is the
//! rule's Amazon Resource Name (ARN)"
//! (`smithy_query(query="shape",shape="GetRuleInput,DeleteRuleInput")`), so the admissible
//! form of the value is an ARN. A value of any other form is a field of the request that
//! failed validation, refused with the modelled `ValidationException` — HTTP 400,
//! `x-amzn-errortype` naming the shape, `reason` `FIELD_VALIDATION_FAILED` and the message
//! `<member> must be the resource's ARN.` — and refused BEFORE the store is consulted, so
//! neither a hit (200 / 204) nor a miss (the 404 of the read, the `@idempotent` 204 of the
//! delete) can be the answer to it.
//!
//! The rule is about the FORM alone: an ARN that names nothing still resolves as a lookup,
//! and so is answered by the miss path of each operation, not by this refusal.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a client sees.

use std::sync::Arc;

use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::EmulatedService;
use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::state::service_state;
use smithy_server_runtime::MultiServiceRouter;

/// The member a rule identifier arrives in, as the model spells it.
const MEMBER: &str = "ruleIdentifier";

/// The sentence this error class states: fixed text naming the offending member.
fn arn_required_message(member: &str) -> String {
    format!("{member} must be the resource's ARN.")
}

/// One reply, reduced to what a caller branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    /// The restJson1 error code (`x-amzn-errortype`), absent on success.
    error_code: Option<String>,
    json: serde_json::Value,
}

impl Reply {
    /// The modelled error code, with any shape-id or version decoration the protocol
    /// allows around it stripped.
    fn code(&self) -> Option<String> {
        self.error_code
            .as_deref()
            .map(|code| code.split(['#', ':']).next_back().unwrap_or(code).to_string())
    }

    fn text_at(&self, member: &str) -> Option<&str> {
        self.json[member].as_str()
    }
}

/// The emulator under test: this one service mounted on the production router.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The wire behaviour under test is the operations', not SigV4's.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// How many rules the store holds, so a refusal can be shown to have removed nothing.
    fn rules_len(&self) -> usize {
        service_state(&*self.engine)
            .expect("the service state is reachable from the shared engine")
            .lock()
            .rules
            .len()
    }

    async fn call(&self, method: &str, path: &str, body: Option<serde_json::Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "network-security-manager.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261021/us-east-1/\
                 network-security-manager/aws4_request, SignedHeaders=host, Signature=0",
            )
            .body(payload)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let raw = response.into_body();
        let json = if raw.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null)
        };
        Reply {
            status,
            error_code,
            json,
        }
    }

    async fn create_rule(&self, name: &str) -> Reply {
        self.call(
            "POST",
            "/rules",
            Some(serde_json::json!({
                "ruleName": name,
                // `RuleFirewallType` models exactly one value, WAF.
                "firewallType": "WAF",
                "ruleType": "CONFIGURATION",
                "configuration": {"DefaultAction": {"Allow": {}}},
                "isPublished": true,
            })),
        )
        .await
    }

    async fn get_rule(&self, identifier: &str) -> Reply {
        self.call("GET", &format!("/rules/{}", path_label(identifier)), None)
            .await
    }

    async fn delete_rule(&self, identifier: &str) -> Reply {
        self.call("DELETE", &format!("/rules/{}", path_label(identifier)), None)
            .await
    }
}

/// An identifier is a NON-greedy `@httpLabel`, so one containing `/` — a resource ARN —
/// reaches the server percent-encoded, exactly as an AWS SDK sends it.
fn path_label(identifier: &str) -> String {
    identifier.replace('/', "%2F")
}

fn string_at(value: &serde_json::Value, member: &str) -> String {
    value[member]
        .as_str()
        .unwrap_or_else(|| panic!("{member} is required, got {value:?}"))
        .to_string()
}

/// An ARN of one of this service's resource types, built from the same helper the handlers
/// mint ARNs with rather than from a hardcoded literal.
fn resource_arn(resource_type: &str, id: &str) -> String {
    engine::emulator_arn(
        "network-security-manager",
        "us-east-1",
        &format!("{resource_type}/{id}"),
    )
}

/// Assert `reply` is the identifier-form refusal, whole: status, error code, reason and
/// message.
fn assert_arn_required(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 400,
        "case {case}: expected 400, got {} {:?}",
        reply.status, reply.json
    );
    assert_eq!(
        reply.code(),
        Some("ValidationException".to_string()),
        "case {case}: expected the modelled error code, got {:?}",
        reply.error_code
    );
    assert_eq!(
        reply.text_at("reason"),
        Some("FIELD_VALIDATION_FAILED"),
        "case {case}: expected the modelled reason, got {:?}",
        reply.json["reason"]
    );
    assert_eq!(
        reply.text_at("message"),
        Some(arn_required_message(MEMBER).as_str()),
        "case {case}: unexpected message, got {:?}",
        reply.json["message"]
    );
    // This refusal states the message and the reason, and nothing else: `fieldList` names
    // the fields of a request whose VALUES failed a constraint.
    assert!(
        reply.json["fieldList"].is_null(),
        "case {case}: this refusal states no fieldList, got {:?}",
        reply.json["fieldList"]
    );
}

/// The identifier forms that are NOT an ARN, each paired with the case it names.
///
/// Rows differ along the FORM, not the value: the rule's own id (the one spelling closest
/// to resolving), another rule's id, the rule's name, a single character (the shortest
/// value `RuleIdentifier`'s `@length(1..1010)` admits), a value as long as an ARN but
/// without the ARN grammar, an ARN missing its resource remainder, one whose literal
/// prefix is wrong, one whose partition segment is empty, and one spelled in upper case
/// (the ARN `@pattern` admits lowercase segments only).
fn non_arn_forms(rule_id: &str, rule_name: &str, rule_arn: &str) -> Vec<(String, String)> {
    vec![
        ("the rule's own bare id".to_string(), rule_id.to_string()),
        (
            "a bare id no rule holds".to_string(),
            "0123456789abcdef".to_string(),
        ),
        ("the rule's name".to_string(), rule_name.to_string()),
        (
            "the shortest value the member's @length admits".to_string(),
            "x".to_string(),
        ),
        (
            "a value of ARN length that is no ARN".to_string(),
            "x".repeat(rule_arn.len()),
        ),
        (
            "an ARN with no resource remainder".to_string(),
            "arn:aws:network-security-manager:us-east-1:123456789012:".to_string(),
        ),
        (
            "a value whose first segment is not arn".to_string(),
            rule_arn.replacen("arn:", "urn:", 1),
        ),
        (
            "an ARN whose partition segment is empty".to_string(),
            rule_arn.replacen("arn:aws:", "arn::", 1),
        ),
        (
            "the ARN spelled in upper case".to_string(),
            rule_arn.to_uppercase(),
        ),
    ]
}

/// RULE: `GetRule` refuses every `ruleIdentifier` that is not an ARN with the modelled
/// `ValidationException`, and answers an ARN — one that resolves and one that does not —
/// from the store.
#[tokio::test]
async fn get_rule_admits_an_arn_and_refuses_every_other_identifier_form() {
    let emulator = Emulator::new();
    let created = emulator.create_rule("identifier-form-rule").await;
    assert_eq!(
        created.status, 201,
        "creating the rule expected 201, got {} {:?}",
        created.status, created.json
    );
    let rule_id = string_at(&created.json, "ruleId");
    let rule_arn = string_at(&created.json, "ruleArn");

    // The documented form resolves: the read answers exactly what the create answered.
    let read = emulator.get_rule(&rule_arn).await;
    assert_eq!(
        read.status, 200,
        "reading the rule by its ARN expected 200, got {} {:?}",
        read.status, read.json
    );
    assert_eq!(
        read.json, created.json,
        "reading the rule by its ARN must answer the created record"
    );

    for (case, identifier) in non_arn_forms(&rule_id, "identifier-form-rule", &rule_arn) {
        let reply = emulator.get_rule(&identifier).await;
        assert_arn_required(&format!("GetRule, {case} ({identifier:?})"), &reply);
    }

    // An ARN is a form the lookup decides: these name nothing, so each is the read's own
    // miss — the 404 — and never the refusal above.
    for (case, identifier) in [
        (
            "an ARN of this service naming no rule",
            resource_arn("rule", "0123456789abcdef"),
        ),
        (
            "the stored id inside another resource type's ARN",
            resource_arn("policy", &rule_id),
        ),
        (
            "an ARN of another service entirely",
            engine::emulator_arn("other-service", "us-east-1", "rule/somerule"),
        ),
        (
            "the rule's ARN with a version suffix",
            format!("{rule_arn}:1"),
        ),
    ] {
        let reply = emulator.get_rule(&identifier).await;
        assert_eq!(
            reply.status, 404,
            "case {case} ({identifier:?}): expected 404, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            reply.code(),
            Some("ResourceNotFoundException".to_string()),
            "case {case} ({identifier:?}): expected the modelled error code, got {:?}",
            reply.error_code
        );
    }

    // Nothing above changed the store: the rule is still there, and still readable.
    assert_eq!(
        emulator.rules_len(),
        1,
        "the refusals and misses must leave the one rule behind, store holds {}",
        emulator.rules_len()
    );
    assert_eq!(
        emulator.get_rule(&rule_arn).await.json,
        created.json,
        "the stored rule must be unchanged"
    );
}

/// RULE: `DeleteRule` refuses every `ruleIdentifier` that is not an ARN — the
/// `@idempotent` 204 belongs to a well-formed identifier that names nothing, not to a
/// malformed one — and the refusal removes nothing.
#[tokio::test]
async fn delete_rule_refuses_every_identifier_form_that_is_not_an_arn() {
    let emulator = Emulator::new();
    let created = emulator.create_rule("identifier-form-doomed").await;
    assert_eq!(
        created.status, 201,
        "creating the rule expected 201, got {} {:?}",
        created.status, created.json
    );
    let rule_id = string_at(&created.json, "ruleId");
    let rule_arn = string_at(&created.json, "ruleArn");

    for (case, identifier) in non_arn_forms(&rule_id, "identifier-form-doomed", &rule_arn) {
        let reply = emulator.delete_rule(&identifier).await;
        assert_arn_required(&format!("DeleteRule, {case} ({identifier:?})"), &reply);
        assert_eq!(
            emulator.rules_len(),
            1,
            "case {case} ({identifier:?}): a refused delete must remove nothing, store \
             holds {}",
            emulator.rules_len()
        );
    }

    // A well-formed identifier naming no rule IS the `@idempotent` success.
    for (case, identifier) in [
        (
            "an ARN of this service naming no rule",
            resource_arn("rule", "0123456789abcdef"),
        ),
        (
            "the stored id inside another resource type's ARN",
            resource_arn("policy", &rule_id),
        ),
    ] {
        let reply = emulator.delete_rule(&identifier).await;
        assert_eq!(
            reply.status, 204,
            "case {case} ({identifier:?}): expected 204, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            emulator.rules_len(),
            1,
            "case {case} ({identifier:?}): nothing may be removed, store holds {}",
            emulator.rules_len()
        );
    }

    // And the documented form removes the rule it names.
    let deleted = emulator.delete_rule(&rule_arn).await;
    assert_eq!(
        deleted.status, 204,
        "deleting the rule by its ARN expected 204, got {} {:?}",
        deleted.status, deleted.json
    );
    assert_eq!(
        emulator.rules_len(),
        0,
        "the delete must remove the rule it named, store holds {}",
        emulator.rules_len()
    );
    // The read path agrees, and still refuses the bare id on its FORM rather than
    // reporting the rule it no longer holds.
    let gone = emulator.get_rule(&rule_arn).await;
    assert_eq!(
        gone.status, 404,
        "the removed rule must be gone from the read path, got {} {:?}",
        gone.status, gone.json
    );
    assert_arn_required(
        &format!("GetRule after the delete, the bare id ({rule_id:?})"),
        &emulator.get_rule(&rule_id).await,
    );
}
