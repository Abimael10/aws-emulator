// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of AWS Network Security Manager's own control plane: the
//! rule, the template, the policy and the scope an `AWS::NetworkSecurityManager::*` declares.
//! One file of [`super`]'s per-type arms, dispatched to by its `Provisioner`.
//!
//! The four entities are the same resource in four spellings, so ONE set of arms serves them
//! and the entity itself travels with the dispatch variant ([`Entity`]): each is created by a
//! `Create…` bound to its own collection (`POST /rules`, `/templates`, `/policies`,
//! `/scopes`), read by a `Get…`, changed by a `Patch`-bound `Update…` and removed by a
//! `Delete…`, each of the last three addressing the entity by the `…Identifier` the create
//! answered. That identifier is the entity's ARN — the model marks each create's `…Arn`
//! output member with the `resourceIdentifier` of the matching `…Identifier` — so the ARN
//! that service minted becomes the resource's physical id, and the `…Id` inside it is the
//! other value the type publishes.
//!
//! A declaration's properties ARE that create's own members with their initials lower-cased
//! ([`camel_cased`]), which is the one difference between a resource type's property names and
//! the API members they are. Three property shapes are not that:
//!
//! * a RULE's `Configuration`, whose keys are the firewall's own setting names rather than
//!   modelled members ("For an AWS WAF CONFIGURATION rule, provide a single web ACL setting,
//!   such as `DefaultAction` or `VisibilityConfig`") — it is a `JsonDocument`, so it is carried
//!   through exactly as declared;
//! * the ASSOCIATIONS a template and a policy declare, whose entries spell the ARN they
//!   reference under the member the entity REPORTS it in (`TemplateArn`, `RuleArn`) while the
//!   request takes the `…Identifier` of a reference shape ([`rule_references`],
//!   [`template_and_rule_references`]);
//! * a SCOPE's `ResourceScopes`, a map keyed by `ScopeResourceType` values (`AWS::EC2::EIP`)
//!   rather than by member names, whose keys are therefore carried verbatim
//!   ([`scope_configuration_member`]).
//!
//! Nothing else is invented: what the entity reports is read off that service's own answer.

use super::*;

/// AWS Network Security Manager's signing name, from its model (`sigv4Name:
/// network-security-manager`), which speaks `restJson1` — so its calls ride on
/// [`call_rest_json_named`], each on the URI its own operation's `@http` binding declares,
/// naming the operation so a refusal is reported the way that service's own SDK raises it.
pub(super) const NETWORK_SECURITY_MANAGER_SIGNING_NAME: &str = "network-security-manager";

/// WHICH of the four entities an arm is acting on, carried by the dispatch variant so one set
/// of arms serves every `AWS::NetworkSecurityManager::*` type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Entity {
    /// `AWS::NetworkSecurityManager::Rule`: one firewall configuration to enforce.
    Rule,
    /// `AWS::NetworkSecurityManager::Template`: a group of rules, reused across policies.
    Template,
    /// `AWS::NetworkSecurityManager::Policy`: templates and rules with enforcement settings.
    Policy,
    /// `AWS::NetworkSecurityManager::Scope`: the accounts and resources a deployment covers.
    Scope,
}

/// The member every create takes its tag map as.
const TAGS_MEMBER: &str = "tags";

/// The member each `Update…` carries the entity's current concurrency token in, and the one
/// every read and write answers it in: "Provide the most recent value on your next update to
/// detect and prevent conflicting concurrent modifications."
const UPDATE_TOKEN_MEMBER: &str = "updateToken";

/// The member each `Update…` requires to say whether the entity stays PUBLISHED. The create
/// defaults it to `true` ("When true, the resource is saved in published (ACTIVE) state"), so
/// the entity a stack created is published and an update of it keeps it so.
const IS_PUBLISHED_MEMBER: &str = "isPublished";

/// The error code that service answers for an entity it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of each `Get…`).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The property a RULE's firewall configuration is declared in, which is a `JsonDocument`:
/// its keys are the firewall's own setting names, so they are carried through as declared.
const CONFIGURATION_PROPERTY: &str = "Configuration";

/// The property a TEMPLATE lists the rules it groups in, and the member `CreateTemplate` and
/// `UpdateTemplate` take that list as — a `RuleReferenceList`, whose entries carry the one
/// required `ruleIdentifier`.
const ASSOCIATED_RULE_LIST_PROPERTY: &str = "AssociatedRuleList";
const ASSOCIATED_RULE_LIST_MEMBER: &str = "associatedRuleList";

/// The property a POLICY lists what it enforces in, and the member `CreatePolicy` and
/// `UpdatePolicy` take that list as — a `TemplateAndRuleReferenceList` of
/// `TemplateOrRuleReference` unions, whose documentation is "Set exactly one of
/// `templateIdentifier` or `ruleIdentifier`".
const ASSOCIATED_TEMPLATE_AND_RULE_LIST_PROPERTY: &str = "AssociatedTemplateAndRuleList";
const ASSOCIATED_TEMPLATE_AND_RULE_LIST_MEMBER: &str = "associatedTemplateAndRuleList";

/// The members an association entry names the ARN it references under, which are the members
/// the entity REPORTS an association in (`AssociatedTemplateOrRule`'s `templateArn`/`ruleArn`,
/// `AssociatedRule`'s `ruleArn`) with their initials upper-cased, as a resource type's
/// property names are.
const TEMPLATE_ARN_PROPERTY: &str = "TemplateArn";
const RULE_ARN_PROPERTY: &str = "RuleArn";

/// The members a reference shape carries that ARN in: "The identifier of the template. This is
/// the template's Amazon Resource Name (ARN)."
const TEMPLATE_IDENTIFIER_MEMBER: &str = "templateIdentifier";
const RULE_IDENTIFIER_MEMBER: &str = "ruleIdentifier";

/// The property a POLICY's enforcement order is declared in, which is an INTEGER on the wire
/// (`Priority`, `@range(min: 1)`).
///
/// A template states a value either in JSON's own spelling or as the TEXT a parameter
/// reference resolves to — `Ref` on a parameter answers that parameter's value, which a stack
/// operation carries as a string whatever the parameter's declared type — so it is read in the
/// one spelling the member takes ([`declared_number`]) before the request is sent.
const PRIORITY_PROPERTY: &str = "Priority";

/// The property a SCOPE's selection is declared in, and the member of it that is a MAP keyed
/// by `ScopeResourceType` values rather than by member names.
const SCOPE_CONFIGURATION_PROPERTY: &str = "ScopeConfiguration";
const RESOURCE_SCOPES_MEMBER: &str = "resourceScopes";

/// The refusal the POLICY's resource handler words for an association entry that does not name
/// exactly ONE of the two ARNs it may reference.
///
/// Recorded behaviour, read from the `ResourceStatusReason` of a failed policy resource (in the
/// behavioral corpus `simparity-behavioral-aws-v1`, check
/// `cloudformation__network-security-manager.deploy-network-security-manager-policy-ambiguous-\
/// reference-rejected`). The request member it stands for is a UNION — "Set exactly one of
/// `templateIdentifier` or `ruleIdentifier`" — which an entry naming both or neither cannot be
/// written as at all, so the handler refuses the declaration rather than sending a request that
/// states something else than the template asked for.
const AMBIGUOUS_ASSOCIATION_MESSAGE: &str = "Each AssociatedTemplateAndRuleList entry must \
                                             specify exactly one of TemplateArn or RuleArn.";

/// The code such a refusal carries, which is the `HandlerErrorCode` class it falls in: the
/// request the handler was given is what is at fault, so a client reading the class sees a
/// template to fix rather than a service that failed.
const INVALID_REQUEST_CODE: &str = super::provisioning::HANDLER_ERROR_INVALID_REQUEST;

impl Entity {
    /// The collection that entity's create is bound to, and the collection one of them is
    /// addressed under by the identifier that service minted.
    fn collection_uri(self) -> &'static str {
        match self {
            Self::Rule => "/rules",
            Self::Template => "/templates",
            Self::Policy => "/policies",
            Self::Scope => "/scopes",
        }
    }

    /// The operations this module calls about that entity, as that service's own model names
    /// them: the create, the read, the in-place change and the delete.
    fn create_operation(self) -> &'static str {
        match self {
            Self::Rule => "CreateRule",
            Self::Template => "CreateTemplate",
            Self::Policy => "CreatePolicy",
            Self::Scope => "CreateScope",
        }
    }

    fn get_operation(self) -> &'static str {
        match self {
            Self::Rule => "GetRule",
            Self::Template => "GetTemplate",
            Self::Policy => "GetPolicy",
            Self::Scope => "GetScope",
        }
    }

    fn update_operation(self) -> &'static str {
        match self {
            Self::Rule => "UpdateRule",
            Self::Template => "UpdateTemplate",
            Self::Policy => "UpdatePolicy",
            Self::Scope => "UpdateScope",
        }
    }

    fn delete_operation(self) -> &'static str {
        match self {
            Self::Rule => "DeleteRule",
            Self::Template => "DeleteTemplate",
            Self::Policy => "DeletePolicy",
            Self::Scope => "DeleteScope",
        }
    }

    /// The members that entity's create and read answer its identity in: the ARN that service
    /// minted and the `…Id` inside it.
    fn arn_member(self) -> &'static str {
        match self {
            Self::Rule => "ruleArn",
            Self::Template => "templateArn",
            Self::Policy => "policyArn",
            Self::Scope => "scopeArn",
        }
    }

    fn id_member(self) -> &'static str {
        match self {
            Self::Rule => "ruleId",
            Self::Template => "templateId",
            Self::Policy => "policyId",
            Self::Scope => "scopeId",
        }
    }

    /// The two attributes the type publishes those values as ([`template`]'s own catalogue of
    /// the type's published return values).
    fn arn_attribute(self) -> &'static str {
        match self {
            Self::Rule => "RuleArn",
            Self::Template => "TemplateArn",
            Self::Policy => "PolicyArn",
            Self::Scope => "ScopeArn",
        }
    }

    fn id_attribute(self) -> &'static str {
        match self {
            Self::Rule => "RuleId",
            Self::Template => "TemplateId",
            Self::Policy => "PolicyId",
            Self::Scope => "ScopeId",
        }
    }

    /// Every property of the type this arm carries onto the owning API, so a declaration
    /// stating one outside the set is REFUSED rather than reported as created without it
    /// ([`refuse_unmapped_properties`]): the create's own members plus the tag set.
    fn mapped_properties(self) -> &'static [&'static str] {
        match self {
            Self::Rule => &[
                "Configuration",
                "FirewallType",
                "RuleDescription",
                "RuleName",
                "RuleType",
                "Tags",
            ],
            Self::Template => &[
                "AssociatedRuleList",
                "FirewallType",
                "Tags",
                "TemplateDescription",
                "TemplateName",
            ],
            Self::Policy => &[
                "AssociatedTemplateAndRuleList",
                "FirewallType",
                "PolicyConfiguration",
                "PolicyDescription",
                "PolicyName",
                "Priority",
                "Tags",
            ],
            Self::Scope => &["ScopeConfiguration", "ScopeDescription", "ScopeName", "Tags"],
        }
    }

    /// The properties the CREATE carries straight onto its own members, each the member of the
    /// same name with a lower-cased initial.
    ///
    /// The association lists and the tag set are not among them: each is written in a shape of
    /// its own ([`rule_references`], [`template_and_rule_references`], [`tags_of`]).
    fn created_properties(self) -> &'static [&'static str] {
        match self {
            Self::Rule => &[
                "Configuration",
                "FirewallType",
                "RuleDescription",
                "RuleName",
                "RuleType",
            ],
            Self::Template => &["FirewallType", "TemplateDescription", "TemplateName"],
            Self::Policy => &[
                "FirewallType",
                "PolicyConfiguration",
                "PolicyDescription",
                "PolicyName",
                "Priority",
            ],
            Self::Scope => &["ScopeConfiguration", "ScopeDescription", "ScopeName"],
        }
    }

    /// The properties that entity's `Update…` takes of those — the ones a changed declaration
    /// reaches the entity in place through.
    ///
    /// Each entity's NAME is absent because no call of that service renames one
    /// ([`template::name_is_create_only`]), and so is its FIREWALL TYPE, which is why that
    /// property is one of the type's create-only ones
    /// ([`template::replacement_properties`]) rather than a member of this request.
    fn updated_properties(self) -> &'static [&'static str] {
        match self {
            Self::Rule => &["Configuration", "RuleDescription", "RuleType"],
            Self::Template => &["TemplateDescription"],
            Self::Policy => &["PolicyConfiguration", "PolicyDescription", "Priority"],
            Self::Scope => &["ScopeConfiguration", "ScopeDescription"],
        }
    }

    /// The SCALAR members that entity's read reports under the properties the type declares
    /// them as, for drift detection.
    ///
    /// A property that is a STRUCTURE, a LIST or a MAP on the wire (a rule's configuration
    /// document, either association list, a policy's or a scope's configuration) is left out,
    /// which is the position every arm whose property is not a scalar is in and what [`drift`]
    /// documents.
    fn reported_properties(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Rule => &[
                ("FirewallType", "firewallType"),
                ("RuleDescription", "ruleDescription"),
                ("RuleName", "ruleName"),
                ("RuleType", "ruleType"),
            ],
            Self::Template => &[
                ("FirewallType", "firewallType"),
                ("TemplateDescription", "templateDescription"),
                ("TemplateName", "templateName"),
            ],
            Self::Policy => &[
                ("FirewallType", "firewallType"),
                ("PolicyDescription", "policyDescription"),
                ("PolicyName", "policyName"),
                ("Priority", "priority"),
            ],
            Self::Scope => &[
                ("ScopeDescription", "scopeDescription"),
                ("ScopeName", "scopeName"),
            ],
        }
    }

    /// The URI ONE entity is addressed at: its collection, then the `…Identifier` `@httpLabel`
    /// escaped as a single path segment ([`percent_encoded`]) so an ARN's own `:` and `/`
    /// address that entity rather than another path.
    fn entity_uri(self, identifier: &str) -> String {
        format!("{}/{}", self.collection_uri(), percent_encoded(identifier))
    }
}

/// Create the entity an `AWS::NetworkSecurityManager::*` declares, in the service that owns it
/// — so a client that deployed the stack finds it through that service's own `ListRules`,
/// `ListTemplates`, `ListPolicies` and `Get…` by the identity the stack published, and the
/// policy really is in force.
///
/// Everything the declaration states is resolved by that service exactly as it is for a
/// client's own call: a policy name the account already holds a policy under, an association
/// naming something that is no ARN, and a configuration its validation refuses are all that
/// service's refusals, which fail the resource and roll the stack back rather than publishing
/// an identity for an entity nothing holds.
///
/// The identity is that service's: the ARN it minted becomes this resource's physical id — the
/// type's primary identifier, and the `…Identifier` every later call on the entity takes — and
/// it is published beside the `…Id` inside it.
pub(super) async fn create_entity(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    entity: Entity,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, entity.mapped_properties())?;
    let mut request = declared_members(&resource.properties, entity.created_properties())?;
    if let Some(references) = associations(entity, &resource.properties)? {
        request.insert(references.0, references.1);
    }
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
    }
    let answered = call_rest_json_named(
        ctx,
        NETWORK_SECURITY_MANAGER_SIGNING_NAME,
        region,
        entity.create_operation(),
        "POST",
        entity.collection_uri(),
        Some(&Value::Object(request)),
    )
    .await?;
    let arn = reported_text(&answered, entity.arn_member()).unwrap_or_default();
    if arn.is_empty() {
        // There would be no handle a client could call that service with, and nothing for the
        // stack to publish, so this is a failed create rather than a created resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: format!(
                "the AWS Network Security Manager control plane answered a created {} without \
                 an ARN",
                resource.resource_type
            ),
        });
    }
    Ok(Created {
        physical_resource_id: Some(arn.clone()),
        attributes: reported_attributes(entity, &answered, &arn),
    })
}

/// Apply a changed declaration to the entity the stack created, through that entity's own
/// `Update…` — the one call that reconfigures one that exists.
///
/// That call takes the entity's own concurrency token, so the token in force is read off the
/// entity first ([`reported_entity`]): "Each read and write returns an `updateToken`. Provide
/// the most recent value on your next update". An entity that service no longer holds has
/// nothing to change, and the refusal of the update itself is what says so.
///
/// `isPublished` is required of every update and is sent as the `true` the create defaulted to,
/// so an entity the stack published stays published.
pub(super) async fn update_entity(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    entity: Entity,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let identifier = &resource.physical_resource_id;
    let mut request = declared_members(declared, entity.updated_properties())?;
    if let Some(references) = associations(entity, declared)? {
        request.insert(references.0, references.1);
    }
    let token = reported_entity(ctx, region, entity, identifier)
        .await?
        .as_ref()
        .and_then(|held| reported_text(held, UPDATE_TOKEN_MEMBER))
        .unwrap_or_default();
    request.insert(UPDATE_TOKEN_MEMBER.to_string(), Value::String(token));
    request.insert(IS_PUBLISHED_MEMBER.to_string(), Value::Bool(true));
    let answered = call_rest_json_named(
        ctx,
        NETWORK_SECURITY_MANAGER_SIGNING_NAME,
        region,
        entity.update_operation(),
        "PATCH",
        &entity.entity_uri(identifier),
        Some(&Value::Object(request)),
    )
    .await?;
    Ok(Created::attributes(reported_attributes(
        entity, &answered, identifier,
    )))
}

/// Delete the entity a stack created, addressed by the ARN that service minted for it — this
/// resource's physical id.
///
/// Each `Delete…` is `@idempotent` and models no not-found error at all, so an identifier that
/// names no entity is already in the state the delete asks for and that service answers it as a
/// success. What it does refuse is a delete of an entity still IN USE
/// (`ConflictException`), which fails the resource rather than leaving the stack reporting a
/// deletion that did not happen.
pub(super) async fn delete_entity(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    entity: Entity,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    call_rest_json_named(
        ctx,
        NETWORK_SECURITY_MANAGER_SIGNING_NAME,
        region,
        entity.delete_operation(),
        "DELETE",
        &entity.entity_uri(physical_resource_id),
        None,
    )
    .await
    .map(|_| ())
}

/// The entity's configuration as that service reports it, for drift detection: its own read's
/// answer, under the properties the type declares those members as
/// ([`Entity::reported_properties`]). An entity it no longer holds
/// (`ResourceNotFoundException`, judged on the CODE and never on a message that carries the
/// request's own values) was deleted, which is what `None` reports.
pub(super) async fn describe_entity(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    entity: Entity,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(reported_entity(ctx, region, entity, physical_resource_id)
        .await?
        .map(|held| reported_as_properties(&held, entity.reported_properties())))
}

/// The entity record that service holds under `identifier`, or `None` when it holds none
/// (`ResourceNotFoundException`, judged on the CODE).
async fn reported_entity(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    entity: Entity,
    identifier: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json_named(
        ctx,
        NETWORK_SECURITY_MANAGER_SIGNING_NAME,
        region,
        entity.get_operation(),
        "GET",
        &entity.entity_uri(identifier),
        None,
    )
    .await
    {
        Ok(answered) => Ok(Some(answered)),
        Err(refusal) if refusal.code == RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The attributes one answered entity gives the resource: the ARN that service minted and the
/// `…Id` inside it, both read off its own answer. A member the answer does not carry falls back
/// to the identity the resource is already addressed by, so nothing publishes an empty value.
fn reported_attributes(entity: Entity, answered: &Value, arn: &str) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    attributes.insert(
        entity.arn_attribute().to_string(),
        reported_text(answered, entity.arn_member()).unwrap_or_else(|| arn.to_string()),
    );
    if let Some(id) = reported_text(answered, entity.id_member()) {
        attributes.insert(entity.id_attribute().to_string(), id);
    }
    attributes
}

/// The declared `properties` as the members of that entity's request: each property the
/// member of the same name with a lower-cased initial ([`camel_cased`]), with the three
/// property shapes that are not that written the way their own member takes them.
fn declared_members(
    declared: &Value,
    properties: &[&str],
) -> Result<Map<String, Value>, Refusal> {
    let Some(stated) = declared.as_object() else {
        return Ok(Map::new());
    };
    let mut members = Map::new();
    for property in properties {
        let Some(value) = stated.get(*property) else {
            continue;
        };
        let member = match *property {
            // A `JsonDocument`: its keys are the firewall's own setting names rather than
            // modelled members, so it travels exactly as declared.
            CONFIGURATION_PROPERTY => value.clone(),
            // An INTEGER on the wire, read in the one spelling that member takes.
            PRIORITY_PROPERTY => declared_number(value).unwrap_or_else(|| value.clone()),
            // A structure one of whose members is a map keyed by resource TYPE.
            SCOPE_CONFIGURATION_PROPERTY => scope_configuration_member(value)?,
            _ => camel_cased(value, 0)?,
        };
        members.insert(lower_initial(property), member);
    }
    Ok(members)
}

/// The association member that entity's request carries, and the list it carries, or `None`
/// for an entity whose declaration states no associations (a rule and a scope have none, and a
/// policy's list is optional).
fn associations(
    entity: Entity,
    declared: &Value,
) -> Result<Option<(String, Value)>, Refusal> {
    match entity {
        Entity::Template => Ok(declared
            .get(ASSOCIATED_RULE_LIST_PROPERTY)
            .map(|list| {
                (
                    ASSOCIATED_RULE_LIST_MEMBER.to_string(),
                    rule_references(list),
                )
            })),
        Entity::Policy => declared
            .get(ASSOCIATED_TEMPLATE_AND_RULE_LIST_PROPERTY)
            .map(|list| {
                template_and_rule_references(list).map(|references| {
                    (
                        ASSOCIATED_TEMPLATE_AND_RULE_LIST_MEMBER.to_string(),
                        references,
                    )
                })
            })
            .transpose(),
        Entity::Rule | Entity::Scope => Ok(None),
    }
}

/// A declared `AssociatedRuleList` as the `RuleReferenceList` the template's own request takes:
/// each entry's `RuleArn` under the `ruleIdentifier` that reference shape requires.
///
/// An entry that names no `RuleArn` at all is sent as the reference it is — one with no
/// identifier — so that operation's own validation is what refuses it, rather than this
/// dropping the entry and reporting a template grouping fewer rules than it declared.
fn rule_references(declared: &Value) -> Value {
    let Some(entries) = declared.as_array() else {
        return declared.clone();
    };
    Value::Array(
        entries
            .iter()
            .map(|entry| {
                let mut reference = Map::new();
                if let Some(arn) = entry.get(RULE_ARN_PROPERTY) {
                    reference.insert(RULE_IDENTIFIER_MEMBER.to_string(), arn.clone());
                }
                Value::Object(reference)
            })
            .collect(),
    )
}

/// A declared `AssociatedTemplateAndRuleList` as the `TemplateAndRuleReferenceList` the
/// policy's own request takes: each entry's `TemplateArn` or `RuleArn` under the matching
/// member of the `TemplateOrRuleReference` union.
///
/// That union is documented "Set exactly one of `templateIdentifier` or `ruleIdentifier`", so
/// an entry naming BOTH or NEITHER is one no request can be written for, and the handler
/// refuses the declaration with its own sentence ([`AMBIGUOUS_ASSOCIATION_MESSAGE`]) rather
/// than sending a request that enforces something else than the template asked for.
fn template_and_rule_references(declared: &Value) -> Result<Value, Refusal> {
    let Some(entries) = declared.as_array() else {
        return Ok(declared.clone());
    };
    let mut references = Vec::with_capacity(entries.len());
    for entry in entries {
        let template_arn = entry.get(TEMPLATE_ARN_PROPERTY);
        let rule_arn = entry.get(RULE_ARN_PROPERTY);
        let mut reference = Map::new();
        match (template_arn, rule_arn) {
            (Some(arn), None) => {
                reference.insert(TEMPLATE_IDENTIFIER_MEMBER.to_string(), arn.clone());
            }
            (None, Some(arn)) => {
                reference.insert(RULE_IDENTIFIER_MEMBER.to_string(), arn.clone());
            }
            _ => {
                return Err(Refusal {
                    code: INVALID_REQUEST_CODE.to_string(),
                    message: AMBIGUOUS_ASSOCIATION_MESSAGE.to_string(),
                })
            }
        }
        references.push(Value::Object(reference));
    }
    Ok(Value::Array(references))
}

/// A declared `ScopeConfiguration` as the member that entity's request takes it as: its
/// members re-cased ([`camel_cased`]), except the entries of its `ResourceScopes` map, whose
/// KEYS are `ScopeResourceType` values (`AWS::EC2::EIP`) rather than member names and are
/// therefore carried verbatim — each entry's VALUE is a structure, so it is re-cased like any
/// other.
fn scope_configuration_member(declared: &Value) -> Result<Value, Refusal> {
    let Some(stated) = declared.as_object() else {
        return Ok(declared.clone());
    };
    let mut members = Map::new();
    for (property, value) in stated {
        let member = lower_initial(property);
        let value = if member == RESOURCE_SCOPES_MEMBER {
            resource_scopes_member(value)?
        } else {
            camel_cased(value, 0)?
        };
        members.insert(member, value);
    }
    Ok(Value::Object(members))
}

/// One declared `ResourceScopes` map as that member takes it: every key exactly as declared,
/// every value re-cased.
fn resource_scopes_member(declared: &Value) -> Result<Value, Refusal> {
    let Some(stated) = declared.as_object() else {
        return Ok(declared.clone());
    };
    let mut scopes = Map::new();
    for (resource_type, scope) in stated {
        scopes.insert(resource_type.clone(), camel_cased(scope, 0)?);
    }
    Ok(Value::Object(scopes))
}
