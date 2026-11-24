// SPDX-License-Identifier: CC0-1.0

//! The `AWS::ECR::SigningConfiguration` provider: the ONE signing configuration the PRIVATE
//! Amazon ECR registry of an account holds, written and removed through that registry's own
//! API.
//!
//! The declaration states the configuration `PutSigningConfiguration` takes — documented
//! "Creates or updates the registry's signing configuration" — so that one call is this type's
//! create and its update alike, and `DeleteSigningConfiguration` is what takes it away. None of
//! the three carries an identifier: a registry holds one configuration, which is why a CREATE
//! PROBES the registry first. A registry that already holds one is not this stack's to
//! overwrite, and a put over it would report a resource the stack took over rather than
//! created, so the create answers the refusal CloudFormation itself words for a resource whose
//! identifier is already in use — the same position the EventBridge rule arm's upsert is in
//! ([`already_in_use`]).

use super::*;

/// The member `PutSigningConfiguration` requires, which is also the member every answer carries
/// the stored configuration in: the declaration's own rules are sent inside it.
///
/// Source: that call's own signature (`smithy_query(query="operation",
/// operation="PutSigningConfiguration")` — `signingConfiguration`, `@required`). No property of
/// the type is spelled for it: the type publishes the CONFIGURATION'S members as its properties
/// ([`SIGNING_RULES_PROPERTY`]) rather than the wrapper the call takes them in.
const SIGNING_CONFIGURATION_MEMBER: &str = "signingConfiguration";

/// The one property an `AWS::ECR::SigningConfiguration` declares: the rules the registry signs
/// by, which is `SigningConfiguration`'s own member with its initial letter upper-cased —
/// the one difference between a CloudFormation property name and the API member it is
/// (`smithy_query(query="shape", shape="SigningConfiguration")` — `rules`, `@required`,
/// `@length(min: 0, max: 10)`).
const SIGNING_RULES_PROPERTY: &str = "Rules";
/// See [`SIGNING_RULES_PROPERTY`].
const SIGNING_RULES_MEMBER: &str = "rules";

/// One signing RULE's scalar members, as the property each is declared under against the
/// member `SigningRule` spells it: the Amazon Web Services Signer profile the matched images
/// are signed with (`smithy_query(query="shape", shape="SigningRule")`).
///
/// `RepositoryFilters` is a LIST of structures of its own, so the members inside it are a
/// table of their own ([`REPOSITORY_FILTER_MEMBERS`]) rather than a value passed through.
///
/// ONE table for both directions: the request a declaration is sent as
/// ([`declared_rule`]) and the configuration a drift read reports
/// ([`reported_rule`]) are the same pairs read each way, so the two cannot come to spell the
/// same member differently.
const SIGNING_RULE_MEMBERS: &[(&str, &str)] = &[("SigningProfileArn", "signingProfileArn")];

/// The repository filters of such a rule, as the property against `SigningRule`'s own member.
const SIGNING_RULE_FILTERS_PROPERTY: &str = "RepositoryFilters";
/// See [`SIGNING_RULE_FILTERS_PROPERTY`].
const SIGNING_RULE_FILTERS_MEMBER: &str = "repositoryFilters";

/// One repository FILTER's members, read the same way as the rule's above
/// (`smithy_query(query="shape", shape="SigningRepositoryFilter")` — `filter` and
/// `filterType`, both `@required`).
const REPOSITORY_FILTER_MEMBERS: &[(&str, &str)] =
    &[("Filter", "filter"), ("FilterType", "filterType")];

/// The three calls on that configuration, as Amazon ECR's own model names them.
const PUT_SIGNING_CONFIGURATION: &str = "PutSigningConfiguration";
const GET_SIGNING_CONFIGURATION: &str = "GetSigningConfiguration";
const DELETE_SIGNING_CONFIGURATION: &str = "DeleteSigningConfiguration";

/// The error code that registry answers for a registry holding NO signing configuration: "the
/// specified signing configuration was not found. This occurs when attempting to retrieve or
/// delete a signing configuration that does not exist", which both the read and the delete
/// answer.
const SIGNING_CONFIGURATION_NOT_FOUND: &str = "SigningConfigurationNotFoundException";

/// Put the signing configuration an `AWS::ECR::SigningConfiguration` declares, for a registry
/// that holds NONE: so a client that pushes an image to a repository the declared rules match
/// gets it signed, and that registry's own `GetSigningConfiguration` answers the rules the
/// template asked for.
///
/// The put is an UPSERT ("Creates or updates the registry's signing configuration"), so the
/// registry is probed first: a configuration it ALREADY holds is not this stack's to replace,
/// and that is the refusal CloudFormation answers for a resource whose identifier — here the
/// registry the configuration belongs to — is already in use.
pub(crate) async fn create_signing_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    if read_signing_configuration(ctx, region).await?.is_some() {
        return Err(already_in_use(resource));
    }
    put_signing_configuration(ctx, region, resource, &resource.properties).await
}

/// Write the declared configuration onto the registry, which is both this type's create
/// ([`create_signing_configuration`]) and its UPDATE: the one call is documented to create or
/// update the configuration, and a registry holds exactly one — so a stack that re-states the
/// rules replaces the ones in force rather than making a second configuration.
///
/// The resource keeps the identity CloudFormation computed for it — the registry whose
/// configuration it is — so this answers attributes alone, and it reports none: the one
/// attribute the type publishes is that registry, which the naming rules already computed and
/// the put's own answer does not carry.
pub(crate) async fn put_signing_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(
        &StoredResource {
            properties: declared.clone(),
            ..resource.clone()
        },
        &[SIGNING_RULES_PROPERTY],
    )?;
    let mut request = Map::new();
    request.insert(
        SIGNING_CONFIGURATION_MEMBER.to_string(),
        declared_configuration(resource, declared)?,
    );
    call_ecr(
        ctx,
        region,
        PUT_SIGNING_CONFIGURATION,
        &Value::Object(request),
    )
    .await?;
    Ok(Created::default())
}

/// Take the registry's signing configuration away, which is what the delete of this type is:
/// the call carries no identifier, because the configuration it removes is the one the registry
/// holds. A registry that holds none is already in the state the delete asks for.
pub(crate) async fn delete_signing_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Result<(), Refusal> {
    match call_ecr(
        ctx,
        region,
        DELETE_SIGNING_CONFIGURATION,
        &Value::Object(Map::new()),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == SIGNING_CONFIGURATION_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The signing configuration as Amazon ECR reports it, for drift detection: the rules its own
/// read answers for this registry, under the property name the type declares them with
/// ([`SIGNING_RULES_PROPERTY`]) — so the two documents a drift detection compares are in one
/// spelling and a configuration in step with its template is never reported as modified.
///
/// A registry that no longer holds one was deleted, which is what `None` reports.
pub(crate) async fn describe_signing_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Result<Option<Value>, Refusal> {
    let Some(configuration) = read_signing_configuration(ctx, region).await? else {
        return Ok(None);
    };
    let reported_rules = configuration
        .get(SIGNING_RULES_MEMBER)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut properties = Map::new();
    properties.insert(
        SIGNING_RULES_PROPERTY.to_string(),
        Value::Array(reported_rules.iter().map(reported_rule).collect()),
    );
    Ok(Some(Value::Object(properties)))
}

/// The configuration that registry holds, as it answers it, or `None` for a registry holding
/// none ([`SIGNING_CONFIGURATION_NOT_FOUND`]) — the one read behind both the create's probe and
/// the drift report, so neither can judge the registry's state differently.
async fn read_signing_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Result<Option<Value>, Refusal> {
    match call_ecr(
        ctx,
        region,
        GET_SIGNING_CONFIGURATION,
        &Value::Object(Map::new()),
    )
    .await
    {
        Ok(answer) => Ok(answer.get(SIGNING_CONFIGURATION_MEMBER).cloned()),
        Err(refusal) if refusal.code == SIGNING_CONFIGURATION_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The configuration the declaration states, as the `signingConfiguration` the put takes: its
/// rules, each mapped member by member onto the ones `SigningRule` spells.
///
/// A declaration carrying no `Rules` at all is one no `PutSigningConfiguration` can be built
/// from — that call's own member is `@required` and this is the property it is built out of — so
/// the resource is refused rather than put as a configuration the template did not describe. A
/// declaration stating an EMPTY list is not that: `rules` is `@length(min: 0, max: 10)`, so a
/// configuration holding no rule is one that registry takes, and it is sent as declared.
fn declared_configuration(resource: &StoredResource, declared: &Value) -> Result<Value, Refusal> {
    let rules = declared
        .get(SIGNING_RULES_PROPERTY)
        .and_then(Value::as_array)
        .ok_or_else(|| Refusal {
            code: INVALID_PROPERTY_CODE.to_string(),
            message: format!(
                "the {SIGNING_RULES_PROPERTY} of {} declares no list of signing rules",
                resource.resource_type
            ),
        })?;
    let mut configuration = Map::new();
    configuration.insert(
        SIGNING_RULES_MEMBER.to_string(),
        Value::Array(rules.iter().map(declared_rule).collect()),
    );
    Ok(Value::Object(configuration))
}

/// One declared signing rule as the `SigningRule` the put takes: its scalar members under the
/// names that shape spells ([`SIGNING_RULE_MEMBERS`]), and its repository filters each mapped
/// the same way.
fn declared_rule(rule: &Value) -> Value {
    let mut mapped = mapped_members(rule, SIGNING_RULE_MEMBERS);
    if let (Some(Value::Array(filters)), Some(members)) = (
        rule.get(SIGNING_RULE_FILTERS_PROPERTY),
        mapped.as_object_mut(),
    ) {
        members.insert(
            SIGNING_RULE_FILTERS_MEMBER.to_string(),
            Value::Array(
                filters
                    .iter()
                    .map(|filter| mapped_members(filter, REPOSITORY_FILTER_MEMBERS))
                    .collect(),
            ),
        );
    }
    mapped
}

/// One REPORTED signing rule under the property names the type declares it with — the inverse
/// of [`declared_rule`], over the same tables.
fn reported_rule(rule: &Value) -> Value {
    let mut reported = reported_as_properties(rule, SIGNING_RULE_MEMBERS);
    if let (Some(Value::Array(filters)), Some(properties)) = (
        rule.get(SIGNING_RULE_FILTERS_MEMBER),
        reported.as_object_mut(),
    ) {
        properties.insert(
            SIGNING_RULE_FILTERS_PROPERTY.to_string(),
            Value::Array(
                filters
                    .iter()
                    .map(|filter| reported_as_properties(filter, REPOSITORY_FILTER_MEMBERS))
                    .collect(),
            ),
        );
    }
    reported
}
