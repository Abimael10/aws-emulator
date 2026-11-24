// SPDX-License-Identifier: CC0-1.0

//! `AWS::ECR::PullThroughCacheRule`: the pull through cache rule a stack wires, registered in
//! the private Amazon ECR registry itself.
//!
//! The type's properties are `CreatePullThroughCacheRule`'s own members under the names the
//! resource reference publishes, so the declaration reaches that registry as written:
//! `EcrRepositoryPrefix` is the prefix the rule caches under — "Ref returns the
//! `EcrRepositoryPrefix`", which makes it this resource's physical id — `UpstreamRegistryUrl`
//! and `UpstreamRegistry` name the registry it caches FROM, `UpstreamRepositoryPrefix` the
//! namespace it caches beneath there, and `CredentialArn`/`CustomRoleArn` what the upstream is
//! reached as.
//!
//! ## Why the rule is really registered
//!
//! A pull through cache rule is not a record about a registry: it is what makes a reference
//! under its prefix RESOLVE. "A pull through cache rule provides a way to cache images from an
//! upstream registry source in your Amazon ECR private registry"
//! (`CreatePullThroughCacheRule`), and the first pull of a reference under the prefix is what
//! creates the cache repository and stores the image. A stack that reported this resource
//! created while nothing registered the rule leaves the registry with no rule at all: every
//! pull of the cached image a later resource of the same stack runs — the container of a task
//! definition whose image is the cached URI — then resolves to no repository and no rule, so
//! the task never starts and nothing the stack deployed on top of it converges. So the
//! deployment makes the call, and the stack fails the resource when that registry refuses it
//! (a prefix a rule already covers, an upstream it does not support, a secret it cannot read).
//!
//! It is reached through the same seam every other per-type provider is ([`super::Owner`]).

use serde_json::{Map, Value};

use super::{call_json_rpc, Created, Readiness, Refusal, Removal, AWS_JSON_1_1};
use crate::state::StoredResource;

/// The resource type this module provisions, as a template spells it.
pub(crate) const ECR_PULL_THROUGH_CACHE_RULE: &str = "AWS::ECR::PullThroughCacheRule";

/// Amazon ECR's signing name and `X-Amz-Target` prefix, from its model (`sigv4Name: ecr`,
/// service shape `AmazonEC2ContainerRegistry_V20150921`), which speaks `awsJson1_1`.
const ECR_SIGNING_NAME: &str = "ecr";
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// The error code Amazon ECR answers for a rule it does not hold
/// (`PullThroughCacheRuleNotFoundException`, the modelled refusal of
/// `DeletePullThroughCacheRule`, `UpdatePullThroughCacheRule` and
/// `DescribePullThroughCacheRules`), which is what says a removal has nothing left to take off
/// and a read nothing to report.
const RULE_NOT_FOUND: &str = "PullThroughCacheRuleNotFoundException";

/// The code a refusal of this module's own carries for a declaration it cannot apply, and the
/// one for a declaration that leaves out a property the type requires: the same two the other
/// per-type providers use.
const NOT_IMPLEMENTED: &str = "NotImplemented";
const INVALID_REQUEST: &str = "InvalidRequest";

/// The property naming the prefix the rule caches under, which is
/// `CreatePullThroughCacheRule`'s own `ecrRepositoryPrefix` member and this resource's
/// physical id.
const ECR_REPOSITORY_PREFIX: &str = "EcrRepositoryPrefix";

/// The member that prefix travels under, and the one a call addresses a rule by.
const ECR_REPOSITORY_PREFIX_MEMBER: &str = "ecrRepositoryPrefix";

/// The property naming the registry the rule caches FROM, which the type requires.
const UPSTREAM_REGISTRY_URL: &str = "UpstreamRegistryUrl";

/// Every property `AWS::ECR::PullThroughCacheRule` publishes, each against the member
/// `CreatePullThroughCacheRule` takes it as.
///
/// Source: AWS's CloudFormation resource reference for the type, property by property against
/// `CreatePullThroughCacheRuleRequest` (`smithy_query(query="operation",
/// operation="CreatePullThroughCacheRule")`) — every one of them is that call's own member
/// under the same name but for its first letter. A declared property outside this list is
/// REFUSED rather than dropped, so a rule is never reported registered with less than its
/// declaration asked for.
const RULE_MEMBERS: &[(&str, &str)] = &[
    ("CredentialArn", "credentialArn"),
    ("CustomRoleArn", "customRoleArn"),
    (ECR_REPOSITORY_PREFIX, ECR_REPOSITORY_PREFIX_MEMBER),
    ("UpstreamRegistry", "upstreamRegistry"),
    (UPSTREAM_REGISTRY_URL, "upstreamRegistryUrl"),
    ("UpstreamRepositoryPrefix", "upstreamRepositoryPrefix"),
];

/// The properties a rule can take a NEW VALUE for while it goes on existing, each against the
/// member `UpdatePullThroughCacheRule` takes it as: that call takes "the ARN of the Secrets
/// Manager secret" and the custom role and NOTHING else, so those two are the type's
/// no-interruption properties and every other one requires a replacement
/// (`crate::state::template::replacement_properties`).
const RULE_IN_PLACE_MEMBERS: &[(&str, &str)] = &[
    ("CredentialArn", "credentialArn"),
    ("CustomRoleArn", "customRoleArn"),
];

/// Register the rule an `AWS::ECR::PullThroughCacheRule` declares, in Amazon ECR itself — so a
/// pull of a reference under its prefix really resolves to the upstream image the rule caches.
///
/// `Err` is that registry's own refusal of the call: a prefix a rule already covers, an
/// upstream registry it does not support, or a credential secret it cannot read.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let request = rule_request(&resource.properties, resource)?;
    call_ecr(ctx, region, "CreatePullThroughCacheRule", &request).await?;
    // The rule was registered under the prefix CloudFormation decided, and the type publishes
    // no `Fn::GetAtt` attribute, so there is nothing of the answer to adopt.
    Ok(Created::default())
}

/// Apply a CHANGED declaration to the rule the stack already registered, through
/// `UpdatePullThroughCacheRule` — the one call that changes a rule that exists.
///
/// `Ok(None)` is a change that call cannot make: a declaration that states the upstream, its
/// repository prefix or the registry differently asks for a DIFFERENT rule, which the
/// algorithm above replaces rather than bringing here, and a property this module cannot apply
/// is reported as not applied rather than as a success that dropped the caller's change.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    refuse_unapplied_properties(declared, &resource.resource_type)?;
    let mut request = Map::new();
    request.insert(
        ECR_REPOSITORY_PREFIX_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    for (property, member) in RULE_IN_PLACE_MEMBERS {
        if let Some(value) = stated(declared, property) {
            request.insert((*member).to_string(), value.clone());
        }
    }
    // Every other property is create-only, so a declaration that changed one never reaches
    // here; what is left is a declaration whose in-place properties this call carries.
    call_ecr(
        ctx,
        region,
        "UpdatePullThroughCacheRule",
        &Value::Object(request),
    )
    .await?;
    // The rule keeps its prefix, which is its identity and its physical id, so this reports no
    // attribute of its own.
    Ok(Some(Created::default()))
}

/// `CreatePullThroughCacheRule` has registered the rule as it answers — a pull under the
/// prefix resolves through it from then on — so there is nothing for a later read to wait on.
pub(super) fn readiness() -> Readiness {
    Readiness::Ready
}

/// `DeletePullThroughCacheRule` takes the rule off as it answers, so there is nothing for a
/// later read to wait on either.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// Delete the rule a stack registered, addressed by the prefix it was registered under.
///
/// A rule that registry no longer holds is already in the state the removal asks for; judged
/// on the CODE and never on a message. The cache repositories a pull through the rule created
/// are repositories of their own and are not touched, exactly as `DeletePullThroughCacheRule`
/// leaves them.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    // A resource whose create never answered holds no prefix, so there is no rule to address.
    if resource.physical_resource_id.is_empty() {
        return Ok(());
    }
    let request = serde_json::json!({
        ECR_REPOSITORY_PREFIX_MEMBER: resource.physical_resource_id,
    });
    match call_ecr(ctx, region, "DeletePullThroughCacheRule", &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == RULE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The registered rule as Amazon ECR reports it, for drift detection:
/// `DescribePullThroughCacheRules` narrowed to this rule's own prefix, with each member it
/// reports read back under the property that declared it ([`RULE_MEMBERS`]).
///
/// A rule that registry no longer holds, and one it reports nothing for, reports nothing here:
/// a member a read does not report is not evidence of anything.
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    if resource.physical_resource_id.is_empty() {
        return Ok(None);
    }
    let request = serde_json::json!({
        "ecrRepositoryPrefixes": [resource.physical_resource_id],
    });
    let answered = match call_ecr(ctx, region, "DescribePullThroughCacheRules", &request).await {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == RULE_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let Some(held) = answered
        .get("pullThroughCacheRules")
        .and_then(Value::as_array)
        .and_then(|held| held.first())
    else {
        return Ok(None);
    };
    let mut configuration = Map::new();
    for (property, member) in RULE_MEMBERS {
        if let Some(value) = held.get(*member).filter(|value| !value.is_null()) {
            configuration.insert((*property).to_string(), value.clone());
        }
    }
    Ok(Some(Value::Object(configuration)))
}

/// The declaration as `CreatePullThroughCacheRule`'s own request: every property the type
/// publishes under the member that call takes it as, with the prefix the resource's physical
/// id is rather than the one the declaration spelled — those are the same value, resolved
/// through the template's intrinsics on the way to the physical id.
fn rule_request(properties: &Value, resource: &StoredResource) -> Result<Value, Refusal> {
    refuse_unapplied_properties(properties, &resource.resource_type)?;
    let mut request = Map::new();
    for (property, member) in RULE_MEMBERS {
        if let Some(value) = stated(properties, property) {
            request.insert((*member).to_string(), value.clone());
        }
    }
    // "Ref returns the EcrRepositoryPrefix": the prefix the rule is registered under is this
    // resource's physical id, which is what every later call on the rule addresses it by.
    request.insert(
        ECR_REPOSITORY_PREFIX_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    if resource.physical_resource_id.is_empty() {
        return Err(required(ECR_REPOSITORY_PREFIX));
    }
    if !request.contains_key("upstreamRegistryUrl") {
        return Err(required(UPSTREAM_REGISTRY_URL));
    }
    Ok(Value::Object(request))
}

/// The refusal for a declaration that leaves out a property the type requires.
fn required(property: &str) -> Refusal {
    Refusal {
        code: INVALID_REQUEST.to_string(),
        message: format!("{ECR_PULL_THROUGH_CACHE_RULE} requires the {property} property"),
    }
}

/// The value a declaration STATES for `property`, or `None` for one it leaves out or states as
/// null.
fn stated<'a>(properties: &'a Value, property: &str) -> Option<&'a Value> {
    properties.get(property).filter(|value| !value.is_null())
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was: refusing is what keeps a rule from being reported as registered with less than its
/// declaration asked for.
fn refuse_unapplied_properties(properties: &Value, resource_type: &str) -> Result<(), Refusal> {
    let Value::Object(properties) = properties else {
        return Ok(());
    };
    for property in properties.keys() {
        if RULE_MEMBERS
            .iter()
            .any(|(published, _)| published == property)
        {
            continue;
        }
        return Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {resource_type}"
            ),
        });
    }
    Ok(())
}

/// Call one Amazon ECR operation on the awsJson1_1 wire that registry's model declares.
async fn call_ecr(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        ECR_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ECR_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One resource record carrying `properties` under the physical id `prefix`.
    fn resource(prefix: &str, properties: Value) -> StoredResource {
        StoredResource {
            logical_id: "CacheRule".to_string(),
            resource_type: ECR_PULL_THROUGH_CACHE_RULE.to_string(),
            physical_resource_id: prefix.to_string(),
            attributes: ::std::collections::BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// RULE: every property the type publishes reaches `CreatePullThroughCacheRule` under that
    /// call's own member name, with the prefix taken from the resource's physical id.
    #[test]
    fn the_declared_properties_are_carried_under_the_api_member_names() {
        // (case, the declared properties, the members the create must carry)
        let rows: &[(&str, Value, Value)] = &[
            (
                "the two members the type requires",
                serde_json::json!({
                    "EcrRepositoryPrefix": "mirror",
                    "UpstreamRegistryUrl": "public.ecr.aws",
                }),
                serde_json::json!({
                    "ecrRepositoryPrefix": "mirror",
                    "upstreamRegistryUrl": "public.ecr.aws",
                }),
            ),
            (
                "an upstream namespace and the registry the rule names",
                serde_json::json!({
                    "EcrRepositoryPrefix": "cached/docker",
                    "UpstreamRegistryUrl": "registry-1.docker.io",
                    "UpstreamRegistry": "docker-hub",
                    "UpstreamRepositoryPrefix": "library",
                    "CredentialArn":
                        "arn:aws:secretsmanager:us-east-1:000000000000:secret:ecr-pullthroughcache/hub",
                }),
                serde_json::json!({
                    "ecrRepositoryPrefix": "cached/docker",
                    "upstreamRegistryUrl": "registry-1.docker.io",
                    "upstreamRegistry": "docker-hub",
                    "upstreamRepositoryPrefix": "library",
                    "credentialArn":
                        "arn:aws:secretsmanager:us-east-1:000000000000:secret:ecr-pullthroughcache/hub",
                }),
            ),
        ];
        for (case, properties, expected) in rows {
            let prefix = properties[ECR_REPOSITORY_PREFIX]
                .as_str()
                .expect("every row declares its prefix");
            let built = rule_request(properties, &resource(prefix, properties.clone()));
            assert_eq!(
                built.as_ref().ok(),
                Some(expected),
                "case {case:?}: expected the create to carry {expected}, got {built:?}"
            );
        }
    }

    /// RULE: a declaration this module cannot apply in full is refused rather than deployed
    /// with less than it asked for.
    #[test]
    fn a_declaration_this_module_cannot_apply_is_refused() {
        // (case, the physical id, the declared properties)
        let rows: &[(&str, &str, Value)] = &[
            (
                "a property the type does not publish",
                "mirror",
                serde_json::json!({
                    "EcrRepositoryPrefix": "mirror",
                    "UpstreamRegistryUrl": "public.ecr.aws",
                    "Elsewhere": "x",
                }),
            ),
            (
                "no upstream registry at all",
                "mirror",
                serde_json::json!({ "EcrRepositoryPrefix": "mirror" }),
            ),
            (
                "no prefix to register the rule under",
                "",
                serde_json::json!({ "UpstreamRegistryUrl": "public.ecr.aws" }),
            ),
        ];
        for (case, prefix, properties) in rows {
            let built = rule_request(properties, &resource(prefix, properties.clone()));
            assert!(
                built.is_err(),
                "case {case:?}: expected a refusal naming the property, got {built:?}"
            );
        }
    }
}
