// SPDX-License-Identifier: CC0-1.0

//! `DeleteTemplate`: the removal, the refusal it owes while the template is still in use,
//! and its idempotence.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a client sees. The rule under test is
//! that a template is removed only while NO stored resource still references it: while a
//! reference remains the delete is refused with the modelled `ConflictException` (409) and
//! the template survives unchanged, and once the last reference is gone the same call
//! removes the template for good, so the read path answers its modelled 404 afterwards.
//! An identifier naming no template is already in the state the caller asked for, so the
//! delete answers its modelled 204 with nothing to remove.

use std::sync::Arc;

use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::EmulatedService;
use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::ops::types::AssociatedTemplateOrRule;
use service_com_amazonaws_networksecuritymanager_piccolocustomerapiservice::state::service_state;
use smithy_server_runtime::MultiServiceRouter;

/// The message AWS states when a resource cannot be changed because something still uses
/// it — the whole sentence is fixed text for this error class.
const IN_USE_MESSAGE: &str = "The resource is currently in use and cannot be modified";

/// The `ServiceResourceType` value naming a template, as the modelled enum declares it
/// (`smithy_query(query="shape", shape="ServiceResourceType")`). It is also the word the
/// not-found message for an absent template opens with, in front of the template id the
/// identifier names — the wording `not_found_refusal_wording.rs` pins over the whole
/// entity surface.
const RESOURCE_TYPE_TEMPLATE: &str = "Template";

/// One reply, reduced to what a caller branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    /// The restJson1 error code (`x-amzn-errortype`), absent on success.
    error_code: Option<String>,
    /// Whether the response carried no body at all, which a modelled 204 owes.
    empty_body: bool,
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

    /// How many templates the store holds.
    fn templates_len(&self) -> usize {
        service_state(&*self.engine)
            .expect("the service state is reachable from the shared engine")
            .lock()
            .templates
            .len()
    }

    /// Rewrite the stored policy's template association to `reference`, so a reference
    /// spelling a create would refuse (the bare id) can still be put in front of the
    /// delete. The store is the one `DeleteTemplate` consults, so this exercises the same
    /// reference graph a request would have written.
    fn rewrite_template_reference(&self, policy_id: &str, reference: &str) {
        let state = service_state(&*self.engine)
            .expect("the service state is reachable from the shared engine");
        let mut store = state.lock();
        let resolved = store
            .resolve_policy_id(policy_id)
            .expect("the policy was just created");
        let policy = store
            .policies
            .get_mut(&resolved)
            .expect("the policy was just created");
        for association in policy.associated_template_and_rule_list.iter_mut() {
            if matches!(association, AssociatedTemplateOrRule::templateArn(_)) {
                *association = AssociatedTemplateOrRule::templateArn(reference.to_string());
            }
        }
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
        let empty_body = raw.is_empty();
        let json = if empty_body {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null)
        };
        Reply {
            status,
            error_code,
            empty_body,
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
                // One of `RuleType`'s two modelled values.
                "ruleType": "CONFIGURATION",
                "configuration": {"DefaultAction": {"Allow": {}}},
                "isPublished": true,
            })),
        )
        .await
    }

    /// Create a template associating `rule_arns`; `associatedRuleList` is modelled
    /// `@length(min: 1)`, so every template holds at least one rule reference.
    async fn create_template(&self, name: &str, rule_arns: &[String], is_published: bool) -> Reply {
        let associations: Vec<serde_json::Value> = rule_arns
            .iter()
            .map(|arn| serde_json::json!({"ruleIdentifier": arn}))
            .collect();
        self.call(
            "POST",
            "/templates",
            Some(serde_json::json!({
                "templateName": name,
                // `TemplateFirewallType` models exactly one value, WAF.
                "firewallType": "WAF",
                "associatedRuleList": associations,
                "isPublished": is_published,
            })),
        )
        .await
    }

    /// Create a policy whose `associatedTemplateAndRuleList` names `template_reference`
    /// and every entry of `rule_arns`.
    async fn create_policy(
        &self,
        name: &str,
        template_reference: &str,
        rule_arns: &[String],
    ) -> Reply {
        let mut associations: Vec<serde_json::Value> = rule_arns
            .iter()
            .map(|arn| serde_json::json!({"ruleIdentifier": arn}))
            .collect();
        associations.push(serde_json::json!({"templateIdentifier": template_reference}));
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

    async fn get_template(&self, identifier: &str) -> Reply {
        self.call(
            "GET",
            &format!("/templates/{}", path_label(identifier)),
            None,
        )
        .await
    }

    async fn delete_template(&self, identifier: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/templates/{}", path_label(identifier)),
            None,
        )
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

    async fn list_template_ids(&self) -> Vec<String> {
        let listed = self.call("GET", "/templates", None).await;
        assert_eq!(
            listed.status, 200,
            "listing templates expected 200, got {} {:?}",
            listed.status, listed.json
        );
        listed.json["templates"]
            .as_array()
            .expect("templates is a required output member")
            .iter()
            .map(|summary| string_at(summary, "templateId"))
            .collect()
    }

    /// `n` fresh rules, as the ARNs a template or a policy associates them by.
    async fn rule_arns(&self, prefix: &str, n: usize) -> Vec<String> {
        let mut arns = Vec::new();
        for index in 0..n {
            let created = self.create_rule(&format!("{prefix}-{index}")).await;
            assert_eq!(
                created.status, 201,
                "creating rule {prefix}-{index} expected 201, got {} {:?}",
                created.status, created.json
            );
            arns.push(string_at(&created.json, "ruleArn"));
        }
        arns
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

/// How the delete NAMES the template. Both identifiers every template read resolves — the
/// ARN the model documents and the bare `templateId` — name the same record.
#[derive(Debug, Clone, Copy)]
enum IdentifierForm {
    Arn,
    BareId,
}

impl IdentifierForm {
    fn identifier(self, template_arn: &str, template_id: &str) -> String {
        match self {
            IdentifierForm::Arn => template_arn.to_string(),
            IdentifierForm::BareId => template_id.to_string(),
        }
    }
}

/// One row of the removal table.
#[derive(Debug, Clone, Copy)]
struct RemovalCase {
    name: &'static str,
    /// How many rules the template associates, so a removal is not tied to a single-entry
    /// association list.
    associated_rules: usize,
    is_published: bool,
    delete_by: IdentifierForm,
    /// How many OTHER templates the store holds, so only the named one is removed.
    other_templates: usize,
}

/// The removal table: rows span a template associating one rule and several, a published
/// and a draft template, both identifier forms a delete may name, and a store holding the
/// template alone or beside others.
fn removal_cases() -> Vec<RemovalCase> {
    vec![
        RemovalCase {
            name: "the only template, one rule, deleted by ARN",
            associated_rules: 1,
            is_published: true,
            delete_by: IdentifierForm::Arn,
            other_templates: 0,
        },
        RemovalCase {
            name: "a draft template among others, deleted by id",
            associated_rules: 3,
            is_published: false,
            delete_by: IdentifierForm::BareId,
            other_templates: 2,
        },
        RemovalCase {
            name: "a published template beside one other, deleted by id",
            associated_rules: 2,
            is_published: true,
            delete_by: IdentifierForm::BareId,
            other_templates: 1,
        },
        RemovalCase {
            name: "a draft template alone, deleted by ARN",
            associated_rules: 1,
            is_published: false,
            delete_by: IdentifierForm::Arn,
            other_templates: 0,
        },
    ]
}

/// A template nothing references is removed: the delete answers its modelled 204 with an
/// empty body, the read path answers its modelled 404 afterwards, the list no longer
/// carries it, and every OTHER template is untouched.
#[tokio::test]
async fn deleting_an_unreferenced_template_removes_only_that_template() {
    for case in removal_cases() {
        let emulator = Emulator::new();
        let rules = emulator.rule_arns("pinned", case.associated_rules).await;
        let created = emulator
            .create_template("doomed-template", &rules, case.is_published)
            .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: creating the template expected 201, got {} {:?}",
            case.name, created.status, created.json
        );
        let template_id = string_at(&created.json, "templateId");
        let template_arn = string_at(&created.json, "templateArn");

        let mut survivors = Vec::new();
        for index in 0..case.other_templates {
            let other = emulator
                .create_template(&format!("other-{index}"), &rules, true)
                .await;
            assert_eq!(
                other.status, 201,
                "case {:?}: creating other template {index} expected 201, got {} {:?}",
                case.name, other.status, other.json
            );
            survivors.push(string_at(&other.json, "templateId"));
        }

        let identifier = case.delete_by.identifier(&template_arn, &template_id);
        let removed = emulator.delete_template(&identifier).await;
        assert_eq!(
            removed.status, 204,
            "case {:?}: deleting an unreferenced template expected 204, got {} {:?}",
            case.name, removed.status, removed.json
        );
        assert!(
            removed.empty_body,
            "case {:?}: a 204 carries no body, got {:?}",
            case.name, removed.json
        );

        // Both identifiers of the removed template now name nothing.
        for spelling in [template_arn.as_str(), template_id.as_str()] {
            let read = emulator.get_template(spelling).await;
            assert_eq!(
                read.status, 404,
                "case {:?}: reading {spelling} after the delete expected 404, got {} {:?}",
                case.name, read.status, read.json
            );
            assert_eq!(
                read.code().as_deref(),
                Some("ResourceNotFoundException"),
                "case {:?}: reading {spelling} expected ResourceNotFoundException, got {:?}",
                case.name,
                read.error_code
            );
            assert_eq!(
                read.json["message"].as_str(),
                Some(format!("{RESOURCE_TYPE_TEMPLATE} not found: {template_id}").as_str()),
                "case {:?}: unexpected not-found message, got {:?}",
                case.name,
                read.json
            );
        }

        let listed = emulator.list_template_ids().await;
        assert!(
            !listed.contains(&template_id),
            "case {:?}: the removed template is still listed, got {listed:?}",
            case.name
        );
        for survivor in &survivors {
            assert!(
                listed.contains(survivor),
                "case {:?}: template {survivor} should have survived, got {listed:?}",
                case.name
            );
            let read = emulator.get_template(survivor).await;
            assert_eq!(
                read.status, 200,
                "case {:?}: reading surviving template {survivor} expected 200, got {} {:?}",
                case.name, read.status, read.json
            );
        }
        assert_eq!(
            emulator.templates_len(),
            case.other_templates,
            "case {:?}: the store should hold only the other templates",
            case.name
        );

        // `@idempotent`: deleting what is already gone is still a success.
        let again = emulator.delete_template(&identifier).await;
        assert_eq!(
            again.status, 204,
            "case {:?}: repeating the delete expected 204, got {} {:?}",
            case.name, again.status, again.json
        );
    }
}

/// How the policy's reference SPELLS the template it names. Every spelling a read of this
/// service resolves pins the template alike.
#[derive(Debug, Clone, Copy)]
enum ReferenceForm {
    /// The template's ARN, as the model documents an identifier ("This is the template's
    /// Amazon Resource Name (ARN)") and as a create requires it.
    Arn,
    /// The ARN with the version suffix a reference "optionally version-qualified" carries.
    VersionQualifiedArn,
    /// The bare `templateId`, the other identifier every template read resolves. A create
    /// refuses this spelling, so the row seeds it into the stored association instead.
    BareId,
}

/// One row of the in-use table.
#[derive(Debug, Clone, Copy)]
struct ConflictCase {
    name: &'static str,
    reference_form: ReferenceForm,
    is_published: bool,
    /// Whether the delete names the template by its ARN or by its bare `templateId` —
    /// both identifiers resolve to the same record, so both must be refused alike.
    delete_by: IdentifierForm,
    /// How many rules the holding policy also references, so a match is by ARN rather
    /// than by the reference's position in the list.
    other_references: usize,
}

/// The in-use table: rows span every spelling a policy's reference may name the template
/// by, a published and a draft template, both identifier forms a delete may name, and a
/// reference standing alone or among others.
fn conflict_cases() -> Vec<ConflictCase> {
    vec![
        ConflictCase {
            name: "a policy references the template by ARN, deleted by ARN",
            reference_form: ReferenceForm::Arn,
            is_published: true,
            delete_by: IdentifierForm::Arn,
            other_references: 0,
        },
        ConflictCase {
            name: "a policy references a draft template by ARN among rules, deleted by id",
            reference_form: ReferenceForm::Arn,
            is_published: false,
            delete_by: IdentifierForm::BareId,
            other_references: 2,
        },
        ConflictCase {
            name: "a policy references the template by a version-qualified ARN",
            reference_form: ReferenceForm::VersionQualifiedArn,
            is_published: true,
            delete_by: IdentifierForm::Arn,
            other_references: 1,
        },
        ConflictCase {
            name: "a policy references the template by its bare id, deleted by id",
            reference_form: ReferenceForm::BareId,
            is_published: true,
            delete_by: IdentifierForm::BareId,
            other_references: 0,
        },
    ]
}

/// While a stored resource still references the template, the delete is refused with the
/// modelled 409 and the template is untouched; once that resource is gone, the SAME call
/// removes the template and the read path answers its modelled 404.
#[tokio::test]
async fn deleting_a_template_a_policy_references_is_refused_until_it_is_released() {
    for case in conflict_cases() {
        let emulator = Emulator::new();
        let template_rules = emulator.rule_arns("member", 1).await;
        let created = emulator
            .create_template("pinned-template", &template_rules, case.is_published)
            .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: creating the template expected 201, got {} {:?}",
            case.name, created.status, created.json
        );
        let template_id = string_at(&created.json, "templateId");
        let template_arn = string_at(&created.json, "templateArn");

        // The rules the holding policy also references, so the template is not the only
        // entry in its association list.
        let others = emulator.rule_arns("other", case.other_references).await;

        // Every spelling a create accepts goes through the wire; the bare id, which a
        // create refuses, is written into the stored association afterwards.
        let created_reference = match case.reference_form {
            ReferenceForm::Arn | ReferenceForm::BareId => template_arn.clone(),
            ReferenceForm::VersionQualifiedArn => format!("{template_arn}:1"),
        };
        let policy = emulator
            .create_policy("holder-policy", &created_reference, &others)
            .await;
        assert_eq!(
            policy.status, 201,
            "case {:?}: creating the policy expected 201, got {} {:?}",
            case.name, policy.status, policy.json
        );
        let policy_id = string_at(&policy.json, "policyId");
        if matches!(case.reference_form, ReferenceForm::BareId) {
            emulator.rewrite_template_reference(&policy_id, &template_id);
        }

        let identifier = case.delete_by.identifier(&template_arn, &template_id);
        let refused = emulator.delete_template(&identifier).await;
        assert_eq!(
            refused.status, 409,
            "case {:?}: deleting a referenced template expected 409, got {} {:?}",
            case.name, refused.status, refused.json
        );
        assert_eq!(
            refused.code().as_deref(),
            Some("ConflictException"),
            "case {:?}: expected ConflictException, got {:?}",
            case.name,
            refused.error_code
        );
        assert_eq!(
            refused.json["message"].as_str(),
            Some(IN_USE_MESSAGE),
            "case {:?}: unexpected refusal message, got {:?}",
            case.name,
            refused.json
        );
        assert_eq!(
            refused.json["resourceId"].as_str(),
            Some(template_id.as_str()),
            "case {:?}: the refusal should name the pinned template, got {:?}",
            case.name,
            refused.json
        );
        assert_eq!(
            refused.json["resourceType"].as_str(),
            Some(RESOURCE_TYPE_TEMPLATE),
            "case {:?}: unexpected resourceType, got {:?}",
            case.name,
            refused.json
        );

        // The refused delete changed nothing: the template is still readable.
        let still_there = emulator.get_template(&identifier).await;
        assert_eq!(
            still_there.status, 200,
            "case {:?}: the refused template should still be readable, got {} {:?}",
            case.name, still_there.status, still_there.json
        );
        assert_eq!(
            still_there.json["templateId"].as_str(),
            Some(template_id.as_str()),
            "case {:?}: the surviving template should be unchanged, got {:?}",
            case.name,
            still_there.json
        );
        assert_eq!(
            emulator.templates_len(),
            1,
            "case {:?}: the refused delete must not remove the template",
            case.name
        );

        // Release the reference, and the SAME call now removes the template.
        let released = emulator.delete_policy(&policy_id).await;
        assert_eq!(
            released.status, 204,
            "case {:?}: deleting the holding policy expected 204, got {} {:?}",
            case.name, released.status, released.json
        );
        let removed = emulator.delete_template(&identifier).await;
        assert_eq!(
            removed.status, 204,
            "case {:?}: deleting the released template expected 204, got {} {:?}",
            case.name, removed.status, removed.json
        );
        let read = emulator.get_template(&identifier).await;
        assert_eq!(
            read.status, 404,
            "case {:?}: reading the removed template expected 404, got {} {:?}",
            case.name, read.status, read.json
        );
        assert_eq!(
            emulator.templates_len(),
            0,
            "case {:?}: the store should hold no template",
            case.name
        );
    }
}

/// An identifier naming no template is already in the state the caller asked for: the
/// delete is a success with nothing to remove, whatever form the identifier takes, and no
/// stored template is disturbed.
#[tokio::test]
async fn deleting_a_template_that_does_not_exist_is_a_success() {
    let emulator = Emulator::new();
    let rules = emulator.rule_arns("member", 1).await;
    let created = emulator
        .create_template("kept-template", &rules, true)
        .await;
    assert_eq!(
        created.status, 201,
        "creating the template expected 201, got {} {:?}",
        created.status, created.json
    );
    let kept_id = string_at(&created.json, "templateId");
    let kept_arn = string_at(&created.json, "templateArn");

    // Identifiers that name no stored template: a well-formed ARN of this service, an ARN
    // of another account's template, a bare id, and the kept template's ARN with a
    // different resource id.
    let absent = [
        engine::emulator_arn(
            "network-security-manager",
            "us-east-1",
            "template/absenttemplate",
        ),
        "arn:aws:network-security-manager:eu-west-1:210987654321:template/elsewhere".to_string(),
        "absenttemplateid".to_string(),
        format!("{kept_arn}x"),
    ];
    for identifier in absent {
        let reply = emulator.delete_template(&identifier).await;
        assert_eq!(
            reply.status, 204,
            "case {identifier:?}: deleting an absent template expected 204, got {} {:?}",
            reply.status, reply.json
        );
        assert!(
            reply.empty_body,
            "case {identifier:?}: a 204 carries no body, got {:?}",
            reply.json
        );
    }

    let read = emulator.get_template(&kept_arn).await;
    assert_eq!(
        read.status, 200,
        "the stored template should be untouched, got {} {:?}",
        read.status, read.json
    );
    assert_eq!(
        read.json["templateId"].as_str(),
        Some(kept_id.as_str()),
        "the stored template should be untouched, got {:?}",
        read.json
    );
}
