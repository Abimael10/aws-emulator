// SPDX-License-Identifier: CC0-1.0

//! `UpdateRule`: what an update applies, and the optimistic concurrency control the
//! `updateToken` enforces.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a client sees, not what a handler
//! returns. The rules under test are that an update applies exactly the members the
//! request carries and leaves every absent one at its stored value, that it bumps the
//! `EntityVersion` by one and rotates the `updateToken`, and that ONLY the token the
//! stored rule currently carries is accepted: every token an earlier call handed out —
//! the create's, a read's, an earlier update's — is refused with the modelled
//! `ConflictException` and changes nothing.

use std::sync::Arc;

use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::EmulatedService;
use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::state::UPDATE_TOKEN_CONFLICT_MESSAGE;
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
    /// The error code with any shape-id prefix stripped, as a client reads it.
    fn code(&self) -> Option<String> {
        self.error_code.as_deref().map(|code| {
            code.split(['#', ':'])
                .next_back()
                .unwrap_or(code)
                .to_string()
        })
    }

    /// The refusal's `message` member.
    fn message(&self) -> Option<String> {
        self.json["message"].as_str().map(str::to_string)
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
        router.register_service(EmulatedService::new(engine));
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

    async fn update(&self, identifier: &str, body: serde_json::Value) -> Reply {
        self.call(
            "PATCH",
            &format!("/rules/{}", path_label(identifier)),
            Some(body),
        )
        .await
    }

    /// A seeded rule: an ACTIVE INSPECTION rule with a description and a configuration,
    /// and the identifier the model documents a rule read and write to name it by (its
    /// ARN).
    async fn seed(&self) -> (String, Reply) {
        let created = self
            .create(serde_json::json!({
                "ruleName": "seed-rule",
                "firewallType": "WAF",
                "ruleType": "INSPECTION",
                "ruleDescription": "the seed description",
                "isPublished": true,
                "configuration": {"Name": "seed-group", "Capacity": 1},
            }))
            .await;
        assert_eq!(
            created.status, 201,
            "setup: creating the seed rule expected 201, got {} {:?}",
            created.status, created.json
        );
        (string_at(&created.json, "ruleArn"), created)
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

/// One row of the update table: what an `UpdateRule` request may vary along.
#[derive(Debug, Clone)]
struct UpdateCase {
    label: &'static str,
    description: Option<&'static str>,
    /// `RuleType` models CONFIGURATION | INSPECTION; the seed rule is an INSPECTION one.
    rule_type: Option<&'static str>,
    configuration: Option<serde_json::Value>,
    is_published: bool,
}

impl UpdateCase {
    fn body(&self, update_token: &str) -> serde_json::Value {
        let mut body = serde_json::json!({
            "updateToken": update_token,
            "isPublished": self.is_published,
        });
        if let Some(description) = self.description {
            body["ruleDescription"] = serde_json::json!(description);
        }
        if let Some(rule_type) = self.rule_type {
            body["ruleType"] = serde_json::json!(rule_type);
        }
        if let Some(configuration) = &self.configuration {
            body["configuration"] = configuration.clone();
        }
        body
    }
}

/// The update table: rows span each optional member alone and all of them at once, both
/// `isPublished` settings, both `RuleType` values, and `configuration` documents of
/// several shapes — the member is a free-form `JsonDocument`, so an object, a nested
/// object, a list and a scalar are all values a caller may send.
fn update_cases() -> Vec<UpdateCase> {
    vec![
        UpdateCase {
            label: "description only",
            description: Some("an updated description"),
            rule_type: None,
            configuration: None,
            is_published: true,
        },
        UpdateCase {
            label: "the rule type only",
            description: None,
            rule_type: Some("CONFIGURATION"),
            configuration: None,
            is_published: true,
        },
        UpdateCase {
            label: "the configuration only",
            description: None,
            rule_type: None,
            configuration: Some(serde_json::json!({
                "Name": "replacement-group",
                "Rules": [{"Name": "block", "Priority": 1}],
            })),
            is_published: true,
        },
        UpdateCase {
            label: "a list configuration",
            description: None,
            rule_type: None,
            configuration: Some(serde_json::json!([1, 2, 3])),
            is_published: true,
        },
        UpdateCase {
            label: "a scalar configuration",
            description: None,
            rule_type: None,
            configuration: Some(serde_json::json!("a bare string document")),
            is_published: true,
        },
        UpdateCase {
            label: "unpublish, carrying nothing else",
            description: None,
            rule_type: None,
            configuration: None,
            is_published: false,
        },
        UpdateCase {
            label: "the empty description the pattern allows",
            description: Some(""),
            rule_type: None,
            configuration: None,
            is_published: true,
        },
        UpdateCase {
            label: "every member at once, as a draft",
            description: Some("all of it at once"),
            rule_type: Some("CONFIGURATION"),
            configuration: Some(serde_json::json!({"DefaultAction": {"Allow": {}}})),
            is_published: false,
        },
    ]
}

/// An update applies exactly the members it carries, leaves every absent one alone, bumps
/// the version by one, rotates the `updateToken`, never re-mints identity, and is
/// observable through `GetRule`.
#[tokio::test]
async fn update_applies_only_the_members_it_carries() {
    for case in update_cases() {
        // A fresh emulator per row, so each row updates a rule in a known state.
        let emulator = Emulator::new();
        let (identifier, created) = emulator.seed().await;
        let first_token = string_at(&created.json, "updateToken");

        let updated = emulator
            .update(&identifier, case.body(&first_token))
            .await;
        assert_eq!(
            updated.status, 200,
            "case {:?}: expected 200, got {} {:?}",
            case.label, updated.status, updated.json
        );

        assert_eq!(
            string_at(&updated.json, "version"),
            "2",
            "case {:?}: an update bumps EntityVersion to 2, got {:?}",
            case.label,
            updated.json["version"]
        );
        let second_token = string_at(&updated.json, "updateToken");
        assert!(
            is_update_token(&second_token) && second_token != first_token,
            "case {:?}: an update must mint a NEW UUID updateToken, got {second_token:?}",
            case.label
        );
        assert_eq!(
            string_at(&updated.json, "status"),
            if case.is_published { "ACTIVE" } else { "DRAFT" },
            "case {:?}: status is recomputed from isPublished, got {:?}",
            case.label,
            updated.json["status"]
        );
        // Published once, published for ever: unpublishing does not retract the published
        // version the seed create made.
        assert_eq!(
            updated.json["hasPublishedVersion"].as_bool(),
            Some(true),
            "case {:?}: expected hasPublishedVersion to stay true, got {:?}",
            case.label,
            updated.json["hasPublishedVersion"]
        );
        assert_eq!(
            updated.json["isSnapshot"].as_bool(),
            Some(false),
            "case {:?}: an updated live rule is not a snapshot, got {:?}",
            case.label,
            updated.json["isSnapshot"]
        );

        // Provided members changed; absent members kept their stored value.
        let expected_description = case
            .description
            .map_or_else(
                || created.json["ruleDescription"].clone(),
                |text| serde_json::json!(text),
            );
        assert_eq!(
            updated.json["ruleDescription"], expected_description,
            "case {:?}: expected ruleDescription {expected_description:?}, got {:?}",
            case.label, updated.json["ruleDescription"]
        );
        let expected_rule_type = case
            .rule_type
            .map_or_else(
                || created.json["ruleType"].clone(),
                |value| serde_json::json!(value),
            );
        assert_eq!(
            updated.json["ruleType"], expected_rule_type,
            "case {:?}: expected ruleType {expected_rule_type:?}, got {:?}",
            case.label, updated.json["ruleType"]
        );
        let expected_configuration = case
            .configuration
            .clone()
            .unwrap_or_else(|| created.json["configuration"].clone());
        assert_eq!(
            updated.json["configuration"], expected_configuration,
            "case {:?}: expected configuration {expected_configuration:?}, got {:?}",
            case.label, updated.json["configuration"]
        );
        // `firewallType` is not an input of this operation, so it stays what the create
        // fixed it at, and identity never moves.
        for member in ["ruleId", "ruleArn", "ruleName", "firewallType"] {
            assert_eq!(
                updated.json[member], created.json[member],
                "case {:?}: an update must not change {member}, got {:?}",
                case.label, updated.json[member]
            );
        }

        // The update is in the store, not just in the response.
        let read = emulator.get(&identifier).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: GetRule after the update expected 200, got {} {:?}",
            case.label, read.status, read.json
        );
        assert_eq!(
            read.json, updated.json,
            "case {:?}: GetRule must observe the update, got {:?}",
            case.label, read.json
        );
    }
}

/// The optimistic concurrency invariant: after every new version, the ONLY token accepted
/// is the one that version returned. Every token handed out before it — by the create, by
/// a read, by an earlier update — is refused with the modelled `ConflictException`, and a
/// refused update leaves the stored rule exactly as it was.
#[tokio::test]
async fn every_prior_update_token_is_refused_after_a_new_version() {
    let emulator = Emulator::new();
    let (identifier, created) = emulator.seed().await;

    // Rows: successive updates, each carrying different members, so the invariant holds
    // whatever an update changed.
    let rounds = [
        serde_json::json!({"ruleDescription": "round one"}),
        serde_json::json!({"configuration": {"Name": "round-two"}}),
        serde_json::json!({"ruleType": "CONFIGURATION", "ruleDescription": "round three"}),
        serde_json::json!({}),
    ];

    // Every token this rule has ever handed out, oldest first. A read hands one out too,
    // so the read's token joins the list: it is the same value as the write's until the
    // next write rotates it.
    let mut spent: Vec<String> = Vec::new();
    let mut current = string_at(&created.json, "updateToken");

    for (round, members) in rounds.iter().enumerate() {
        let before_read = emulator.get(&identifier).await;
        assert_eq!(
            string_at(&before_read.json, "updateToken"),
            current,
            "round {round}: a read must hand out the rule's current token"
        );

        let mut body = members.clone();
        body["updateToken"] = serde_json::json!(current);
        body["isPublished"] = serde_json::json!(true);
        let updated = emulator.update(&identifier, body).await;
        assert_eq!(
            updated.status, 200,
            "round {round}: the current token must be accepted, got {} {:?}",
            updated.status, updated.json
        );
        assert_eq!(
            string_at(&updated.json, "version"),
            (round + 2).to_string(),
            "round {round}: each accepted update bumps the version by one, got {:?}",
            updated.json["version"]
        );

        spent.push(current.clone());
        current = string_at(&updated.json, "updateToken");
        assert!(
            !spent.contains(&current),
            "round {round}: a rotated token must never repeat one already handed out, got \
             {current:?}"
        );

        // Every token the rule has moved past is now stale — not only the newest one.
        for (age, stale) in spent.iter().enumerate() {
            let mut body = members.clone();
            body["updateToken"] = serde_json::json!(stale);
            body["isPublished"] = serde_json::json!(true);
            let refused = emulator.update(&identifier, body).await;
            assert_eq!(
                refused.status, 409,
                "round {round}, stale token {age}: expected 409, got {} {:?}",
                refused.status, refused.json
            );
            assert_eq!(
                refused.code(),
                Some("ConflictException".to_string()),
                "round {round}, stale token {age}: expected the modelled error code, got {:?}",
                refused.error_code
            );
            // One sentence for the error class, whichever stale token was carried: the
            // refusal states neither token, so it cannot vary with the run.
            assert_eq!(
                refused.message().as_deref(),
                Some(UPDATE_TOKEN_CONFLICT_MESSAGE),
                "round {round}, stale token {age}: expected the error class's sentence, got \
                 {:?}",
                refused.message()
            );
            let after = emulator.get(&identifier).await;
            assert_eq!(
                after.json, updated.json,
                "round {round}, stale token {age}: a refused update must change nothing, got \
                 {:?}",
                after.json
            );
        }

        // A well-formed token no call ever handed out is just as stale.
        let mut body = members.clone();
        body["updateToken"] = serde_json::json!("00000000-0000-0000-0000-000000000000");
        body["isPublished"] = serde_json::json!(true);
        let refused = emulator.update(&identifier, body).await;
        assert_eq!(
            (refused.status, refused.code()),
            (409, Some("ConflictException".to_string())),
            "round {round}: a token the service never minted expected a 409 \
             ConflictException, got {} {:?}",
            refused.status,
            refused.json
        );
    }
}

/// `clientToken` is `@idempotencyToken`: replaying an update that already landed answers
/// with that update's result instead of applying it a second time, even though the first
/// attempt rotated the token the replay carries.
#[tokio::test]
async fn a_replayed_client_token_returns_the_first_update_result() {
    let emulator = Emulator::new();
    let (identifier, created) = emulator.seed().await;
    let body = serde_json::json!({
        "clientToken": "token-a",
        "updateToken": string_at(&created.json, "updateToken"),
        "isPublished": true,
        "ruleDescription": "applied once",
    });

    let first = emulator.update(&identifier, body.clone()).await;
    assert_eq!(
        first.status, 200,
        "the first update expected 200, got {} {:?}",
        first.status, first.json
    );
    // Rows: the same request twice more.
    for attempt in 1..=2 {
        let replay = emulator.update(&identifier, body.clone()).await;
        assert_eq!(
            replay.status, 200,
            "replay {attempt}: expected 200, got {} {:?}",
            replay.status, replay.json
        );
        assert_eq!(
            replay.json, first.json,
            "replay {attempt}: a replayed clientToken must answer with the original result"
        );
        assert_eq!(
            string_at(&replay.json, "version"),
            "2",
            "replay {attempt}: a replay must not bump the version again, got {:?}",
            replay.json["version"]
        );
    }

    // A fresh clientToken carrying the stale token is a conflict like any other.
    let mut stale = body.clone();
    stale["clientToken"] = serde_json::json!("token-b");
    let refused = emulator.update(&identifier, stale).await;
    assert_eq!(
        (refused.status, refused.code()),
        (409, Some("ConflictException".to_string())),
        "a fresh clientToken with a stale updateToken expected a 409 ConflictException, got \
         {} {:?}",
        refused.status,
        refused.json
    );
}

/// An identifier no rule answers to is the modelled 404, decided before the token; an
/// identifier that is not an ARN at all never reaches the lookup — its FORM is refused
/// first, with the modelled 400.
#[tokio::test]
async fn updating_an_unknown_rule_is_refused_before_the_token() {
    let emulator = Emulator::new();
    let (identifier, created) = emulator.seed().await;
    let current = string_at(&created.json, "updateToken");
    let body = |token: &str| {
        serde_json::json!({
            "updateToken": token,
            "isPublished": true,
            "ruleDescription": "never applied",
        })
    };

    // Rows: an ARN nothing holds and the stored rule id inside another resource type's
    // ARN — both the documented form, so both are decided by the lookup. Each is tried
    // with the stored rule's CURRENT token too, so the 404 is not an artefact of a stale
    // one.
    for absent in [
        engine::emulator_arn(
            "network-security-manager",
            "us-east-1",
            "rule/0123456789abcdef",
        ),
        engine::emulator_arn(
            "network-security-manager",
            "us-east-1",
            &format!("policy/{}", string_at(&created.json, "ruleId")),
        ),
    ] {
        for token in [current.as_str(), "00000000-0000-0000-0000-000000000000"] {
            let reply = emulator.update(&absent, body(token)).await;
            assert_eq!(
                (reply.status, reply.code()),
                (404, Some("ResourceNotFoundException".to_string())),
                "case {absent:?} with token {token:?}: expected a 404 \
                 ResourceNotFoundException, got {} {:?}",
                reply.status,
                reply.json
            );
        }
    }

    // Rows: a well-formed rule id and the rule's NAME — neither is an ARN, so each is
    // refused on its form rather than reported as an absent rule.
    for malformed in ["0123456789abcdef", "seed-rule"] {
        let reply = emulator.update(malformed, body(&current)).await;
        assert_eq!(
            (reply.status, reply.code()),
            (400, Some("ValidationException".to_string())),
            "case {malformed:?}: expected a 400 ValidationException, got {} {:?}",
            reply.status,
            reply.json
        );
    }

    // Nothing above touched the stored rule.
    let read = emulator.get(&identifier).await;
    assert_eq!(
        read.json, created.json,
        "a refused update must leave the stored rule as the create left it, got {:?}",
        read.json
    );
}

/// The optimistic-concurrency refusal is ONE sentence for the whole service, not one per
/// entity: a rule update and a policy update carrying a token their resource has moved
/// past are refused with the same 409 `ConflictException` message, each naming its own
/// resource through the shape's `resourceId`/`resourceType` members rather than in the
/// sentence.
#[tokio::test]
async fn every_entitys_stale_update_token_states_one_sentence() {
    let emulator = Emulator::new();

    // The rule half.
    let (rule_identifier, rule) = emulator.seed().await;
    let rule_token = string_at(&rule.json, "updateToken");
    let applied = emulator
        .update(
            &rule_identifier,
            serde_json::json!({
                "updateToken": rule_token,
                "isPublished": true,
                "ruleDescription": "moved on",
            }),
        )
        .await;
    assert_eq!(
        applied.status, 200,
        "setup: the rule update expected 200, got {} {:?}",
        applied.status, applied.json
    );

    // The policy half: a WAF policy needs its wafConfig block and an association.
    let created_policy = emulator
        .call(
            "POST",
            "/policies",
            Some(serde_json::json!({
                "policyName": "seed-policy",
                "priority": 1,
                "firewallType": "WAF",
                "policyConfiguration": {
                    "remediationEnabled": true,
                    "resourcesCleanUp": true,
                    "wafConfig": {
                        "existingCustomerWebACLResolution": "RETROFIT",
                        "conflictResolution": "MERGE_WHERE_APPLICABLE",
                    },
                },
                "associatedTemplateAndRuleList": [
                    {"ruleIdentifier": string_at(&rule.json, "ruleArn")},
                ],
            })),
        )
        .await;
    assert_eq!(
        created_policy.status, 201,
        "setup: creating the seed policy expected 201, got {} {:?}",
        created_policy.status, created_policy.json
    );
    let policy_identifier = string_at(&created_policy.json, "policyArn");
    let policy_token = string_at(&created_policy.json, "updateToken");
    let applied_policy = emulator
        .call(
            "PATCH",
            &format!("/policies/{}", path_label(&policy_identifier)),
            Some(serde_json::json!({
                "updateToken": policy_token,
                "isPublished": true,
                "policyDescription": "moved on",
            })),
        )
        .await;
    assert_eq!(
        applied_policy.status, 200,
        "setup: the policy update expected 200, got {} {:?}",
        applied_policy.status, applied_policy.json
    );

    // Rows: each entity's update replayed with the token its first call returned, which
    // the applied update moved past.
    let rows = [
        (
            "rule",
            format!("/rules/{}", path_label(&rule_identifier)),
            serde_json::json!({
                "updateToken": string_at(&rule.json, "updateToken"),
                "isPublished": true,
            }),
            "Rule",
            string_at(&rule.json, "ruleId"),
        ),
        (
            "policy",
            format!("/policies/{}", path_label(&policy_identifier)),
            serde_json::json!({
                "updateToken": string_at(&created_policy.json, "updateToken"),
                "isPublished": true,
            }),
            "Policy",
            string_at(&created_policy.json, "policyId"),
        ),
    ];
    for (label, path, body, resource_type, resource_id) in rows {
        let refused = emulator.call("PATCH", &path, Some(body)).await;
        assert_eq!(
            (refused.status, refused.code()),
            (409, Some("ConflictException".to_string())),
            "case {label:?}: expected a 409 ConflictException, got {} {:?}",
            refused.status,
            refused.json
        );
        assert_eq!(
            refused.message().as_deref(),
            Some(UPDATE_TOKEN_CONFLICT_MESSAGE),
            "case {label:?}: expected the error class's one sentence, got {:?}",
            refused.message()
        );
        assert_eq!(
            refused.json["resourceType"].as_str(),
            Some(resource_type),
            "case {label:?}: the refusal must name the resource type, got {:?}",
            refused.json["resourceType"]
        );
        assert_eq!(
            refused.json["resourceId"].as_str(),
            Some(resource_id.as_str()),
            "case {label:?}: the refusal must name the resource it is about, got {:?}",
            refused.json["resourceId"]
        );
    }
}
