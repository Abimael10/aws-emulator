// SPDX-License-Identifier: CC0-1.0

//! The Template WRITE path, end to end over HTTP: what `CreateTemplate` persists and what
//! it refuses.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a client sees, not what a handler
//! returns.
//!
//! RULE under test: a create echoes the members the request carried, derives the rest from
//! them (the status from `isPublished`, the first `EntityVersion`, a live record rather
//! than a snapshot, a modelled `updateToken`) and persists exactly ONE record per distinct
//! `@idempotencyToken`, which a read answers with; and a request whose reference member
//! carries an identifier the service could not answer a read with, or whose reference list
//! falls outside its modelled bound, is refused with the modelled `ValidationException`
//! (HTTP 400) having persisted nothing.
//!
//! The read-back path (`GetTemplate`, `ListTemplates`) has its own suite in
//! `template_read_path`; here a read is only the proof that a create persisted.

use std::sync::Arc;

use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// What a caller sees: the HTTP facts it branches on plus the JSON body.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    json: serde_json::Value,
}

impl Reply {
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

    async fn create_template(&self, body: serde_json::Value) -> Reply {
        self.call("POST", "/templates", Some(body)).await
    }

    async fn get_template(&self, identifier: &str) -> Reply {
        self.call(
            "GET",
            &format!("/templates/{}", identifier.replace('/', "%2F")),
            None,
        )
        .await
    }

    /// Every `templateId` the store holds, sorted, read through `ListTemplates`.
    async fn stored_template_ids(&self) -> Vec<String> {
        let reply = self.call("GET", "/templates", None).await;
        assert_eq!(
            reply.status, 200,
            "listing the store: expected 200, got {} {:?}",
            reply.status, reply.json
        );
        let mut ids: Vec<String> = reply.json["templates"]
            .as_array()
            .unwrap_or_else(|| panic!("listing the store: templates is required, got {:?}", reply.json))
            .iter()
            .map(|summary| {
                summary["templateId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("listing the store: templateId is required, got {summary:?}"))
                    .to_string()
            })
            .collect();
        ids.sort();
        ids
    }
}

/// An ARN of one of this service's resource types, built from the same helper the handlers
/// mint ARNs with rather than from a hardcoded literal.
fn rule_arn(id: &str) -> String {
    engine::emulator_arn("network-security-manager", "us-east-1", &format!("rule/{id}"))
}

/// `rules` distinct rule references, as a `CreateTemplate` body carries them.
fn rule_references(rules: usize) -> serde_json::Value {
    serde_json::Value::Array(
        (0..rules)
            .map(|index| serde_json::json!({ "ruleIdentifier": rule_arn(&format!("createrule{index}")) }))
            .collect(),
    )
}

/// `UpdateToken`'s modelled form, `^([0-9a-f]{8})-([0-9a-f]{4}-){3}([0-9a-f]{12})$`
/// (`smithy_query(query="shape", shape="UpdateToken")`), checked without a regex crate.
fn is_modelled_update_token(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && [8_usize, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(wanted, group)| {
                group.len() == *wanted && group.chars().all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
            })
}

/// RULE: a create echoes what it was given and derives the rest, and the record it answers
/// with is the one a read returns.
#[tokio::test]
async fn a_create_echoes_its_members_and_derives_the_rest() {
    let emulator = Emulator::new();

    /// One row: the case, the request body, and the `EntityStatus` the request's
    /// `isPublished` implies.
    struct Row {
        label: &'static str,
        body: serde_json::Value,
        status: &'static str,
    }

    // The rows differ along publish state (absent — `isPublished` is modelled
    // `@default(true)` — and both values), whether a `templateDescription` is carried, and
    // how many rules are associated: both published bounds of `RuleReferenceList`,
    // `@length(min: 1, max: 50)`
    // (`smithy_query(query="shape", shape="RuleReferenceList")`).
    let rows = vec![
        Row {
            label: "published by default, one rule, no description",
            body: serde_json::json!({
                "templateName": "defaulted-template",
                "firewallType": "WAF",
                "associatedRuleList": rule_references(1),
            }),
            status: "ACTIVE",
        },
        Row {
            label: "explicitly published, described, two rules",
            body: serde_json::json!({
                "templateName": "Published.Template_1",
                "templateDescription": "edge rules for the shared account",
                "firewallType": "WAF",
                "isPublished": true,
                "associatedRuleList": rule_references(2),
            }),
            status: "ACTIVE",
        },
        Row {
            label: "a draft with tags",
            body: serde_json::json!({
                "templateName": "draft template",
                "templateDescription": "",
                "firewallType": "WAF",
                "isPublished": false,
                "tags": { "owner": "net-sec", "tier": "edge" },
                "associatedRuleList": rule_references(3),
            }),
            status: "DRAFT",
        },
        Row {
            label: "the largest reference list the model admits",
            body: serde_json::json!({
                "templateName": "fifty-rule-template",
                "firewallType": "WAF",
                "associatedRuleList": rule_references(50),
            }),
            status: "ACTIVE",
        },
    ];

    let mut minted: Vec<String> = Vec::new();
    for row in rows {
        let label = row.label;
        let created = emulator.create_template(row.body.clone()).await;
        assert_eq!(
            created.status, 201,
            "case {label}: expected 201, got {} {:?}",
            created.status, created.json
        );

        let template_id = created.string_at("templateId");
        assert!(
            !minted.contains(&template_id),
            "case {label}: templateId {template_id:?} was already handed out"
        );
        minted.push(template_id.clone());
        assert!(
            template_id
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit()),
            "case {label}: TemplateId is modelled ^[a-z0-9]{{1,64}}$, got {template_id:?}"
        );
        assert_eq!(
            created.string_at("templateArn"),
            engine::emulator_arn(
                "network-security-manager",
                "us-east-1",
                &format!("template/{template_id}")
            ),
            "case {label}: unexpected templateArn"
        );

        // Echoed straight from the request.
        for member in ["templateName", "firewallType"] {
            assert_eq!(
                created.json[member], row.body[member],
                "case {label}: the answer must echo the submitted {member}, got {:?}",
                created.json[member]
            );
        }
        assert_eq!(
            created.json["templateDescription"],
            row.body
                .get("templateDescription")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            "case {label}: unexpected templateDescription"
        );
        // The reference list becomes the association list, entry for entry and in order:
        // the model documents `ruleIdentifier` as "the rule's Amazon Resource Name (ARN)",
        // so the association carries that ARN.
        let wanted_rules: Vec<serde_json::Value> = row.body["associatedRuleList"]
            .as_array()
            .expect("the row carries a reference list")
            .iter()
            .map(|reference| serde_json::json!({ "ruleArn": reference["ruleIdentifier"] }))
            .collect();
        assert_eq!(
            created.json["associatedRuleList"],
            serde_json::Value::Array(wanted_rules),
            "case {label}: unexpected associatedRuleList, got {:?}",
            created.json["associatedRuleList"]
        );

        // Derived at create time.
        assert_eq!(
            created.json["status"].as_str(),
            Some(row.status),
            "case {label}: expected status {:?}, got {:?}",
            row.status,
            created.json["status"]
        );
        assert_eq!(
            created.json["version"].as_str(),
            Some("1"),
            "case {label}: a first create is version 1, got {:?}",
            created.json["version"]
        );
        assert_eq!(
            created.json["isSnapshot"].as_bool(),
            Some(false),
            "case {label}: a created template is the live record, not a snapshot, got {:?}",
            created.json["isSnapshot"]
        );
        assert_eq!(
            created.json["hasPublishedVersion"].as_bool(),
            Some(row.status == "ACTIVE"),
            "case {label}: unexpected hasPublishedVersion, got {:?}",
            created.json["hasPublishedVersion"]
        );
        let update_token = created.string_at("updateToken");
        assert!(
            is_modelled_update_token(&update_token),
            "case {label}: expected a modelled updateToken, got {update_token:?}"
        );
        assert!(
            created.json["updatedAt"]
                .as_str()
                .is_some_and(|value| value.contains('T') && value.ends_with('Z')),
            "case {label}: expected a date-time updatedAt, got {:?}",
            created.json["updatedAt"]
        );

        // The read is what proves the create PERSISTED: it answers with the record the
        // create returned, member for member.
        let read = emulator.get_template(&template_id).await;
        assert_eq!(
            read.status, 200,
            "case {label}: reading it back expected 200, got {} {:?}",
            read.status, read.json
        );
        assert_eq!(
            read.json, created.json,
            "case {label}: a read must answer what the create answered"
        );
    }

    assert_eq!(
        emulator.stored_template_ids().await.len(),
        minted.len(),
        "every create must have persisted exactly one template, minted {minted:?}"
    );
}

/// RULE: `clientToken` is `@idempotencyToken` — a retry carrying the same token answers
/// with the original call's template instead of minting a second one, and a distinct token
/// (or none) mints a fresh one.
#[tokio::test]
async fn one_client_token_persists_one_template() {
    let emulator = Emulator::new();

    fn body(token: Option<&str>) -> serde_json::Value {
        let mut body = serde_json::json!({
            "templateName": "idempotent-template",
            "firewallType": "WAF",
            "associatedRuleList": rule_references(1),
        });
        if let Some(token) = token {
            body["clientToken"] = serde_json::json!(token);
        }
        body
    }

    // The rows differ along the token carried: the same token replayed twice, a second
    // token, and a request carrying no token at all.
    let rows: Vec<(&str, Option<&str>)> = vec![
        ("the first call of token-one", Some("token-one")),
        ("the first replay of token-one", Some("token-one")),
        ("the second replay of token-one", Some("token-one")),
        ("token-two", Some("token-two")),
        ("no token", None),
        ("another request with no token", None),
    ];

    let mut first_of_token_one: Option<serde_json::Value> = None;
    let mut distinct: Vec<String> = Vec::new();
    for (label, token) in rows {
        let reply = emulator.create_template(body(token)).await;
        assert_eq!(
            reply.status, 201,
            "case {label}: expected 201, got {} {:?}",
            reply.status, reply.json
        );
        match (token, &first_of_token_one) {
            (Some("token-one"), Some(first)) => assert_eq!(
                &reply.json, first,
                "case {label}: a replayed clientToken must answer the first call's template"
            ),
            (Some("token-one"), None) => first_of_token_one = Some(reply.json.clone()),
            _ => {}
        }
        let id = reply.string_at("templateId");
        if !distinct.contains(&id) {
            distinct.push(id);
        }
    }

    // One record per distinct token, plus one per token-less request: four in all.
    assert_eq!(
        distinct.len(),
        4,
        "expected one template per distinct clientToken and one per token-less request, got {distinct:?}"
    );
    let mut wanted = distinct.clone();
    wanted.sort();
    assert_eq!(
        emulator.stored_template_ids().await,
        wanted,
        "the store must hold exactly the templates the creates minted"
    );
}

/// RULE: an identifier the service could not answer a read with — a `ruleIdentifier` that
/// is not an ARN — is refused with the modelled `ValidationException` naming the offending
/// list member, and nothing is persisted.
#[tokio::test]
async fn a_rule_reference_that_is_not_an_arn_is_refused() {
    let emulator = Emulator::new();

    // The rows differ along the FORM of the identifier: a bare rule id, a rule name, an
    // ARN missing its resource remainder, an ARN-shaped value shorter than the 20-character
    // minimum the model's ARN members carry, and an ARN whose partition segment is empty.
    let rows: Vec<(&str, String)> = vec![
        ("a bare rule id", "abcdef0123456789".to_string()),
        ("a rule name", "my-edge-rule".to_string()),
        (
            "an ARN with no resource remainder",
            "arn:aws:network-security-manager:us-east-1:000000000000:".to_string(),
        ),
        ("an ARN shorter than the modelled minimum", "arn:a:b:c:d:e".to_string()),
        (
            "an ARN with an empty partition",
            "arn::network-security-manager:us-east-1:000000000000:rule/abc".to_string(),
        ),
    ];

    for (label, identifier) in rows {
        let reply = emulator
            .create_template(serde_json::json!({
                "templateName": "refused-template",
                "firewallType": "WAF",
                "associatedRuleList": [{ "ruleIdentifier": identifier }],
            }))
            .await;
        assert_eq!(
            reply.status, 400,
            "case {label}: expected 400, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some("ValidationException"),
            "case {label}: unexpected x-amzn-errortype, got {:?}",
            reply.error_code
        );
        let message = reply.json["message"].as_str().unwrap_or_default().to_string();
        assert!(
            message.contains("associatedRuleList") && message.contains("ruleIdentifier"),
            "case {label}: the refusal must name the member that failed, got {message:?}"
        );
        assert_eq!(
            reply.json["fieldList"][0]["name"].as_str(),
            Some("associatedRuleList"),
            "case {label}: unexpected fieldList, got {:?}",
            reply.json["fieldList"]
        );
    }

    assert!(
        emulator.stored_template_ids().await.is_empty(),
        "a refused create persists nothing"
    );

    // The same request with an ARN in that member is taken, so the refusals above turn on
    // the identifier's form and nothing else.
    let accepted = emulator
        .create_template(serde_json::json!({
            "templateName": "refused-template",
            "firewallType": "WAF",
            "associatedRuleList": [{ "ruleIdentifier": rule_arn("goodrule") }],
        }))
        .await;
    assert_eq!(
        accepted.status, 201,
        "case the same body with an ARN: expected 201, got {} {:?}",
        accepted.status, accepted.json
    );
}

/// RULE: a reference list outside `RuleReferenceList`'s modelled `@length(min: 1, max: 50)`
/// is refused with the standard constraint report — HTTP 400 `ValidationException` — and
/// persists nothing, while both bounds themselves are taken.
#[tokio::test]
async fn a_rule_list_outside_its_modelled_bound_is_refused() {
    let emulator = Emulator::new();

    for (label, rules) in [("the empty list", 0_usize), ("one past the maximum", 51)] {
        let reply = emulator
            .create_template(serde_json::json!({
                "templateName": "bounded-template",
                "firewallType": "WAF",
                "associatedRuleList": rule_references(rules),
            }))
            .await;
        assert_eq!(
            reply.status, 400,
            "case {label}: expected 400, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some("ValidationException"),
            "case {label}: unexpected x-amzn-errortype, got {:?}",
            reply.error_code
        );
    }
    assert!(
        emulator.stored_template_ids().await.is_empty(),
        "a refused create persists nothing"
    );

    for (label, rules) in [("the minimum", 1_usize), ("the maximum", 50)] {
        let reply = emulator
            .create_template(serde_json::json!({
                "templateName": "bounded-template",
                "firewallType": "WAF",
                "associatedRuleList": rule_references(rules),
            }))
            .await;
        assert_eq!(
            reply.status, 201,
            "case {label}: expected 201, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            reply.json["associatedRuleList"].as_array().map(Vec::len),
            Some(rules),
            "case {label}: unexpected associatedRuleList, got {:?}",
            reply.json["associatedRuleList"]
        );
    }
}
