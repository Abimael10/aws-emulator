// SPDX-License-Identifier: CC0-1.0

//! `CreateRuleSnapshot`, end to end over HTTP, together with the read of the snapshot it
//! mints and the live rule it was taken of.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a client sees, not what a handler
//! returns.
//!
//! RULE under test: a snapshot is an immutable copy of the rule's PUBLISHED version. It
//! carries that version's number and that version's body — `configuration`, `ruleType`,
//! `ruleDescription` — frozen as the version published them, however far the live rule
//! moves on afterwards; its `updatedAt` is the time the snapshot was created; and the
//! members that belong to the rule as a whole rather than to one of its versions
//! (`ruleName`, `firewallType`) are reported from the LIVE rule. A rule that has published
//! no version has nothing to copy and is refused with the modelled `ConflictException`.

use std::sync::Arc;

use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::EmulatedService;
use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::state::service_state;
use smithy_server_runtime::MultiServiceRouter;

/// What a caller sees: the HTTP facts it branches on plus the JSON body.
#[derive(Debug)]
struct Reply {
    status: u16,
    /// The restJson1 error code (`x-amzn-errortype`), absent on success.
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
    /// The same shared engine the router handed the service, so a test can assert what the
    /// store holds as well as what the wire returns.
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

    async fn create_rule(&self, body: serde_json::Value) -> Reply {
        self.call("POST", "/rules", Some(body)).await
    }

    async fn get_rule(&self, identifier: &str) -> Reply {
        self.call("GET", &format!("/rules/{}", label(identifier)), None)
            .await
    }

    async fn delete_rule(&self, identifier: &str) -> Reply {
        self.call("DELETE", &format!("/rules/{}", label(identifier)), None)
            .await
    }

    async fn create_snapshot(&self, identifier: &str, body: serde_json::Value) -> Reply {
        self.call(
            "POST",
            &format!("/rules/{}/snapshots", label(identifier)),
            Some(body),
        )
        .await
    }

    async fn update_rule(&self, identifier: &str, body: serde_json::Value) -> Reply {
        self.call(
            "PATCH",
            &format!("/rules/{}", label(identifier)),
            Some(body),
        )
        .await
    }

    /// How many snapshots the store holds, over every rule.
    fn snapshots_len(&self) -> usize {
        service_state(&*self.engine)
            .expect("the service state is reachable from the shared engine")
            .lock()
            .rule_snapshots
            .len()
    }
}

/// The body of an `UpdateRule` request that writes a whole new version: every member the
/// operation can write, at the `updateToken` the previous call handed out.
fn update_body(update_token: &str, is_published: bool, configuration: &str) -> serde_json::Value {
    serde_json::json!({
        "updateToken": update_token,
        "isPublished": is_published,
        "ruleType": "CONFIGURATION",
        "ruleDescription": "the body this update wrote",
        "configuration": configuration,
    })
}

/// `ruleIdentifier` is a NON-greedy `@httpLabel`, so an identifier containing `/` — the
/// rule's ARN — reaches the server percent-encoded, exactly as an AWS SDK sends it.
fn label(identifier: &str) -> String {
    identifier.replace('/', "%2F")
}

/// An ARN of one of this service's resources, built from the same helper the handlers use
/// rather than from a hardcoded literal.
fn resource_arn(resource_type: &str, id: &str) -> String {
    engine::emulator_arn(
        "network-security-manager",
        "us-east-1",
        &format!("{resource_type}/{id}"),
    )
}

/// `UpdateToken`'s modelled pattern, `^([0-9a-f]{8})-([0-9a-f]{4}-){3}([0-9a-f]{12})$`.
fn is_update_token(value: &str) -> bool {
    let widths = [8usize, 4, 4, 4, 12];
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == widths.len()
        && groups.iter().zip(widths).all(|(group, width)| {
            group.len() == width
                && group
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
}

/// One row of the snapshot table: what the rule a snapshot is taken of may vary along.
#[derive(Debug, Clone)]
struct SnapshotCase {
    label: &'static str,
    rule_name: String,
    /// `RuleType`, one of the model's CONFIGURATION | INSPECTION.
    rule_type: &'static str,
    description: Option<&'static str>,
    /// The `configuration` the published version carries: the member is a free-form
    /// `JsonDocument`, so an object, a nested object, a list and a scalar are all values a
    /// caller may send.
    configuration: serde_json::Value,
    tags: Option<serde_json::Value>,
}

impl SnapshotCase {
    fn create_body(&self) -> serde_json::Value {
        let mut body = serde_json::json!({
            "ruleName": self.rule_name,
            // `RuleFirewallType` models exactly one value, WAF.
            "firewallType": "WAF",
            "ruleType": self.rule_type,
            "configuration": self.configuration,
            "isPublished": true,
        });
        if let Some(description) = self.description {
            body["ruleDescription"] = serde_json::json!(description);
        }
        if let Some(tags) = &self.tags {
            body["tags"] = tags.clone();
        }
        body
    }
}

/// The snapshot table: rows span both `RuleType` values, a description present, empty and
/// absent, tags present and absent, names at the short and long ends of `RuleName`'s
/// `@length(1..128)`, and `configuration` documents of several shapes.
fn snapshot_cases() -> Vec<SnapshotCase> {
    vec![
        SnapshotCase {
            label: "an inspection rule with an object configuration",
            rule_name: "snapshot-inspection".to_string(),
            rule_type: "INSPECTION",
            description: Some("a WAF rule group"),
            configuration: serde_json::json!({
                "Name": "probe-group",
                "Capacity": 2,
                "Rules": [{"Name": "allow", "Priority": 0}],
            }),
            tags: Some(serde_json::json!({"team": "network", "env": "test"})),
        },
        SnapshotCase {
            label: "a configuration rule with a nested configuration",
            rule_name: "Cfg.rule_1:v2/all=yes+now-@here".to_string(),
            rule_type: "CONFIGURATION",
            description: None,
            configuration: serde_json::json!({
                "DefaultAction": {"Allow": {}},
                "VisibilityConfig": {"SampledRequestsEnabled": true, "MetricName": "m"},
            }),
            tags: None,
        },
        SnapshotCase {
            label: "the shortest name RuleName allows, with a list configuration",
            rule_name: "r".to_string(),
            rule_type: "INSPECTION",
            description: Some(""),
            configuration: serde_json::json!([1, 2, 3]),
            tags: Some(serde_json::json!({})),
        },
        SnapshotCase {
            label: "the longest name RuleName allows, with a scalar configuration",
            rule_name: format!("n{}", "a".repeat(127)),
            rule_type: "CONFIGURATION",
            description: Some("every character the description pattern allows _.:/=+-@"),
            configuration: serde_json::json!("a bare string is a JsonDocument too"),
            tags: None,
        },
    ]
}

/// Assert every member a snapshot of `version` must carry, given the frozen body it was
/// taken of and the live rule it belongs to.
struct Expected<'a> {
    rule_id: &'a str,
    /// The LIVE rule's ARN: a snapshot answers with it carrying the version suffix.
    rule_arn: &'a str,
    /// Read from the live rule, not from the frozen version.
    rule_name: &'a str,
    version: u32,
    frozen_rule_type: &'a str,
    frozen_description: Option<&'a str>,
    frozen_configuration: &'a serde_json::Value,
}

fn assert_snapshot(case: &str, snapshot: &Reply, expected: &Expected<'_>) {
    assert_eq!(
        snapshot.json["isSnapshot"].as_bool(),
        Some(true),
        "case {case}: a snapshot must say so, got {:?}",
        snapshot.json["isSnapshot"]
    );
    assert_eq!(
        snapshot.json["hasPublishedVersion"].as_bool(),
        Some(true),
        "case {case}: a snapshot is a copy of a published version, got {:?}",
        snapshot.json["hasPublishedVersion"]
    );
    assert_eq!(
        snapshot.json["ruleId"].as_str(),
        Some(expected.rule_id),
        "case {case}: a snapshot is a version of its rule, so it carries that ruleId, got {:?}",
        snapshot.json["ruleId"]
    );
    assert_eq!(
        snapshot.json["ruleArn"].as_str(),
        Some(format!("{}:{}", expected.rule_arn, expected.version).as_str()),
        "case {case}: unexpected ruleArn, got {:?}",
        snapshot.json["ruleArn"]
    );
    assert_eq!(
        snapshot.json["ruleName"].as_str(),
        Some(expected.rule_name),
        "case {case}: ruleName is read from the live rule, got {:?}",
        snapshot.json["ruleName"]
    );
    assert_eq!(
        snapshot.json["firewallType"].as_str(),
        Some("WAF"),
        "case {case}: unexpected firewallType, got {:?}",
        snapshot.json["firewallType"]
    );
    assert_eq!(
        snapshot.json["status"].as_str(),
        Some("ACTIVE"),
        "case {case}: a copy of a published version is in use, got {:?}",
        snapshot.json["status"]
    );
    assert_eq!(
        snapshot.json["version"].as_str(),
        Some(expected.version.to_string().as_str()),
        "case {case}: a snapshot carries the version it copied, got {:?}",
        snapshot.json["version"]
    );
    assert_eq!(
        snapshot.json["ruleType"].as_str(),
        Some(expected.frozen_rule_type),
        "case {case}: ruleType must be the published version's, got {:?}",
        snapshot.json["ruleType"]
    );
    assert_eq!(
        snapshot.json["ruleDescription"].as_str(),
        expected.frozen_description,
        "case {case}: ruleDescription must be the published version's, got {:?}",
        snapshot.json["ruleDescription"]
    );
    assert_eq!(
        &snapshot.json["configuration"], expected.frozen_configuration,
        "case {case}: configuration must be the published version's, got {:?}",
        snapshot.json["configuration"]
    );
    assert!(
        is_update_token(&snapshot.string_at("updateToken")),
        "case {case}: unexpected updateToken, got {:?}",
        snapshot.json["updateToken"]
    );
    assert!(
        snapshot.json["updatedAt"].is_string(),
        "case {case}: updatedAt is the modelled date-time the snapshot was created at, got {:?}",
        snapshot.json["updatedAt"]
    );
}

/// RULE: a snapshot freezes the rule's published version, is rendered from the record it
/// persists — so the version-qualified identifier it answers with reads back exactly the
/// same — and is taken once per published version.
#[tokio::test]
async fn create_rule_snapshot_freezes_the_published_version_and_reads_back() {
    let emulator = Emulator::new();
    for case in snapshot_cases() {
        let label = case.label;
        let created = emulator.create_rule(case.create_body()).await;
        assert_eq!(
            created.status, 201,
            "case {label}: CreateRule expected 201, got {} {:?}",
            created.status, created.json
        );
        let rule_id = created.string_at("ruleId");
        let rule_arn = created.string_at("ruleArn");

        let snapshot = emulator
            .create_snapshot(&rule_arn, serde_json::json!({}))
            .await;
        assert_eq!(
            snapshot.status, 201,
            "case {label}: CreateRuleSnapshot expected 201, got {} {:?}",
            snapshot.status, snapshot.json
        );
        let expected = Expected {
            rule_id: &rule_id,
            rule_arn: &rule_arn,
            rule_name: &case.rule_name,
            version: 1,
            frozen_rule_type: case.rule_type,
            frozen_description: case.description,
            frozen_configuration: &case.configuration,
        };
        assert_snapshot(
            &format!("{label}, the snapshot of version 1"),
            &snapshot,
            &expected,
        );
        let snapshot_arn = snapshot.string_at("ruleArn");

        // Read back through the identifier the snapshot answered with: the stored record,
        // member for member what the create returned.
        let read = emulator.get_rule(&snapshot_arn).await;
        assert_eq!(
            read.status, 200,
            "case {label}: GetRule of the snapshot expected 200, got {} {:?}",
            read.status, read.json
        );
        assert_snapshot(
            &format!("{label}, the snapshot read back"),
            &read,
            &expected,
        );
        assert_eq!(
            read.json, snapshot.json,
            "case {label}: the read must observe exactly what the create answered, got {:?}",
            read.json
        );

        // An immutable copy of one published version is not minted twice: asking again
        // answers with the stored snapshot, down to its own `updateToken` and `updatedAt`.
        let again = emulator
            .create_snapshot(&rule_arn, serde_json::json!({}))
            .await;
        assert_eq!(
            again.status, 201,
            "case {label}: the second CreateRuleSnapshot expected 201, got {} {:?}",
            again.status, again.json
        );
        assert_eq!(
            again.json, snapshot.json,
            "case {label}: the snapshot of a version already frozen is the stored one, got {:?}",
            again.json
        );

        // Taking a snapshot changed nothing about the live rule.
        let live = emulator.get_rule(&rule_arn).await;
        assert_eq!(
            live.json, created.json,
            "case {label}: taking a snapshot must leave the live rule untouched, got {:?}",
            live.json
        );
    }
    assert_eq!(
        emulator.snapshots_len(),
        snapshot_cases().len(),
        "each published version is frozen exactly once, store holds {}",
        emulator.snapshots_len()
    );
}

/// RULE: the frozen body belongs to the version, the rule-wide members to the rule — so a
/// live rule moved on by a DRAFT update changes neither the snapshot's version nor its
/// body, and the snapshot still reads back through the identifier it answered with.
#[tokio::test]
async fn a_snapshot_keeps_its_version_while_the_live_rule_moves_on() {
    let emulator = Emulator::new();
    for case in snapshot_cases() {
        let label = case.label;
        let created = emulator.create_rule(case.create_body()).await;
        assert_eq!(
            created.status, 201,
            "case {label}: CreateRule expected 201, got {} {:?}",
            created.status, created.json
        );
        let rule_id = created.string_at("ruleId");
        let rule_arn = created.string_at("ruleArn");
        let snapshot_arn = emulator
            .create_snapshot(&rule_arn, serde_json::json!({}))
            .await
            .string_at("ruleArn");

        // A DRAFT update moves the live rule on: a new body, at version 2, unpublished.
        let live_configuration = "a body only the live rule carries";
        let updated = emulator
            .update_rule(
                &rule_arn,
                update_body(
                    &created.string_at("updateToken"),
                    false,
                    live_configuration,
                ),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {label}: the draft UpdateRule expected 200, got {} {:?}",
            updated.status, updated.json
        );
        assert_eq!(
            updated.json["status"].as_str(),
            Some("DRAFT"),
            "case {label}: the draft update leaves the rule unpublished, got {:?}",
            updated.json["status"]
        );

        let read = emulator.get_rule(&snapshot_arn).await;
        assert_eq!(
            read.status, 200,
            "case {label}: the snapshot must still read back, got {} {:?}",
            read.status, read.json
        );
        assert_snapshot(
            &format!("{label}, the snapshot read after the live rule moved on"),
            &read,
            &Expected {
                rule_id: &rule_id,
                rule_arn: &rule_arn,
                // The name belongs to the rule, not to one of its versions, and no update
                // changes it.
                rule_name: &case.rule_name,
                version: 1,
                frozen_rule_type: case.rule_type,
                frozen_description: case.description,
                frozen_configuration: &case.configuration,
            },
        );

        // The live rule reads back as the record it now is, at its own version.
        let live = emulator.get_rule(&rule_arn).await;
        assert_eq!(
            live.json["version"].as_str(),
            Some("2"),
            "case {label}: the live rule carries its own version, got {:?}",
            live.json["version"]
        );
        assert_eq!(
            live.json["configuration"],
            serde_json::json!(live_configuration),
            "case {label}: the live rule carries its own body, got {:?}",
            live.json["configuration"]
        );
        assert_eq!(
            live.json["isSnapshot"].as_bool(),
            Some(false),
            "case {label}: the live rule is not a snapshot, got {:?}",
            live.json["isSnapshot"]
        );

        // A draft update published nothing, so the snapshot of the published version is
        // still the one already frozen.
        let again = emulator
            .create_snapshot(&rule_arn, serde_json::json!({}))
            .await;
        assert_eq!(
            again.json, read.json,
            "case {label}: a draft update must not move what a snapshot copies, got {:?}",
            again.json
        );
    }
}

/// RULE: the snapshot copies the version the rule has PUBLISHED, so a publishing update
/// decides what the next snapshot freezes — whether it is the rule's first published
/// version or a later one — while the snapshots already taken stay as they were.
#[tokio::test]
async fn a_publishing_update_is_what_the_next_snapshot_freezes() {
    // Row (a): a draft create has nothing to copy until an update publishes one. Row (b):
    // a published create is frozen at version 1, and a publishing update moves what the
    // next snapshot copies to version 2.
    for (case, created_published) in [
        ("a rule published for the first time by an update", false),
        ("a rule whose published version an update replaces", true),
    ] {
        let emulator = Emulator::new();
        let created = emulator
            .create_rule(serde_json::json!({
                "ruleName": "published-by-update",
                "firewallType": "WAF",
                "ruleType": "INSPECTION",
                "configuration": {"Name": "the body the create wrote"},
                "isPublished": created_published,
            }))
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: CreateRule expected 201, got {} {:?}",
            created.status, created.json
        );
        let rule_id = created.string_at("ruleId");
        let rule_arn = created.string_at("ruleArn");

        // What the create published, if anything, is frozen before the update runs.
        let first_snapshot = emulator
            .create_snapshot(&rule_arn, serde_json::json!({}))
            .await;
        let expected_first_status = if created_published { 201 } else { 409 };
        assert_eq!(
            first_snapshot.status, expected_first_status,
            "case {case}: the snapshot before the update expected {expected_first_status}, \
             got {} {:?}",
            first_snapshot.status, first_snapshot.json
        );

        let published_configuration = "the body the publishing update wrote";
        let published = emulator
            .update_rule(
                &rule_arn,
                update_body(
                    &created.string_at("updateToken"),
                    true,
                    published_configuration,
                ),
            )
            .await;
        assert_eq!(
            published.status, 200,
            "case {case}: the publishing UpdateRule expected 200, got {} {:?}",
            published.status, published.json
        );
        assert_eq!(
            published.json["version"].as_str(),
            Some("2"),
            "case {case}: an update bumps the version, got {:?}",
            published.json["version"]
        );

        let snapshot = emulator
            .create_snapshot(&rule_arn, serde_json::json!({}))
            .await;
        assert_eq!(
            snapshot.status, 201,
            "case {case}: the snapshot of the newly published version expected 201, got {} {:?}",
            snapshot.status, snapshot.json
        );
        let live_configuration = serde_json::json!(published_configuration);
        assert_snapshot(
            &format!("{case}, the snapshot of the version the update published"),
            &snapshot,
            &Expected {
                rule_id: &rule_id,
                rule_arn: &rule_arn,
                rule_name: "published-by-update",
                version: 2,
                frozen_rule_type: "CONFIGURATION",
                frozen_description: Some("the body this update wrote"),
                frozen_configuration: &live_configuration,
            },
        );
        assert_eq!(
            emulator.get_rule(&snapshot.string_at("ruleArn")).await.json,
            snapshot.json,
            "case {case}: the new snapshot must read back through its own identifier"
        );

        // The snapshot the create's version left behind, if there was one, is untouched.
        if created_published {
            let older = emulator.get_rule(&format!("{rule_arn}:1")).await;
            assert_eq!(
                older.json, first_snapshot.json,
                "case {case}: the older snapshot is immutable, got {:?}",
                older.json
            );
            assert_eq!(
                emulator.snapshots_len(),
                2,
                "case {case}: each published version is frozen once, store holds {}",
                emulator.snapshots_len()
            );
        } else {
            assert_eq!(
                emulator.get_rule(&format!("{rule_arn}:1")).await.status,
                404,
                "case {case}: the draft create published no version 1 to freeze"
            );
            assert_eq!(
                emulator.snapshots_len(),
                1,
                "case {case}: only the published version is frozen, store holds {}",
                emulator.snapshots_len()
            );
        }
    }
}

/// RULE: a rule that has published no version has nothing to copy, so the snapshot is
/// refused with the modelled `ConflictException` and nothing is persisted.
#[tokio::test]
async fn create_rule_snapshot_refuses_a_rule_with_no_published_version() {
    let emulator = Emulator::new();
    for rule_type in ["CONFIGURATION", "INSPECTION"] {
        let created = emulator
            .create_rule(serde_json::json!({
                "ruleName": format!("draft-{}", rule_type.to_lowercase()),
                "firewallType": "WAF",
                "ruleType": rule_type,
                "configuration": {"DefaultAction": {"Block": {}}},
                "isPublished": false,
            }))
            .await;
        assert_eq!(
            created.status, 201,
            "case {rule_type}: CreateRule expected 201, got {} {:?}",
            created.status, created.json
        );
        let rule_arn = created.string_at("ruleArn");

        let refused = emulator
            .create_snapshot(&rule_arn, serde_json::json!({}))
            .await;
        assert_eq!(
            refused.status, 409,
            "case {rule_type}: expected 409, got {} {:?}",
            refused.status, refused.json
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("ConflictException"),
            "case {rule_type}: unexpected x-amzn-errortype, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.json["resourceType"].as_str(),
            Some("Rule"),
            "case {rule_type}: unexpected resourceType, got {:?}",
            refused.json["resourceType"]
        );
        assert_eq!(
            refused.json["resourceId"].as_str(),
            Some(created.string_at("ruleId").as_str()),
            "case {rule_type}: the refusal names the rule, got {:?}",
            refused.json["resourceId"]
        );

        // Nothing was frozen: version 1 of this rule resolves to no snapshot.
        let miss = emulator.get_rule(&format!("{rule_arn}:1")).await;
        assert_eq!(
            miss.status, 404,
            "case {rule_type}: a refused snapshot persists nothing, got {} {:?}",
            miss.status, miss.json
        );
        assert_eq!(
            emulator.snapshots_len(),
            0,
            "case {rule_type}: the store must hold no snapshot, it holds {}",
            emulator.snapshots_len()
        );
    }
}

/// RULE: `ruleIdentifier` is the rule's ARN. A value of another form is a field that
/// failed validation; an ARN that names no rule is the read's own miss.
#[tokio::test]
async fn create_rule_snapshot_refuses_identifiers_no_rule_answers_to() {
    let emulator = Emulator::new();
    let created = emulator
        .create_rule(serde_json::json!({
            "ruleName": "identifier-forms",
            "firewallType": "WAF",
            "ruleType": "INSPECTION",
            "configuration": {"Name": "group"},
            "isPublished": true,
        }))
        .await;
    assert_eq!(
        created.status, 201,
        "CreateRule expected 201, got {} {:?}",
        created.status, created.json
    );
    let rule_id = created.string_at("ruleId");
    let rule_arn = created.string_at("ruleArn");

    // An ARN is a form the lookup decides: each of these names nothing.
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
    ] {
        let reply = emulator
            .create_snapshot(&identifier, serde_json::json!({}))
            .await;
        assert_eq!(
            reply.status, 404,
            "case {case} ({identifier:?}): expected 404, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case} ({identifier:?}): unexpected x-amzn-errortype, got {:?}",
            reply.error_code
        );
        assert_eq!(
            reply.json["resourceType"].as_str(),
            Some("Rule"),
            "case {case} ({identifier:?}): unexpected resourceType, got {:?}",
            reply.json["resourceType"]
        );
    }

    // Values that are not ARNs at all: refused as a field of the request.
    for (case, identifier) in [
        ("the bare rule id", rule_id.clone()),
        ("the rule's name", "identifier-forms".to_string()),
        ("a resource path", format!("rule/{rule_id}")),
        ("an ARN missing its resource", "arn:aws:network-security-manager:us-east-1:123456789012:".to_string()),
    ] {
        let reply = emulator
            .create_snapshot(&identifier, serde_json::json!({}))
            .await;
        assert_eq!(
            reply.status, 400,
            "case {case} ({identifier:?}): expected 400, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some("ValidationException"),
            "case {case} ({identifier:?}): unexpected x-amzn-errortype, got {:?}",
            reply.error_code
        );
    }

    assert_eq!(
        emulator.snapshots_len(),
        0,
        "no refusal may persist a snapshot, store holds {}",
        emulator.snapshots_len()
    );
    // The one well-formed identifier still works, so none of the refusals disturbed the
    // store.
    assert_eq!(
        emulator
            .create_snapshot(&rule_arn, serde_json::json!({}))
            .await
            .status,
        201,
        "the rule's own ARN must still be accepted"
    );
}

/// RULE: `clientToken` is `@idempotencyToken` — replaying a request with the same token
/// answers with the original call's result and freezes nothing a second time.
#[tokio::test]
async fn replaying_a_client_token_answers_with_the_first_snapshot() {
    let emulator = Emulator::new();
    let created = emulator
        .create_rule(serde_json::json!({
            "ruleName": "idempotent-snapshot",
            "firewallType": "WAF",
            "ruleType": "INSPECTION",
            "configuration": {"Name": "group", "Capacity": 1},
            "isPublished": true,
        }))
        .await;
    let rule_arn = created.string_at("ruleArn");

    let first = emulator
        .create_snapshot(
            &rule_arn,
            serde_json::json!({ "clientToken": "token-one", "tags": { "take": "first" } }),
        )
        .await;
    assert_eq!(
        first.status, 201,
        "the first snapshot expected 201, got {} {:?}",
        first.status, first.json
    );

    for (case, body) in [
        (
            "the same token and the same parameters",
            serde_json::json!({ "clientToken": "token-one", "tags": { "take": "first" } }),
        ),
        (
            "the same token with other parameters",
            serde_json::json!({ "clientToken": "token-one", "tags": { "take": "second" } }),
        ),
        ("a different token", serde_json::json!({ "clientToken": "token-two" })),
        ("no token at all", serde_json::json!({})),
    ] {
        let retry = emulator.create_snapshot(&rule_arn, body).await;
        assert_eq!(
            retry.status, 201,
            "case {case}: expected 201, got {} {:?}",
            retry.status, retry.json
        );
        assert_eq!(
            retry.json, first.json,
            "case {case}: the reply must be the first snapshot's, got {:?}",
            retry.json
        );
    }
    assert_eq!(
        emulator.snapshots_len(),
        1,
        "one published version is one snapshot, store holds {}",
        emulator.snapshots_len()
    );
}

/// RULE: a snapshot is a version of its rule, so deleting the rule takes its snapshots
/// with it — the version-qualified identifier resolves to nothing afterwards.
#[tokio::test]
async fn deleting_a_rule_removes_the_snapshots_taken_of_it() {
    let emulator = Emulator::new();
    let created = emulator
        .create_rule(serde_json::json!({
            "ruleName": "rule-to-delete",
            "firewallType": "WAF",
            "ruleType": "CONFIGURATION",
            "configuration": {"DefaultAction": {"Allow": {}}},
            "isPublished": true,
        }))
        .await;
    let rule_arn = created.string_at("ruleArn");
    let snapshot_arn = emulator
        .create_snapshot(&rule_arn, serde_json::json!({}))
        .await
        .string_at("ruleArn");
    assert_eq!(
        emulator.snapshots_len(),
        1,
        "the snapshot must be stored before the delete, store holds {}",
        emulator.snapshots_len()
    );

    assert_eq!(
        emulator.delete_rule(&rule_arn).await.status,
        204,
        "DeleteRule expected 204"
    );
    for (case, identifier) in [
        ("the deleted rule", rule_arn),
        ("its snapshot", snapshot_arn),
    ] {
        let reply = emulator.get_rule(&identifier).await;
        assert_eq!(
            reply.status, 404,
            "case {case}: a deleted rule takes its snapshots with it, got {} {:?}",
            reply.status, reply.json
        );
    }
    assert_eq!(
        emulator.snapshots_len(),
        0,
        "the delete must release the snapshot it kept, store holds {}",
        emulator.snapshots_len()
    );
}
