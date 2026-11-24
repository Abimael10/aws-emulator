// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arm of the PRIVATE Amazon ECR registry's PULL-TIME-UPDATE
//! EXCLUSION LIST: the one IAM principal an `AWS::ECR::PullTimeUpdateExclusion` declares.
//! One concern of [`super`]'s Amazon ECR arms, dispatched to by the `Provisioner` of
//! [`super::super`].
//!
//! The resource IS the principal it excludes. `RegisterPullTimeUpdateExclusion` ("Adds an
//! IAM principal to the pull time update exclusion list for a registry. Amazon ECR will not
//! record the pull time if an excluded principal pulls an image", required input member
//! `principalArn`) takes that ARN and no identifier of its own,
//! `DeregisterPullTimeUpdateExclusion` addresses the record by the same ARN, and
//! `ListPullTimeUpdateExclusions` reports the ARNs the registry holds and nothing else about
//! one — so the type's one property is the whole request, the ARN is the resource's physical
//! id ([`crate::state::template::name_property_candidates`] reads it off the declaration) and
//! that is what the type's `Ref` returns.
//!
//! Amazon ECR is also what JUDGES the declaration: an ARN outside the pattern that member
//! declares is its `InvalidParameterException`, a principal the registry already excludes its
//! `ExclusionAlreadyExistsException`, and a registry already holding its full list its
//! `LimitExceededException` — so a declaration that registry refuses fails the resource with
//! that service's own refusal, reported through the handler-error class the code falls in
//! ([`crate::state::provisioning::handler_error_code`]), rather than being recorded as an
//! exclusion nothing holds.
//!
//! There is no UPDATE arm: the exclusion is the principal it names, and that principal is the
//! resource's own identity, so a declaration naming another principal replaces the resource
//! ([`crate::state::template::replacing_properties`]) instead of changing one.

use super::*;

/// The type's ONE property, as AWS's resource reference spells it, and the member every one
/// of these calls takes it as on the wire (Amazon ECR's own `principalArn`).
const EXCLUDED_PRINCIPAL_PROPERTY: &str = "PrincipalArn";
const PRINCIPAL_ARN_MEMBER: &str = "principalArn";

/// The operations this module calls, as Amazon ECR's own model names them.
const REGISTER_EXCLUSION: &str = "RegisterPullTimeUpdateExclusion";
const DEREGISTER_EXCLUSION: &str = "DeregisterPullTimeUpdateExclusion";
const LIST_EXCLUSIONS: &str = "ListPullTimeUpdateExclusions";

/// The error code Amazon ECR answers for an exclusion the registry does not hold
/// (`ExclusionNotFoundException`, the modelled 400 of the deregister: "The specified pull
/// time update exclusion was not found"): the delete reads it as the state the delete asks
/// for.
const EXCLUSION_NOT_FOUND: &str = "ExclusionNotFoundException";

/// The member `ListPullTimeUpdateExclusions` answers the registry's exclusions in, and the
/// member carrying the token of the page after it — which the same call takes back under that
/// one spelling.
const REPORTED_EXCLUSIONS: &str = "pullTimeUpdateExclusions";
const EXCLUSION_PAGE_TOKEN: &str = "nextToken";

/// How many pages of the registry's exclusion list one read follows.
///
/// The list is BOUNDED: `ListPullTimeUpdateExclusions`'s `pullTimeUpdateExclusions` member
/// targets `PullTimeUpdateExclusionList`, declared `@length(min: 0, max: 100)`
/// (`smithy_query(query="shape", shape="PullTimeUpdateExclusionList")`), and a page that
/// carries a token carries at least one entry — so following this many pages reads every
/// exclusion a registry can hold, and the walk cannot run away on an answer that keeps
/// handing back a token.
const MAX_EXCLUSION_PAGES: usize = 100;

/// Register the IAM principal an `AWS::ECR::PullTimeUpdateExclusion` declares on the private
/// registry's exclusion list, so a client's own `ListPullTimeUpdateExclusions` reports the
/// principal the stack published and Amazon ECR really stops recording its pull times.
///
/// The declared `PrincipalArn` IS the request: it travels as the one member that call takes,
/// under the name that member carries on the wire. The identity is that same ARN, which the
/// naming rules already computed for the resource, so nothing here replaces it, and the type
/// publishes no `Fn::GetAtt` attribute — the principal is the whole of what the stack
/// publishes for the resource.
pub(crate) async fn register_pull_time_update_exclusion(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let mut request = Map::new();
    request.insert(
        PRINCIPAL_ARN_MEMBER.to_string(),
        Value::String(required_property(resource, EXCLUDED_PRINCIPAL_PROPERTY)?),
    );
    call_ecr(ctx, region, REGISTER_EXCLUSION, &Value::Object(request)).await?;
    Ok(Created::attributes(BTreeMap::new()))
}

/// Take the exclusion a stack registered off the registry again, by the principal ARN it was
/// registered under — this resource's physical id, which is the one handle
/// `DeregisterPullTimeUpdateExclusion` takes.
///
/// An exclusion Amazon ECR no longer holds (`ExclusionNotFoundException`, judged on the CODE
/// and never on a message that carries the request's own values) is already in the state the
/// delete asks for.
pub(crate) async fn deregister_pull_time_update_exclusion(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert(
        PRINCIPAL_ARN_MEMBER.to_string(),
        Value::String(physical_resource_id.to_string()),
    );
    match call_ecr(ctx, region, DEREGISTER_EXCLUSION, &Value::Object(request)).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == EXCLUSION_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The exclusion's configuration as Amazon ECR reports it, for drift detection: the declared
/// `PrincipalArn` where the registry still excludes that principal, and `None` where the
/// record is gone.
///
/// Amazon ECR publishes no read of ONE exclusion — the registry's exclusions are reported as
/// the LIST of principal ARNs `ListPullTimeUpdateExclusions` answers, which is the only read
/// of them that service has — so the record is read out of that listing. The ARN is the whole
/// of what the registry holds about an exclusion, so a principal the listing names reports
/// exactly the property the declaration states.
pub(crate) async fn describe_pull_time_update_exclusion(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    if !registry_excludes(ctx, region, physical_resource_id).await? {
        return Ok(None);
    }
    let mut properties = Map::new();
    properties.insert(
        EXCLUDED_PRINCIPAL_PROPERTY.to_string(),
        Value::String(physical_resource_id.to_string()),
    );
    Ok(Some(Value::Object(properties)))
}

/// Whether the registry's exclusion list still names `principal_arn`, read across the pages
/// `ListPullTimeUpdateExclusions` answers it in ([`MAX_EXCLUSION_PAGES`]).
async fn registry_excludes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    principal_arn: &str,
) -> Result<bool, Refusal> {
    let mut after: Option<String> = None;
    for _ in 0..MAX_EXCLUSION_PAGES {
        let mut request = Map::new();
        if let Some(token) = &after {
            request.insert(
                EXCLUSION_PAGE_TOKEN.to_string(),
                Value::String(token.clone()),
            );
        }
        let reported = call_ecr(ctx, region, LIST_EXCLUSIONS, &Value::Object(request)).await?;
        if let Some(Value::Array(listed)) = reported.get(REPORTED_EXCLUSIONS) {
            if listed
                .iter()
                .filter_map(Value::as_str)
                .any(|excluded| excluded == principal_arn)
            {
                return Ok(true);
            }
        }
        after = reported
            .get(EXCLUSION_PAGE_TOKEN)
            .and_then(Value::as_str)
            .map(str::to_string);
        if after.is_none() {
            break;
        }
    }
    Ok(false)
}
