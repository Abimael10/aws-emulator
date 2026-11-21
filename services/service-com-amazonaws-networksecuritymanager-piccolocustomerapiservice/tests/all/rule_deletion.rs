// SPDX-License-Identifier: CC0-1.0

//! `DeleteRule`: the removal, and the refusal it owes while the rule is still in use.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a client sees. The rule under test is
//! that a rule is removed only while NO stored resource still references its ARN: while a
//! reference remains the delete is refused with the modelled `ConflictException` (409) and
//! the rule survives unchanged, and once the last reference is gone the same call removes
//! the rule for good, so the read path answers its modelled 404 afterwards.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::EmulatedService;
use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::ops::types::AssociatedRule;
use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::state::{
    STATUS_ACTIVE, Template, mint_update_token, service_state,
};
use smithy_server_runtime::MultiServiceRouter;

/// The message AWS states when a resource cannot be changed because something still uses
/// it — the whole sentence is fixed text for this error class.
const IN_USE_MESSAGE: &str = "The resource is currently in use and cannot be modified";

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
        service_state(&*self.engine)
            .expect("the service state is reachable from the shared engine")
            .lock()
            .rules
            .len()
    }

    /// Persist a template associating `rule_arns`, the other holder of rule references in
    /// this service's state. `CreateTemplate` is still a scaffold, so the record is
    /// seeded through the same store its handler will write.
    fn attach_template(&self, template_id: &str, rule_arns: &[String]) {
        let now = SystemTime::now();
        let template = Template {
            template_id: template_id.to_string(),
            template_arn: reference_arn("template", template_id),
            template_name: format!("holder-{template_id}"),
            template_description: None,
            status: STATUS_ACTIVE.to_string(),
            firewall_type: "WAF".to_string(),
            associated_rule_list: rule_arns
                .iter()
                .map(|arn| AssociatedRule {
                    rule_arn: arn.clone(),
                })
                .collect(),
            version: 1,
            update_token: mint_update_token(),
            is_snapshot: false,
            has_published_version: true,
            tags: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        };
        service_state(&*self.engine)
            .expect("the service state is reachable from the shared engine")
            .lock()
            .templates
            .insert(template_id.to_string(), template);
    }

    /// Drop the seeded template again, releasing its references.
    fn detach_template(&self, template_id: &str) {
        service_state(&*self.engine)
            .expect("the service state is reachable from the shared engine")
            .lock()
            .templates
            .remove(template_id);
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

    async fn create_policy(&self, name: &str, rule_arns: &[String]) -> Reply {
        let associations: Vec<serde_json::Value> = rule_arns
            .iter()
            .map(|arn| serde_json::json!({"ruleIdentifier": arn}))
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
                    // The two enum values come from `ExistingCustomerWebACLResolution`
                    // and `WAFConflictResolutionOptions` in the model.
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

    async fn delete_policy(&self, identifier: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/policies/{}", path_label(identifier)),
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

fn string_at(value: &serde_json::Value, member: &str) -> String {
    value[member]
        .as_str()
        .unwrap_or_else(|| panic!("{member} is required, got {value:?}"))
        .to_string()
}

/// An ARN of one of this service's resource types, built from the same helper the handlers
/// use rather than from a hardcoded literal.
fn reference_arn(resource_type: &str, id: &str) -> String {
    engine::emulator_arn(
        "network-security-manager",
        "us-east-1",
        &format!("{resource_type}/{id}"),
    )
}

/// Which stored resource holds the reference that pins the rule.
#[derive(Debug, Clone, Copy)]
enum Holder {
    /// A policy's `associatedTemplateAndRuleList`.
    Policy,
    /// A template's `associatedRuleList`.
    Template,
}

/// How the holder's reference SPELLS the rule it names. Every spelling a read of this
/// service resolves pins the rule alike.
#[derive(Debug, Clone, Copy)]
enum ReferenceForm {
    /// The rule's ARN, as the model documents an identifier ("This is the rule's Amazon
    /// Resource Name (ARN)") and as a create requires it.
    Arn,
    /// The ARN with the version suffix a reference "optionally version-qualified" carries.
    VersionQualifiedArn,
    /// The bare `ruleId`, the other identifier every rule read resolves.
    BareId,
}

impl ReferenceForm {
    fn reference(self, rule_arn: &str, rule_id: &str) -> String {
        match self {
            ReferenceForm::Arn => rule_arn.to_string(),
            ReferenceForm::VersionQualifiedArn => format!("{rule_arn}:1"),
            ReferenceForm::BareId => rule_id.to_string(),
        }
    }
}

/// One row of the in-use table.
#[derive(Debug, Clone, Copy)]
struct ConflictCase {
    name: &'static str,
    holder: Holder,
    reference_form: ReferenceForm,
    rule_type: &'static str,
    is_published: bool,
    /// Whether the delete names the rule by its ARN or by its bare `ruleId` — both
    /// identifiers resolve to the same record, so both must be refused alike.
    delete_by_arn: bool,
    /// How many OTHER rules the holder references beside the one under test, so a match
    /// is by ARN rather than by the reference's position in the list.
    other_references: usize,
}

/// The in-use table: rows span both holders of a rule reference in this service's state,
/// every spelling a reference may name the rule by, both `RuleType` values, a published and
/// a draft rule, both identifier forms a delete may name, and a reference standing alone or
/// among others.
fn conflict_cases() -> Vec<ConflictCase> {
    vec![
        ConflictCase {
            name: "a policy references the only rule, deleted by ARN",
            holder: Holder::Policy,
            reference_form: ReferenceForm::Arn,
            rule_type: "CONFIGURATION",
            is_published: true,
            delete_by_arn: true,
            other_references: 0,
        },
        ConflictCase {
            name: "a policy references a draft rule among others, deleted by id",
            holder: Holder::Policy,
            reference_form: ReferenceForm::Arn,
            rule_type: "INSPECTION",
            is_published: false,
            delete_by_arn: false,
            other_references: 2,
        },
        ConflictCase {
            name: "a policy references the rule by a version-qualified ARN",
            holder: Holder::Policy,
            reference_form: ReferenceForm::VersionQualifiedArn,
            rule_type: "CONFIGURATION",
            is_published: true,
            delete_by_arn: true,
            other_references: 1,
        },
        ConflictCase {
            name: "a template references the rule by its bare id",
            holder: Holder::Template,
            reference_form: ReferenceForm::BareId,
            rule_type: "CONFIGURATION",
            is_published: true,
            delete_by_arn: true,
            other_references: 0,
        },
        ConflictCase {
            name: "a template references the only rule, deleted by id",
            holder: Holder::Template,
            reference_form: ReferenceForm::Arn,
            rule_type: "INSPECTION",
            is_published: true,
            delete_by_arn: false,
            other_references: 0,
        },
        ConflictCase {
            name: "a template references a draft rule among others, deleted by ARN",
            holder: Holder::Template,
            reference_form: ReferenceForm::Arn,
            rule_type: "CONFIGURATION",
            is_published: false,
            delete_by_arn: true,
            other_references: 1,
        },
    ]
}

/// While a stored resource still references the rule, the delete is refused with the
/// modelled 409 and the rule is untouched; once that resource is gone, the SAME call
/// removes the rule and the read path answers its modelled 404.
#[tokio::test]
async fn deleting_a_rule_another_resource_references_is_refused_until_it_is_released() {
    for case in conflict_cases() {
        let emulator = Emulator::new();
        let created = emulator
            .create_rule("pinned-rule", case.rule_type, case.is_published)
            .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: creating the rule expected 201, got {} {:?}",
            case.name, created.status, created.json
        );
        let rule_id = string_at(&created.json, "ruleId");
        let rule_arn = string_at(&created.json, "ruleArn");

        // The other rules the holder also references, so the one under test is not the
        // only entry in the list.
        let mut others = Vec::new();
        for index in 0..case.other_references {
            let other = emulator
                .create_rule(&format!("other-{index}"), "INSPECTION", true)
                .await;
            assert_eq!(
                other.status, 201,
                "case {:?}: creating other rule {index} expected 201, got {} {:?}",
                case.name, other.status, other.json
            );
            others.push(string_at(&other.json, "ruleArn"));
        }
        let mut references = others.clone();
        references.push(case.reference_form.reference(&rule_arn, &rule_id));

        // Attach the holder.
        let policy_identifier = match case.holder {
            Holder::Policy => {
                let policy = emulator.create_policy("holder-policy", &references).await;
                assert_eq!(
                    policy.status, 201,
                    "case {:?}: creating the policy expected 201, got {} {:?}",
                    case.name, policy.status, policy.json
                );
                Some(string_at(&policy.json, "policyArn"))
            }
            Holder::Template => {
                emulator.attach_template("holdertemplate", &references);
                None
            }
        };

        let identifier = if case.delete_by_arn {
            rule_arn.clone()
        } else {
            rule_id.clone()
        };
        let stored_rules = emulator.rules_len();

        let refused = emulator.delete_rule(&identifier).await;
        assert_eq!(
            refused.status, 409,
            "case {:?}: deleting a referenced rule expected 409, got {} {:?}",
            case.name, refused.status, refused.json
        );
        assert_eq!(
            refused.code(),
            Some("ConflictException".to_string()),
            "case {:?}: expected the modelled error code, got {:?}",
            case.name,
            refused.error_code
        );
        assert_eq!(
            refused.json["message"].as_str(),
            Some(IN_USE_MESSAGE),
            "case {:?}: expected the in-use message, got {:?}",
            case.name,
            refused.json["message"]
        );
        // A refusal changes nothing: the rule reads back exactly as it was created,
        // version and updateToken included.
        let survived = emulator.get_rule(&identifier).await;
        assert_eq!(
            survived.status, 200,
            "case {:?}: a refused delete must leave the rule readable, got {} {:?}",
            case.name, survived.status, survived.json
        );
        assert_eq!(
            survived.json, created.json,
            "case {:?}: a refused delete must not change the rule",
            case.name
        );
        assert_eq!(
            emulator.rules_len(),
            stored_rules,
            "case {:?}: a refused delete must remove nothing, store holds {}",
            case.name,
            emulator.rules_len()
        );

        // Release the only reference, and the same call now succeeds.
        match (case.holder, &policy_identifier) {
            (Holder::Policy, Some(identifier)) => {
                let released = emulator.delete_policy(identifier).await;
                assert_eq!(
                    released.status, 204,
                    "case {:?}: deleting the policy expected 204, got {} {:?}",
                    case.name, released.status, released.json
                );
            }
            _ => emulator.detach_template("holdertemplate"),
        }

        let deleted = emulator.delete_rule(&identifier).await;
        assert_eq!(
            deleted.status, 204,
            "case {:?}: deleting the released rule expected 204, got {} {:?}",
            case.name, deleted.status, deleted.json
        );
        assert_eq!(
            deleted.json,
            serde_json::Value::Null,
            "case {:?}: the modelled output is Unit, so the body is empty, got {:?}",
            case.name,
            deleted.json
        );
        assert_eq!(
            emulator.rules_len(),
            stored_rules - 1,
            "case {:?}: the delete must remove exactly the rule it named, store holds {}",
            case.name,
            emulator.rules_len()
        );

        // The removal is visible to the read path, by either identifier — and the miss is
        // worded with the rule id the identifier names, whichever form it took (the
        // version-qualified ARN among them: the version is not part of the id).
        for identifier in [
            rule_id.clone(),
            rule_arn.clone(),
            format!("{rule_arn}:1"),
        ] {
            let gone = emulator.get_rule(&identifier).await;
            assert_eq!(
                gone.status, 404,
                "case {:?}: reading {identifier:?} after the delete expected 404, got {} {:?}",
                case.name, gone.status, gone.json
            );
            assert_eq!(
                gone.code(),
                Some("ResourceNotFoundException".to_string()),
                "case {:?}: reading {identifier:?} expected the modelled error code, got {:?}",
                case.name,
                gone.error_code
            );
            assert_eq!(
                gone.json["message"].as_str(),
                Some(format!("Rule not found: {rule_id}").as_str()),
                "case {:?}: reading {identifier:?} expected the miss worded with the rule \
                 id, got {:?}",
                case.name,
                gone.json["message"]
            );
        }

        // Every other rule the holder referenced is still there: only the named one went.
        for other in &others {
            let read = emulator.get_rule(other).await;
            assert_eq!(
                read.status, 200,
                "case {:?}: {other:?} must survive another rule's delete, got {} {:?}",
                case.name, read.status, read.json
            );
        }
    }
}

/// One row of the unreferenced-delete table.
#[derive(Debug, Clone, Copy)]
struct DeleteCase {
    name: &'static str,
    rule_type: &'static str,
    is_published: bool,
    delete_by_arn: bool,
    /// Whether a policy exists that references a DIFFERENT rule — a reference elsewhere
    /// in the store must not pin this one.
    unrelated_policy: bool,
}

/// Rows span both `RuleType` values, a published and a draft rule, both identifier forms,
/// and the presence of an unrelated reference in the store.
fn delete_cases() -> Vec<DeleteCase> {
    vec![
        DeleteCase {
            name: "a published configuration rule, deleted by ARN",
            rule_type: "CONFIGURATION",
            is_published: true,
            delete_by_arn: true,
            unrelated_policy: false,
        },
        DeleteCase {
            name: "a draft inspection rule, deleted by id",
            rule_type: "INSPECTION",
            is_published: false,
            delete_by_arn: false,
            unrelated_policy: false,
        },
        DeleteCase {
            name: "a rule beside a policy that references another rule, deleted by ARN",
            rule_type: "INSPECTION",
            is_published: true,
            delete_by_arn: true,
            unrelated_policy: true,
        },
        DeleteCase {
            name: "a rule beside a policy that references another rule, deleted by id",
            rule_type: "CONFIGURATION",
            is_published: false,
            delete_by_arn: false,
            unrelated_policy: true,
        },
    ]
}

/// A rule nothing references is removed: 204 with an empty body, gone from the store, and
/// `@idempotent` — repeating the call is still a success.
#[tokio::test]
async fn deleting_an_unreferenced_rule_removes_it_and_repeats_as_a_success() {
    for case in delete_cases() {
        let emulator = Emulator::new();
        let created = emulator
            .create_rule("doomed", case.rule_type, case.is_published)
            .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: creating the rule expected 201, got {} {:?}",
            case.name, created.status, created.json
        );
        let rule_id = string_at(&created.json, "ruleId");
        let rule_arn = string_at(&created.json, "ruleArn");

        let mut keeper_arn = None;
        if case.unrelated_policy {
            let keeper = emulator.create_rule("keeper", "INSPECTION", true).await;
            assert_eq!(
                keeper.status, 201,
                "case {:?}: creating the referenced rule expected 201, got {} {:?}",
                case.name, keeper.status, keeper.json
            );
            let arn = string_at(&keeper.json, "ruleArn");
            let policy = emulator
                .create_policy("unrelated-policy", std::slice::from_ref(&arn))
                .await;
            assert_eq!(
                policy.status, 201,
                "case {:?}: creating the unrelated policy expected 201, got {} {:?}",
                case.name, policy.status, policy.json
            );
            keeper_arn = Some(arn);
        }

        let identifier = if case.delete_by_arn {
            rule_arn.clone()
        } else {
            rule_id.clone()
        };
        let expected_remaining = emulator.rules_len() - 1;

        // Rows: the delete, then the same delete again — `@idempotent`, and this
        // operation models no not-found error, so the repeat is a 204 too.
        for attempt in 1..=2 {
            let deleted = emulator.delete_rule(&identifier).await;
            assert_eq!(
                deleted.status, 204,
                "case {:?} attempt {attempt}: expected 204, got {} {:?}",
                case.name, deleted.status, deleted.json
            );
            assert_eq!(
                deleted.json,
                serde_json::Value::Null,
                "case {:?} attempt {attempt}: the modelled output is Unit, so the body is \
                 empty, got {:?}",
                case.name,
                deleted.json
            );
            assert_eq!(
                deleted.error_code, None,
                "case {:?} attempt {attempt}: a success carries no error code, got {:?}",
                case.name, deleted.error_code
            );
            assert_eq!(
                emulator.rules_len(),
                expected_remaining,
                "case {:?} attempt {attempt}: the store must hold {expected_remaining} \
                 rules, holds {}",
                case.name,
                emulator.rules_len()
            );
        }

        let gone = emulator.get_rule(&identifier).await;
        assert_eq!(
            gone.status, 404,
            "case {:?}: the deleted rule must be gone from the read path, got {} {:?}",
            case.name, gone.status, gone.json
        );

        if let Some(arn) = keeper_arn {
            let read = emulator.get_rule(&arn).await;
            assert_eq!(
                read.status, 200,
                "case {:?}: the referenced rule must be untouched, got {} {:?}",
                case.name, read.status, read.json
            );
        }
    }
}

/// `DeleteRule` models no not-found error: an identifier naming no rule is already in the
/// state the caller asked for, so it is a success that removes nothing.
#[tokio::test]
async fn deleting_a_rule_no_identifier_names_is_a_success_that_removes_nothing() {
    let emulator = Emulator::new();
    let created = emulator.create_rule("the-only-rule", "INSPECTION", true).await;
    assert_eq!(
        created.status, 201,
        "creating the rule expected 201, got {} {:?}",
        created.status, created.json
    );
    let rule_id = string_at(&created.json, "ruleId");

    // Rows: a well-formed id nothing holds, an ARN nothing holds, the stored id inside
    // another resource type's ARN, and the rule's NAME, which is not an identifier.
    for identifier in [
        "0123456789abcdef".to_string(),
        reference_arn("rule", "0123456789abcdef"),
        reference_arn("policy", &rule_id),
        "the-only-rule".to_string(),
    ] {
        let reply = emulator.delete_rule(&identifier).await;
        assert_eq!(
            reply.status, 204,
            "case {identifier:?}: expected 204, got {} {:?}",
            reply.status, reply.json
        );
        assert_eq!(
            emulator.rules_len(),
            1,
            "case {identifier:?}: nothing may be removed, store holds {}",
            emulator.rules_len()
        );
    }

    let read = emulator.get_rule(&rule_id).await;
    assert_eq!(
        read.status, 200,
        "the stored rule must still be readable, got {} {:?}",
        read.status, read.json
    );
    assert_eq!(
        read.json, created.json,
        "the stored rule must be unchanged by the misses"
    );
}

/// A reference that only STARTS with the rule's identifier names a DIFFERENT resource —
/// `:` and `/` carry on inside an ARN — so it does not pin the rule: the delete succeeds.
#[tokio::test]
async fn a_reference_that_merely_starts_with_the_identifier_does_not_pin_the_rule() {
    // Rows: a trailing segment that is not an `EntityVersion` (`^[1-9][0-9]*$`), a version
    // with a further field after it, a deeper resource path, a leading zero where a version
    // may not have one, and the bare id extended by a character.
    for suffix in [":notaversion", ":1:2", "/child", ":01", "x"] {
        let emulator = Emulator::new();
        let created = emulator.create_rule("free-rule", "CONFIGURATION", true).await;
        assert_eq!(
            created.status, 201,
            "case {suffix:?}: creating the rule expected 201, got {} {:?}",
            created.status, created.json
        );
        let rule_id = string_at(&created.json, "ruleId");
        let rule_arn = string_at(&created.json, "ruleArn");
        let base = if suffix == "x" { &rule_id } else { &rule_arn };
        emulator.attach_template("holdertemplate", &[format!("{base}{suffix}")]);

        let deleted = emulator.delete_rule(&rule_arn).await;
        assert_eq!(
            deleted.status, 204,
            "case {suffix:?}: a reference to another resource must not refuse the delete, \
             got {} {:?}",
            deleted.status, deleted.json
        );
        assert_eq!(
            emulator.rules_len(),
            0,
            "case {suffix:?}: the rule must be gone, store holds {}",
            emulator.rules_len()
        );
    }
}
