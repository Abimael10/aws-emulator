// SPDX-License-Identifier: CC0-1.0

//! What this service states when an identifier names no resource.
//!
//! RULE under test: a call that resolves an identifier against an entity store and finds
//! nothing answers the modelled `ResourceNotFoundException` (HTTP 404, that
//! `x-amzn-errortype`) whose `message` names the entity's `ServiceResourceType` followed by
//! `" not found: "` and the id the identifier carries — the resource segment of the ARN the
//! model documents an identifier to be, with a version suffix dropped — and whose
//! `resourceId` / `resourceType` members report that same id and type. The only variable
//! part of the sentence comes from the request.
//!
//! The rule belongs to the refusal, not to one operation: the rows below span every value
//! of `ServiceResourceType` (`smithy_query(query="shape", shape="ServiceResourceType")` —
//! Rule, Template, Policy, Deployment, Scope), each read's identifier in the forms a
//! caller may spell it, the update that resolves the same identifier, and a template the
//! lifecycle actually removed.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a client sees.

use std::sync::Arc;

use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

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
        self.error_code.as_deref().map(|code| {
            code.split(['#', ':'])
                .next_back()
                .unwrap_or(code)
                .to_string()
        })
    }

    fn string_at(&self, member: &str) -> String {
        self.json[member]
            .as_str()
            .unwrap_or_else(|| panic!("{member} is required, got {:?}", self.json))
            .to_string()
    }
}

/// The emulator under test: this one service mounted on the production router.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The wire behaviour under test is the operations', not SigV4's.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router }
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

    /// Read one entity by `identifier`, with the identifier percent-encoded the way a
    /// client's URI builder carries an ARN in a `@httpLabel`.
    async fn read(&self, collection: &str, identifier: &str) -> Reply {
        let encoded = identifier.replace(':', "%3A").replace('/', "%2F");
        self.call("GET", &format!("/{collection}/{encoded}"), None)
            .await
    }
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

/// Assert `reply` is the modelled refusal for an absent `resource_type` whose id is
/// `resource_id`.
fn assert_not_found(case: &str, resource_type: &str, resource_id: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 404,
        "case {case:?}: expected 404, got {} {:?}",
        reply.status, reply.json
    );
    assert_eq!(
        reply.code().as_deref(),
        Some("ResourceNotFoundException"),
        "case {case:?}: expected the ResourceNotFoundException error code, got {:?}",
        reply.error_code
    );
    assert_eq!(
        reply.json["message"].as_str(),
        Some(format!("{resource_type} not found: {resource_id}").as_str()),
        "case {case:?}: expected the message {:?}, got {:?}",
        format!("{resource_type} not found: {resource_id}"),
        reply.json["message"]
    );
    assert_eq!(
        reply.json["resourceId"].as_str(),
        Some(resource_id),
        "case {case:?}: expected resourceId {resource_id:?}, got {:?}",
        reply.json["resourceId"]
    );
    assert_eq!(
        reply.json["resourceType"].as_str(),
        Some(resource_type),
        "case {case:?}: expected resourceType {resource_type:?}, got {:?}",
        reply.json["resourceType"]
    );
}

/// One entity of the service: its `ServiceResourceType`, the collection its reads hang
/// off, and the ARN resource segment its identifiers carry.
struct Entity {
    resource_type: &'static str,
    collection: &'static str,
    arn_segment: &'static str,
    /// Whether the read resolves a bare id as well as the ARN. A read documented to take
    /// the resource's ARN refuses any other form as a field-validation failure before the
    /// store is consulted (pinned by `rule_identifier_form.rs`), so a bare id is not a
    /// miss there.
    resolves_bare_id: bool,
}

/// Every value of the modelled `ServiceResourceType` enum, in declaration order.
fn entities() -> Vec<Entity> {
    vec![
        Entity {
            resource_type: "Rule",
            collection: "rules",
            arn_segment: "rule",
            resolves_bare_id: false,
        },
        Entity {
            resource_type: "Template",
            collection: "templates",
            arn_segment: "template",
            resolves_bare_id: true,
        },
        Entity {
            resource_type: "Policy",
            collection: "policies",
            arn_segment: "policy",
            resolves_bare_id: true,
        },
        Entity {
            resource_type: "Deployment",
            collection: "deployments",
            arn_segment: "deployment",
            resolves_bare_id: true,
        },
        Entity {
            resource_type: "Scope",
            collection: "scopes",
            arn_segment: "scope",
            resolves_bare_id: true,
        },
    ]
}

/// RULE: each entity's read refuses an identifier naming nothing with its own
/// `ServiceResourceType` and the id the identifier carries, whatever form that identifier
/// takes.
#[tokio::test]
async fn every_entity_reads_states_its_own_type_and_the_missing_id() {
    let emulator = Emulator::new();

    for entity in entities() {
        // The rows differ along the FORM of the identifier: an ARN nothing minted, that
        // same ARN qualified with an `EntityVersion` suffix, an ARN of ANOTHER resource
        // type (so the id read out is that other resource's), and — where the read admits
        // one — a bare id and the one-character minimum of the identifier's
        // `@length(1..1010)`.
        let missing_id = format!("nevermint{}1", entity.arn_segment);
        let other_segment = if entity.arn_segment == "rule" {
            "template"
        } else {
            "rule"
        };
        let mut rows: Vec<(String, String, String)> = vec![
            (
                format!("{} by an ARN nothing minted", entity.resource_type),
                resource_arn(entity.arn_segment, &missing_id),
                missing_id.clone(),
            ),
            (
                format!("{} by a version-qualified ARN", entity.resource_type),
                format!("{}:2", resource_arn(entity.arn_segment, &missing_id)),
                missing_id.clone(),
            ),
            (
                format!("{} by another type's ARN", entity.resource_type),
                resource_arn(other_segment, "crosstypeid1"),
                "crosstypeid1".to_string(),
            ),
        ];
        if entity.resolves_bare_id {
            rows.push((
                format!("{} by a bare id nothing minted", entity.resource_type),
                missing_id.clone(),
                missing_id.clone(),
            ));
            rows.push((
                format!("{} by the shortest identifier the model admits", entity.resource_type),
                "x".to_string(),
                "x".to_string(),
            ));
        }

        for (case, identifier, expected_id) in rows {
            let reply = emulator.read(entity.collection, &identifier).await;
            assert_not_found(&case, entity.resource_type, &expected_id, &reply);
        }
    }
}

/// RULE: the refusal is owed by whatever resolves the identifier, not only by the read —
/// the policy update states the same sentence for a policy that is absent, while a policy
/// that IS stored is answered rather than refused.
#[tokio::test]
async fn an_update_states_the_same_refusal_as_the_read() {
    let emulator = Emulator::new();

    let created = emulator
        .call(
            "POST",
            "/policies",
            Some(serde_json::json!({
                "policyName": "not-found-wording-policy",
                "priority": 1,
                "firewallType": "SHIELD_ADVANCED",
                "policyConfiguration": {
                    "remediationEnabled": true,
                    "resourcesCleanUp": true,
                },
            })),
        )
        .await;
    assert_eq!(
        created.status, 201,
        "case the stored policy: expected 201, got {} {:?}",
        created.status, created.json
    );
    let policy_id = created.string_at("policyId");
    let policy_arn = created.string_at("policyArn");

    // The stored policy is answered, so the refusal below is about absence.
    let read = emulator.read("policies", &policy_arn).await;
    assert_eq!(
        read.status, 200,
        "case the stored policy read back: expected 200, got {} {:?}",
        read.status, read.json
    );

    let missing = format!("{policy_id}zz");
    let refused = emulator
        .call(
            "PATCH",
            &format!("/policies/{missing}"),
            Some(serde_json::json!({
                "updateToken": created.string_at("updateToken"),
                "isPublished": true,
                "priority": 2,
                "policyConfiguration": {
                    "remediationEnabled": true,
                    "resourcesCleanUp": true,
                },
            })),
        )
        .await;
    assert_not_found(
        "UpdatePolicy on an absent policy",
        "Policy",
        &missing,
        &refused,
    );
}

/// RULE: once the lifecycle removes a template, its own identifier names nothing — and the
/// refusal names the template id that identifier carries, in either spelling.
#[tokio::test]
async fn a_removed_template_is_refused_by_its_own_identifier() {
    let emulator = Emulator::new();

    let rule = emulator
        .call(
            "POST",
            "/rules",
            Some(serde_json::json!({
                "ruleName": "not-found-wording-rule",
                "firewallType": "WAF",
                "ruleType": "CONFIGURATION",
                "configuration": { "DefaultAction": "ALLOW" },
            })),
        )
        .await;
    assert_eq!(
        rule.status, 201,
        "case the associated rule: expected 201, got {} {:?}",
        rule.status, rule.json
    );

    let template = emulator
        .call(
            "POST",
            "/templates",
            Some(serde_json::json!({
                "templateName": "not-found-wording-template",
                "firewallType": "WAF",
                "associatedRuleList": [{ "ruleIdentifier": rule.string_at("ruleArn") }],
            })),
        )
        .await;
    assert_eq!(
        template.status, 201,
        "case the created template: expected 201, got {} {:?}",
        template.status, template.json
    );
    let template_id = template.string_at("templateId");
    let template_arn = template.string_at("templateArn");

    let removed = emulator
        .call(
            "DELETE",
            &format!("/templates/{}", template_arn.replace(':', "%3A").replace('/', "%2F")),
            None,
        )
        .await;
    assert_eq!(
        removed.status, 204,
        "case removing the template: expected 204, got {} {:?}",
        removed.status, removed.json
    );

    // Both spellings of the removed template's identifier, and the version-qualified ARN,
    // name the same template id — so each refusal states it.
    for (case, identifier) in [
        ("the removed template by its ARN", template_arn.clone()),
        ("the removed template by its bare id", template_id.clone()),
        (
            "the removed template by its version-qualified ARN",
            format!("{template_arn}:1"),
        ),
    ] {
        let reply = emulator.read("templates", &identifier).await;
        assert_not_found(case, "Template", &template_id, &reply);
    }
}
