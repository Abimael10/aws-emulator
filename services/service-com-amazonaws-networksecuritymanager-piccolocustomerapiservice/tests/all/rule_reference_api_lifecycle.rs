// SPDX-License-Identifier: CC0-1.0

//! The rule dependency edge driven ONLY through this service's own operations:
//! `CreateRule` → `CreateTemplate`/`CreatePolicy` → `DeleteRule` → `DeleteTemplate`/
//! `DeletePolicy` → `DeleteRule` → `GetRule`.
//!
//! RULE under test: a rule is removed only once NO resource this service persists still
//! associates it — while a holder created through its own create operation still carries the
//! rule in its reference list, `DeleteRule` is refused with the modelled `ConflictException`
//! (HTTP 409) stating the fixed in-use sentence and naming the rule in the error's optional
//! `resourceId`/`resourceType`, and the refusal changes nothing (a following `GetRule`
//! answers exactly what the create answered); once the last holder is released through its
//! own delete operation, the identical `DeleteRule` answers 204 with an empty body, and
//! `GetRule` then answers the modelled 404 `ResourceNotFoundException`.
//!
//! `rule_deletion` pins the same refusal over reference records SEEDED into the store; here
//! every holder is created and released over the wire, so what is asserted is that a create
//! WRITES the reference the dependency check reads and a delete RELEASES it.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a client sees. The rows differ along what
//! the rule generalises over: which operation created the holder (template, policy), how many
//! holders pin the rule at once and in which order they are released, how many other rules
//! the holder associates beside the one under test, which spelling the reference names the
//! rule by, the `RuleType`, and whether the rule is published.

use std::sync::Arc;

use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The message this service states when a resource cannot be changed because something
/// still uses it: the whole sentence is fixed text for this error class.
const IN_USE_MESSAGE: &str = "The resource is currently in use and cannot be modified";

/// What a caller sees: the HTTP facts it branches on plus the JSON body.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    json: serde_json::Value,
}

impl Reply {
    /// The modelled error code, with any shape-id or version decoration the protocol allows
    /// around it stripped.
    fn code(&self) -> Option<String> {
        self.error_code
            .as_deref()
            .map(|code| code.split(['#', ':']).next_back().unwrap_or(code).to_string())
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
            // The credential scope is what routes the request to this service and names the
            // region its ARNs carry.
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

    async fn create_rule(&self, name: &str, rule_type: &str, is_published: bool) -> Reply {
        self.call(
            "POST",
            "/rules",
            Some(serde_json::json!({
                "ruleName": name,
                // `RuleFirewallType` models exactly one value, WAF.
                "firewallType": "WAF",
                "ruleType": rule_type,
                "configuration": {"DefaultAction": {"Allow": {}}},
                "isPublished": is_published,
            })),
        )
        .await
    }

    async fn create_template(&self, name: &str, rule_references: &[String]) -> Reply {
        let associations: Vec<serde_json::Value> = rule_references
            .iter()
            .map(|reference| serde_json::json!({"ruleIdentifier": reference}))
            .collect();
        self.call(
            "POST",
            "/templates",
            Some(serde_json::json!({
                "templateName": name,
                "firewallType": "WAF",
                "associatedRuleList": associations,
                "isPublished": true,
            })),
        )
        .await
    }

    async fn create_policy(&self, name: &str, rule_references: &[String]) -> Reply {
        let associations: Vec<serde_json::Value> = rule_references
            .iter()
            .map(|reference| serde_json::json!({"ruleIdentifier": reference}))
            .collect();
        self.call(
            "POST",
            "/policies",
            Some(serde_json::json!({
                "policyName": name,
                "priority": 100,
                "firewallType": "WAF",
                "policyConfiguration": {
                    "remediationEnabled": false,
                    "resourcesCleanUp": true,
                    // The two enum values come from `ExistingCustomerWebACLResolution` and
                    // `WAFConflictResolutionOptions` in the model.
                    "wafConfig": {
                        "existingCustomerWebACLResolution": "NO_REMEDIATION",
                        "conflictResolution": "MERGE_WHERE_APPLICABLE",
                    },
                },
                "associatedTemplateAndRuleList": associations,
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

    /// Release one holder through its own delete operation.
    async fn delete_holder(&self, kind: HolderKind, identifier: &str) -> Reply {
        let collection = match kind {
            HolderKind::Template => "templates",
            HolderKind::Policy => "policies",
        };
        self.call(
            "DELETE",
            &format!("/{collection}/{}", path_label(identifier)),
            None,
        )
        .await
    }
}

/// An identifier is a NON-greedy `@httpLabel`, so one containing `/` — a resource ARN —
/// reaches the server percent-encoded, exactly as an AWS SDK sends it.
fn path_label(identifier: &str) -> String {
    identifier.replace('/', "%2F")
}

/// Which operation created the resource that holds the reference pinning the rule.
#[derive(Debug, Clone, Copy)]
enum HolderKind {
    /// A template's `associatedRuleList`.
    Template,
    /// A policy's `associatedTemplateAndRuleList`.
    Policy,
}

impl HolderKind {
    /// The output member carrying the created holder's ARN, the identifier its delete takes.
    fn arn_member(self) -> &'static str {
        match self {
            HolderKind::Template => "templateArn",
            HolderKind::Policy => "policyArn",
        }
    }
}

/// How a holder's reference SPELLS the rule it names. Both spellings are identifiers a read
/// of this service resolves, so both pin the rule.
#[derive(Debug, Clone, Copy)]
enum ReferenceForm {
    /// The rule's ARN, as the model documents a `ruleIdentifier` ("This is the rule's Amazon
    /// Resource Name (ARN)").
    Arn,
    /// The ARN with the version suffix a reference "optionally version-qualified" carries.
    VersionQualifiedArn,
}

impl ReferenceForm {
    fn reference(self, rule_arn: &str) -> String {
        match self {
            ReferenceForm::Arn => rule_arn.to_string(),
            ReferenceForm::VersionQualifiedArn => format!("{rule_arn}:1"),
        }
    }
}

/// One holder of the reference under test.
#[derive(Debug, Clone, Copy)]
struct Holder {
    kind: HolderKind,
    reference_form: ReferenceForm,
    /// How many OTHER rules this holder references beside the one under test, so a match is
    /// by identifier rather than by the reference's position in the list.
    other_rules: usize,
}

/// One row of the dependency table.
struct Row {
    label: &'static str,
    rule_type: &'static str,
    is_published: bool,
    /// The holders to create, released in this order: the delete stays refused until the
    /// last of them is gone.
    holders: &'static [Holder],
}

/// The dependency table: rows span both operations that persist a rule reference, one and
/// two holders at once in either release order, a reference standing alone and among others,
/// both `RuleType` values and a published as well as a draft rule.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "a template references the only rule",
            rule_type: "CONFIGURATION",
            is_published: true,
            holders: &[Holder {
                kind: HolderKind::Template,
                reference_form: ReferenceForm::Arn,
                other_rules: 0,
            }],
        },
        Row {
            label: "a template references a draft rule among two others",
            rule_type: "INSPECTION",
            is_published: false,
            holders: &[Holder {
                kind: HolderKind::Template,
                reference_form: ReferenceForm::Arn,
                other_rules: 2,
            }],
        },
        Row {
            label: "a template references the rule by a version-qualified ARN",
            rule_type: "CONFIGURATION",
            is_published: true,
            holders: &[Holder {
                kind: HolderKind::Template,
                reference_form: ReferenceForm::VersionQualifiedArn,
                other_rules: 1,
            }],
        },
        Row {
            label: "a policy references the only rule",
            rule_type: "INSPECTION",
            is_published: true,
            holders: &[Holder {
                kind: HolderKind::Policy,
                reference_form: ReferenceForm::Arn,
                other_rules: 0,
            }],
        },
        Row {
            label: "a template and a policy both reference the rule, template released first",
            rule_type: "CONFIGURATION",
            is_published: true,
            holders: &[
                Holder {
                    kind: HolderKind::Template,
                    reference_form: ReferenceForm::Arn,
                    other_rules: 1,
                },
                Holder {
                    kind: HolderKind::Policy,
                    reference_form: ReferenceForm::Arn,
                    other_rules: 0,
                },
            ],
        },
        Row {
            label: "a policy and a template both reference the rule, policy released first",
            rule_type: "CONFIGURATION",
            is_published: true,
            holders: &[
                Holder {
                    kind: HolderKind::Policy,
                    reference_form: ReferenceForm::VersionQualifiedArn,
                    other_rules: 0,
                },
                Holder {
                    kind: HolderKind::Template,
                    reference_form: ReferenceForm::Arn,
                    other_rules: 0,
                },
            ],
        },
    ]
}

/// RULE: while any resource created through this API still references the rule the delete
/// names, the delete is refused with the modelled 409 and changes nothing; once the last
/// reference is released the same call removes the rule, and the read path answers its
/// modelled 404 afterwards.
#[tokio::test]
async fn a_rule_is_deletable_only_once_every_holder_releases_it() {
    for row in rows() {
        let label = row.label;
        let emulator = Emulator::new();

        let created = emulator
            .create_rule("pinned-rule", row.rule_type, row.is_published)
            .await;
        assert_eq!(
            created.status, 201,
            "case {label}: creating the rule expected 201, got {} {:?}",
            created.status, created.json
        );
        let rule_id = created.string_at("ruleId");
        let rule_arn = created.string_at("ruleArn");

        // Create each holder over the wire, so the reference the dependency check reads is
        // the one its own create operation persisted.
        let mut holder_arns: Vec<String> = Vec::new();
        let mut other_rule_arns: Vec<String> = Vec::new();
        for (index, holder) in row.holders.iter().enumerate() {
            let mut references = vec![holder.reference_form.reference(&rule_arn)];
            for other in 0..holder.other_rules {
                let extra = emulator
                    .create_rule(&format!("other-rule-{index}-{other}"), "INSPECTION", true)
                    .await;
                assert_eq!(
                    extra.status, 201,
                    "case {label}: creating the other rule {index}-{other} expected 201, got {} {:?}",
                    extra.status, extra.json
                );
                let arn = extra.string_at("ruleArn");
                other_rule_arns.push(arn.clone());
                references.push(arn);
            }
            let name = format!("holder-{index}");
            let reply = match holder.kind {
                HolderKind::Template => emulator.create_template(&name, &references).await,
                HolderKind::Policy => emulator.create_policy(&name, &references).await,
            };
            assert_eq!(
                reply.status, 201,
                "case {label}: creating holder {index} ({:?}) expected 201, got {} {:?}",
                holder.kind, reply.status, reply.json
            );
            holder_arns.push(reply.string_at(holder.kind.arn_member()));
        }

        // One holder is released per turn of this loop: while any remains, the delete is
        // refused and the rule survives untouched.
        for (index, holder) in row.holders.iter().enumerate() {
            let refused = emulator.delete_rule(&rule_arn).await;
            assert_eq!(
                refused.status, 409,
                "case {label}: with {} holder(s) left, deleting the rule expected 409, got {} {:?}",
                row.holders.len() - index,
                refused.status,
                refused.json
            );
            assert_eq!(
                refused.code(),
                Some("ConflictException".to_string()),
                "case {label}: expected the modelled error code, got {:?}",
                refused.error_code
            );
            assert_eq!(
                refused.json["message"].as_str(),
                Some(IN_USE_MESSAGE),
                "case {label}: expected the in-use message, got {:?}",
                refused.json["message"]
            );
            assert_eq!(
                refused.json["resourceId"].as_str(),
                Some(rule_id.as_str()),
                "case {label}: the refusal must name the rule it protected, got {:?}",
                refused.json["resourceId"]
            );
            assert_eq!(
                refused.json["resourceType"].as_str(),
                Some("Rule"),
                "case {label}: unexpected resourceType, got {:?}",
                refused.json["resourceType"]
            );

            // A refusal changes nothing: the rule reads back exactly as it was created,
            // version and updateToken included.
            let survived = emulator.get_rule(&rule_arn).await;
            assert_eq!(
                survived.status, 200,
                "case {label}: a refused delete must leave the rule readable, got {} {:?}",
                survived.status, survived.json
            );
            assert_eq!(
                survived.json, created.json,
                "case {label}: a refused delete must not change the rule"
            );

            let released = emulator
                .delete_holder(holder.kind, &holder_arns[index])
                .await;
            assert_eq!(
                released.status, 204,
                "case {label}: releasing holder {index} ({:?}) expected 204, got {} {:?}",
                holder.kind, released.status, released.json
            );
        }

        // Nothing references the rule any more, so the identical call removes it.
        let deleted = emulator.delete_rule(&rule_arn).await;
        assert_eq!(
            deleted.status, 204,
            "case {label}: deleting the released rule expected 204, got {} {:?}",
            deleted.status, deleted.json
        );
        assert_eq!(
            deleted.json,
            serde_json::Value::Null,
            "case {label}: the modelled output is Unit, so the body is empty, got {:?}",
            deleted.json
        );

        let gone = emulator.get_rule(&rule_arn).await;
        assert_eq!(
            gone.status, 404,
            "case {label}: reading the deleted rule expected 404, got {} {:?}",
            gone.status, gone.json
        );
        assert_eq!(
            gone.code(),
            Some("ResourceNotFoundException".to_string()),
            "case {label}: expected the modelled error code, got {:?}",
            gone.error_code
        );
        assert_eq!(
            gone.json["message"].as_str(),
            Some(format!("Rule not found: {rule_id}").as_str()),
            "case {label}: expected the miss worded with the rule id, got {:?}",
            gone.json["message"]
        );

        // The delete removed exactly the rule it named: every other rule the holders also
        // referenced is still readable.
        for other in &other_rule_arns {
            let still_there = emulator.get_rule(other).await;
            assert_eq!(
                still_there.status, 200,
                "case {label}: the rule {other:?} was not named by the delete and must \
                 survive, got {} {:?}",
                still_there.status, still_there.json
            );
        }
    }
}

/// RULE: the refusal turns on a reference STILL being held — a holder that never referenced
/// the rule does not pin it, so the same delete succeeds while that holder stands.
#[tokio::test]
async fn a_holder_that_references_another_rule_does_not_pin_this_one() {
    let emulator = Emulator::new();

    let target = emulator.create_rule("free-rule", "CONFIGURATION", true).await;
    assert_eq!(
        target.status, 201,
        "creating the rule under test expected 201, got {} {:?}",
        target.status, target.json
    );
    let target_arn = target.string_at("ruleArn");

    let decoy = emulator.create_rule("decoy-rule", "INSPECTION", true).await;
    assert_eq!(
        decoy.status, 201,
        "creating the referenced rule expected 201, got {} {:?}",
        decoy.status, decoy.json
    );
    let decoy_arn = decoy.string_at("ruleArn");

    // Rows: each holder operation, holding a reference to the OTHER rule only.
    for (label, kind) in [
        ("a template references another rule", HolderKind::Template),
        ("a policy references another rule", HolderKind::Policy),
    ] {
        let references = vec![decoy_arn.clone()];
        let holder = match kind {
            HolderKind::Template => emulator.create_template("elsewhere", &references).await,
            HolderKind::Policy => emulator.create_policy("elsewhere", &references).await,
        };
        assert_eq!(
            holder.status, 201,
            "case {label}: creating the holder expected 201, got {} {:?}",
            holder.status, holder.json
        );
    }

    let deleted = emulator.delete_rule(&target_arn).await;
    assert_eq!(
        deleted.status, 204,
        "case an unreferenced rule beside referenced ones: expected 204, got {} {:?}",
        deleted.status, deleted.json
    );
    let gone = emulator.get_rule(&target_arn).await;
    assert_eq!(
        gone.status, 404,
        "case an unreferenced rule beside referenced ones: the read after the delete \
         expected 404, got {} {:?}",
        gone.status, gone.json
    );

    // And the rule those holders DO reference is still protected.
    let refused = emulator.delete_rule(&decoy_arn).await;
    assert_eq!(
        refused.status, 409,
        "case the referenced rule: expected 409, got {} {:?}",
        refused.status, refused.json
    );
    assert_eq!(
        refused.json["message"].as_str(),
        Some(IN_USE_MESSAGE),
        "case the referenced rule: expected the in-use message, got {:?}",
        refused.json["message"]
    );
}
