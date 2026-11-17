// SPDX-License-Identifier: CC0-1.0

//! `CreateRule` and the read that proves it: `GetRule`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a client sees, not what a handler
//! returns. The rule under test is that a create mints a rule from the REQUEST and
//! persists it, and that every member of the 201 is the stored record's: reading the
//! rule back through `GetRule`, by either identifier, returns exactly what the create
//! answered.

use std::sync::Arc;

use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::EmulatedService;
use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::state::MAX_RULES;
use smithy_server_runtime::MultiServiceRouter;

/// One reply, reduced to what a caller branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    /// The restJson1 error code (`x-amzn-errortype`), absent on success.
    error_code: Option<String>,
    json: serde_json::Value,
}

/// The emulator under test: this one service mounted on the production router.
struct Emulator {
    router: MultiServiceRouter,
    /// The same shared engine the router handed the service, so a test can assert what
    /// the store holds as well as what the wire returns.
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

    /// How many rules the store holds.
    fn rules_len(&self) -> usize {
        service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::state::service_state(
            &*self.engine,
        )
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
            // The credential scope is what routes the request to this service and names
            // the region its ARNs carry.
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

    async fn create(&self, body: serde_json::Value) -> Reply {
        self.call("POST", "/rules", Some(body)).await
    }

    async fn get(&self, identifier: &str) -> Reply {
        self.call("GET", &format!("/rules/{}", path_label(identifier)), None)
            .await
    }
}

/// `ruleIdentifier` is a NON-greedy `@httpLabel`, so an identifier containing `/` — the
/// rule's ARN — reaches the server percent-encoded, exactly as an AWS SDK sends it.
fn path_label(identifier: &str) -> String {
    identifier.replace('/', "%2F")
}

fn string_at(value: &serde_json::Value, member: &str) -> String {
    value[member]
        .as_str()
        .unwrap_or_else(|| panic!("{member} is required, got {value:?}"))
        .to_string()
}

/// `RuleId`'s modelled pattern, `^[a-z0-9]{1,64}$`.
fn is_rule_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// `UpdateToken`'s modelled pattern, `^([0-9a-f]{8})-([0-9a-f]{4}-){3}([0-9a-f]{12})$`.
fn is_update_token(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    let widths = [8usize, 4, 4, 4, 12];
    groups.len() == widths.len()
        && groups.iter().zip(widths).all(|(group, width)| {
            group.len() == width
                && group
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
}

/// The ARN the service must mint for `rule_id`, built from the same ARN helper the
/// handlers use rather than from a hardcoded literal.
fn expected_rule_arn(rule_id: &str) -> String {
    engine::emulator_arn(
        "network-security-manager",
        "us-east-1",
        &format!("rule/{rule_id}"),
    )
}

/// One row of the create table: what a `CreateRule` request may vary along.
#[derive(Debug, Clone)]
struct CreateCase {
    name: &'static str,
    rule_name: String,
    rule_type: &'static str,
    description: Option<&'static str>,
    /// `None` leaves `isPublished` out of the request, which the model defaults to true.
    is_published: Option<bool>,
    configuration: serde_json::Value,
    tags: Option<serde_json::Value>,
}

impl CreateCase {
    /// The `EntityStatus` the model's "When true, the resource is saved in published
    /// (ACTIVE) state. When false, it is saved as a draft (DRAFT). Default: true."
    fn expected_status(&self) -> &'static str {
        if self.is_published.unwrap_or(true) {
            "ACTIVE"
        } else {
            "DRAFT"
        }
    }

    fn body(&self) -> serde_json::Value {
        let mut body = serde_json::json!({
            "ruleName": self.rule_name,
            // `RuleFirewallType` models exactly one value, WAF.
            "firewallType": "WAF",
            "ruleType": self.rule_type,
            "configuration": self.configuration,
        });
        if let Some(description) = self.description {
            body["ruleDescription"] = serde_json::json!(description);
        }
        if let Some(published) = self.is_published {
            body["isPublished"] = serde_json::json!(published);
        }
        if let Some(tags) = &self.tags {
            body["tags"] = tags.clone();
        }
        body
    }
}

/// The create table: rows span both `RuleType` values (the model: CONFIGURATION |
/// INSPECTION), both `isPublished` settings plus its absence, a description present and
/// absent, tags present and absent, names at the short and long ends of `RuleName`'s
/// `@length(1..128)` and across the punctuation its `@pattern` allows, and
/// `configuration` documents of several shapes — the member is a free-form
/// `JsonDocument`, so an object, a nested object, a list and a scalar are all values a
/// caller may send.
fn create_cases() -> Vec<CreateCase> {
    vec![
        CreateCase {
            name: "an inspection rule published by default",
            rule_name: "inspection-default".to_string(),
            rule_type: "INSPECTION",
            description: Some("a WAF rule group"),
            is_published: None,
            configuration: serde_json::json!({
                "Name": "probe-group",
                "Capacity": 2,
                "Rules": [{"Name": "allow", "Priority": 0}],
            }),
            tags: Some(serde_json::json!({"team": "network", "env": "test"})),
        },
        CreateCase {
            name: "a configuration rule saved as a draft",
            rule_name: "configuration-draft".to_string(),
            rule_type: "CONFIGURATION",
            description: None,
            is_published: Some(false),
            configuration: serde_json::json!({"DefaultAction": {"Allow": {}}}),
            tags: None,
        },
        CreateCase {
            name: "a configuration rule published explicitly",
            rule_name: "Cfg.rule_1:v2/all=yes+now-@here".to_string(),
            rule_type: "CONFIGURATION",
            description: Some("every character the name pattern allows _.:/=+-@"),
            is_published: Some(true),
            configuration: serde_json::json!({"VisibilityConfig": {"SampledRequestsEnabled": true}}),
            tags: Some(serde_json::json!({})),
        },
        CreateCase {
            name: "the shortest name RuleName allows",
            rule_name: "r".to_string(),
            rule_type: "INSPECTION",
            description: Some(""),
            is_published: Some(false),
            // A document need not be an object: the model constrains it no further.
            configuration: serde_json::json!([1, 2, 3]),
            tags: None,
        },
        CreateCase {
            name: "the longest name RuleName allows",
            rule_name: format!("n{}", "a".repeat(127)),
            rule_type: "INSPECTION",
            description: None,
            is_published: None,
            configuration: serde_json::json!("a bare string document"),
            tags: Some(serde_json::json!({"only": "one"})),
        },
    ]
}

/// Every member of the 201 is the stored record's, and the record is what a later read
/// returns: for each row, the create's body is echoed where the model says it is, the
/// service-minted members match the shapes the model declares, and `GetRule` answers the
/// same document for the rule's id AND for its ARN.
#[tokio::test]
async fn creating_a_rule_persists_every_member_it_answers_with() {
    let emulator = Emulator::new();
    for case in create_cases() {
        let created = emulator.create(case.body()).await;
        assert_eq!(
            created.status, 201,
            "case {:?}: expected 201, got {} {:?}",
            case.name, created.status, created.json
        );

        let rule_id = string_at(&created.json, "ruleId");
        assert!(
            is_rule_id(&rule_id),
            "case {:?}: expected a minted ruleId matching ^[a-z0-9]{{1,64}}$, got {rule_id:?}",
            case.name
        );
        assert_eq!(
            string_at(&created.json, "ruleArn"),
            expected_rule_arn(&rule_id),
            "case {:?}: ruleArn must be this emulator's ARN for the minted id",
            case.name
        );
        assert_eq!(
            string_at(&created.json, "ruleName"),
            case.rule_name,
            "case {:?}: ruleName must echo the request",
            case.name
        );
        assert_eq!(
            string_at(&created.json, "ruleType"),
            case.rule_type,
            "case {:?}: ruleType must echo the request",
            case.name
        );
        assert_eq!(
            string_at(&created.json, "firewallType"),
            "WAF",
            "case {:?}: firewallType must echo the request",
            case.name
        );
        assert_eq!(
            created.json["configuration"],
            case.configuration,
            "case {:?}: configuration must be the document the request carried, got {:?}",
            case.name,
            created.json["configuration"]
        );
        assert_eq!(
            created.json["ruleDescription"].as_str(),
            case.description,
            "case {:?}: ruleDescription must echo the request, got {:?}",
            case.name,
            created.json["ruleDescription"]
        );
        assert_eq!(
            string_at(&created.json, "status"),
            case.expected_status(),
            "case {:?}: status must follow isPublished",
            case.name
        );
        assert_eq!(
            created.json["hasPublishedVersion"].as_bool(),
            Some(case.is_published.unwrap_or(true)),
            "case {:?}: hasPublishedVersion must follow isPublished, got {:?}",
            case.name,
            created.json["hasPublishedVersion"]
        );
        assert_eq!(
            created.json["isSnapshot"].as_bool(),
            Some(false),
            "case {:?}: a created rule is the live resource, not a snapshot, got {:?}",
            case.name,
            created.json["isSnapshot"]
        );
        assert_eq!(
            string_at(&created.json, "version"),
            "1",
            "case {:?}: a first create is version 1",
            case.name
        );
        let update_token = string_at(&created.json, "updateToken");
        assert!(
            is_update_token(&update_token),
            "case {:?}: expected an updateToken matching UpdateToken's pattern, got \
             {update_token:?}",
            case.name
        );
        // `DateTimestamp` is rendered by the framework's restJson1 timestamp codec; what
        // matters here is that the member is populated from the stored instant and that
        // every later read returns the SAME value (asserted by the round trip below).
        assert!(
            !created.json["updatedAt"].is_null(),
            "case {:?}: updatedAt must be populated from the stored instant, got {:?}",
            case.name,
            created.json["updatedAt"]
        );

        // The round trip: both identifiers resolve to the same stored record, and the
        // record is exactly what the create answered.
        for identifier in [rule_id.clone(), expected_rule_arn(&rule_id)] {
            let read = emulator.get(&identifier).await;
            assert_eq!(
                read.status, 200,
                "case {:?}: reading {identifier:?} expected 200, got {} {:?}",
                case.name, read.status, read.json
            );
            assert_eq!(
                read.json, created.json,
                "case {:?}: reading {identifier:?} must return what the create answered",
                case.name
            );
        }
    }
    assert_eq!(
        emulator.rules_len(),
        create_cases().len(),
        "every row must have left one rule behind, got {}",
        emulator.rules_len()
    );
}

/// A rule's name is not one of its identifiers, and no modelled member makes it unique:
/// creating the same name repeatedly yields DISTINCT rules, each readable on its own id.
#[tokio::test]
async fn rules_sharing_a_name_are_distinct_resources() {
    let emulator = Emulator::new();
    // Rows: the same name over and over, and two names reused in an interleaved order,
    // so the rule holds whatever order the creates arrive in.
    for case in [
        vec!["shared", "shared", "shared"],
        vec!["left", "right", "left", "right"],
    ] {
        let mut ids: Vec<String> = Vec::new();
        for name in &case {
            let created = emulator
                .create(serde_json::json!({
                    "ruleName": name,
                    "firewallType": "WAF",
                    "ruleType": "INSPECTION",
                    "configuration": {"Name": name},
                }))
                .await;
            assert_eq!(
                created.status, 201,
                "case {case:?}: creating {name:?} expected 201, got {} {:?}",
                created.status, created.json
            );
            let rule_id = string_at(&created.json, "ruleId");
            assert!(
                !ids.contains(&rule_id),
                "case {case:?}: {rule_id:?} was minted twice"
            );
            ids.push(rule_id);
        }
        for (index, rule_id) in ids.iter().enumerate() {
            let read = emulator.get(rule_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: rule {index} expected 200, got {} {:?}",
                read.status, read.json
            );
            assert_eq!(
                string_at(&read.json, "ruleName"),
                case[index],
                "case {case:?}: rule {index} must keep the name it was created with"
            );
        }
    }
}

/// `clientToken` is `@idempotencyToken`: replaying a create with the same token answers
/// with the first call's result and mints nothing, while a fresh token mints a new rule.
#[tokio::test]
async fn a_replayed_client_token_returns_the_first_result() {
    let emulator = Emulator::new();
    let body = |token: &str| {
        serde_json::json!({
            "clientToken": token,
            "ruleName": "idempotent",
            "firewallType": "WAF",
            "ruleType": "CONFIGURATION",
            "configuration": {"DefaultAction": {"Block": {}}},
        })
    };

    let first = emulator.create(body("token-a")).await;
    assert_eq!(
        first.status, 201,
        "the first create expected 201, got {} {:?}",
        first.status, first.json
    );
    // Rows: the same token twice more, then a different one.
    for attempt in 1..=2 {
        let replay = emulator.create(body("token-a")).await;
        assert_eq!(
            replay.status, 201,
            "replay {attempt}: expected 201, got {} {:?}",
            replay.status, replay.json
        );
        assert_eq!(
            replay.json, first.json,
            "replay {attempt}: a replayed clientToken must answer with the original result"
        );
        assert_eq!(
            emulator.rules_len(),
            1,
            "replay {attempt}: a replay must not mint a second rule, store holds {}",
            emulator.rules_len()
        );
    }

    let fresh = emulator.create(body("token-b")).await;
    assert_ne!(
        string_at(&fresh.json, "ruleId"),
        string_at(&first.json, "ruleId"),
        "a fresh clientToken must mint a new rule"
    );
    assert_eq!(
        emulator.rules_len(),
        2,
        "a fresh clientToken must mint a new rule, store holds {}",
        emulator.rules_len()
    );
}

/// An identifier no rule answers to is the modelled 404, whatever form it takes — and a
/// rule's id does not resolve on another account's ARN shape.
#[tokio::test]
async fn reading_an_unknown_rule_is_the_modelled_not_found() {
    let emulator = Emulator::new();
    let created = emulator
        .create(serde_json::json!({
            "ruleName": "the-only-rule",
            "firewallType": "WAF",
            "ruleType": "INSPECTION",
            "configuration": {"Name": "the-only-rule"},
        }))
        .await;
    let rule_id = string_at(&created.json, "ruleId");

    // Rows: a well-formed id nothing holds, an ARN nothing holds, the stored id inside
    // another resource type's ARN, and the rule's NAME, which is not an identifier.
    for identifier in [
        "0123456789abcdef".to_string(),
        expected_rule_arn("0123456789abcdef"),
        engine::emulator_arn(
            "network-security-manager",
            "us-east-1",
            &format!("policy/{rule_id}"),
        ),
        "the-only-rule".to_string(),
    ] {
        let read = emulator.get(&identifier).await;
        assert_eq!(
            read.status, 404,
            "case {identifier:?}: expected 404, got {} {:?}",
            read.status, read.json
        );
        assert_eq!(
            read.error_code.as_deref().map(|code| code
                .split(['#', ':'])
                .next_back()
                .unwrap_or(code)
                .to_string()),
            Some("ResourceNotFoundException".to_string()),
            "case {identifier:?}: expected the modelled error code, got {:?}",
            read.error_code
        );
    }

    // The stored rule is still readable: a miss changed nothing.
    assert_eq!(
        emulator.get(&rule_id).await.status,
        200,
        "the stored rule must still be readable after the misses"
    );
}

/// The rule store is bounded: at capacity a create is refused with the modelled quota
/// error (HTTP 402) instead of growing the process, and nothing is persisted for it.
#[tokio::test]
async fn the_rule_store_is_bounded() {
    let emulator = Emulator::new();
    let filler = |index: usize| {
        serde_json::json!({
            "ruleName": format!("filler-{index}"),
            "firewallType": "WAF",
            "ruleType": "INSPECTION",
            "configuration": {"Name": format!("filler-{index}")},
        })
    };
    for index in 0..MAX_RULES {
        let reply = emulator.create(filler(index)).await;
        assert_eq!(
            reply.status, 201,
            "create {index} of {MAX_RULES} expected 201, got {} {:?}",
            reply.status, reply.json
        );
    }

    // Rows: the first create past the bound, and another after it.
    for attempt in 1..=2 {
        let refused = emulator.create(filler(MAX_RULES + attempt)).await;
        assert_eq!(
            refused.status, 402,
            "attempt {attempt}: a create past the bound expected 402, got {} {:?}",
            refused.status, refused.json
        );
        assert_eq!(
            refused.error_code.as_deref().map(|code| code
                .split(['#', ':'])
                .next_back()
                .unwrap_or(code)
                .to_string()),
            Some("ServiceQuotaExceededException".to_string()),
            "attempt {attempt}: expected the modelled error code, got {:?}",
            refused.error_code
        );
        assert_eq!(
            emulator.rules_len(),
            MAX_RULES,
            "attempt {attempt}: a refused create must persist nothing, store holds {}",
            emulator.rules_len()
        );
    }
}
