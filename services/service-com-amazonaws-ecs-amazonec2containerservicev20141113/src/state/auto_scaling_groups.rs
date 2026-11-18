// SPDX-License-Identifier: CC0-1.0

//! The Auto Scaling group a capacity provider BINDS onto, resolved against Amazon EC2
//! Auto Scaling's own records.
//!
//! `AutoScalingGroupProvider$autoScalingGroupArn` is "The Amazon Resource Name (ARN) that
//! identifies the Auto Scaling group, or the Auto Scaling group name" — a reference to a
//! group of the CALLER's, which Amazon EC2 Auto Scaling holds and this service only points
//! at. A provider is the binding onto the capacity that group runs, and managed scaling
//! drives that group's desired capacity, so a reference naming a group the account does not
//! hold binds onto no capacity at all and is refused.
//!
//! The member is named for what it REPORTS: a provider's binding is reported as the group's
//! ARN whichever of the two forms created it, so a reference naming the group by its bare
//! name is resolved to the ARN that service minted for it before the provider is stored
//! ([`auto_scaling_group_binding`]) — one value for one group, which is what a client
//! reading the provider back resolves the group by.
//!
//! The groups of the account are the Auto Scaling service's own records, so they are read
//! out of that service's store through the one shared engine rather than copied here — the
//! same cross-service read the Amazon VPC networking of a placement is resolved through
//! ([`unresolved_subnet_refusal`]).

use ::service_com_amazonaws_autoscaling_autoscaling_2011_01_01::ops::types::Tag as AutoScalingTag;
use ::service_com_amazonaws_autoscaling_autoscaling_2011_01_01::state as auto_scaling;

use super::*;

/// `ClientException$message` for an `autoScalingGroupProvider` whose reference names no
/// Auto Scaling group of the account.
///
/// A fixed sentence: it states what was not found and what to send instead, and echoes
/// neither the reference the request carried nor the groups that do exist.
pub const AUTO_SCALING_GROUP_NOT_FOUND_MESSAGE: &str =
    "The specified Auto Scaling group does not exist. Specify a valid Auto Scaling group \
     and try again.";

/// The tag key Amazon ECS writes onto the Auto Scaling group a capacity provider binds.
///
/// Creating a capacity provider over a group makes that group Amazon ECS's to manage — its
/// managed scaling drives the group's desired capacity and its managed termination
/// protection decides which instances may be scaled in — and this tag is what says so on the
/// group itself: it is the condition the Amazon ECS service-linked role's policy is scoped
/// by, so a group without it is one that role may not manage. A group a caller creates for a
/// capacity provider by hand carries the same key ("add the `AmazonECSManaged` tag to the
/// Auto Scaling group", Amazon ECS capacity providers), which is why the KEY is the marker
/// and the value carries nothing.
pub const MANAGED_GROUP_TAG_KEY: &str = "AmazonECSManaged";

/// The value that marker carries: none. The key is the whole of the statement.
pub const MANAGED_GROUP_TAG_VALUE: &str = "";

/// Whether the marker is added to the instances the group launches: it is, so an instance
/// the group brings up carries the same statement the group does ("set the `PropagateAtLaunch`
/// value to `true`", same page).
pub const MANAGED_GROUP_TAG_PROPAGATES: bool = true;

/// The Auto Scaling action writing that tag performs, as the IAM Service Authorization
/// Reference names it — what the caller of a create that binds a group must be permitted.
pub const AUTO_SCALING_TAG_ACTION: &str = "autoscaling:CreateOrUpdateTags";

/// `AccessDeniedException$message` for a create whose caller may not write that tag.
///
/// Entirely fixed text: it states which permission the call needed and what it was needed
/// for, and quotes no part of the request.
pub const AUTO_SCALING_TAG_DENIED_MESSAGE: &str =
    "The capacity provider could not be created because you do not have \
     autoscaling:CreateOrUpdateTags permissions to create tags on the Auto Scaling group.";

/// The refusal a create that binds the Auto Scaling group `group_arn` earns when the IAM
/// policies in force do not let its caller write that group's tags, or `None` when they do.
///
/// Part of creating the provider is TAGGING the group ([`mark_group_managed`]), so the call
/// needs the Auto Scaling action that write performs on the group it writes to, and a caller
/// without it is refused the whole create rather than served a provider whose group was never
/// marked. The decision is the shared one the caller's own policies are judged by
/// ([`crate::service::authorization::permits_action`]), taken against the group's ARN because
/// that is the resource the write addresses — and taken BEFORE anything is stored, so a
/// refused call leaves the account exactly as it was.
pub fn auto_scaling_tag_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    group_arn: &str,
) -> Option<&'static str> {
    (!crate::service::authorization::permits_action(
        engine,
        ctx,
        AUTO_SCALING_TAG_ACTION,
        group_arn,
    ))
    .then_some(AUTO_SCALING_TAG_DENIED_MESSAGE)
}

/// Write the managed marker ([`MANAGED_GROUP_TAG_KEY`]) onto the Auto Scaling group
/// `group_arn` names, in Amazon EC2 Auto Scaling's OWN store.
///
/// The tag belongs to the group, not to the provider, so it is written where the group lives:
/// `DescribeAutoScalingGroups` and that service's own tag reads then report it beside the
/// caller's own tags, through that service's one tag writer, which decides the members a
/// stored tag carries. The group is addressed by NAME, because a
/// reference naming one group by ARN and one by its bare name names the same group
/// ([`auto_scaling_group_key`]).
///
/// LOCK ORDER: takes the Auto Scaling store's lock and holds no other while it does, so it is
/// called with NO guard of this service's store held — the same contract
/// [`auto_scaling_group_binding`] has. Writing a tag twice writes it once (a key the group
/// already carries is overwritten in place), so a repeat of the create that already had its
/// effect leaves the group as it stands. An engine carrying no Auto Scaling store holds no
/// group to mark.
///
/// ONE key, Amazon ECS's own: the write adds a tag only to a group that does not already
/// carry the marker, so it grows the group by at most that one key — the per-call and
/// per-group tag bounds a CALLER's tag write is held to are about the tags a caller asks for.
pub fn mark_group_managed<Eng: Send + Sync + 'static>(engine: &Eng, group_arn: &str) {
    let Some(state) = auto_scaling::service_state(engine) else {
        return;
    };
    let name = auto_scaling_group_key(group_arn);
    let mut store = state.lock();
    if let Some(group) = store.groups.get_mut(name) {
        // The three members a stored group tag carries beyond the caller's own — which
        // resource it is on, what kind of resource that is — are that service's own
        // decision, which its writer fills in.
        group.put_tag(AutoScalingTag {
            resource_id: None,
            resource_type: None,
            key: MANAGED_GROUP_TAG_KEY.to_string(),
            value: Some(MANAGED_GROUP_TAG_VALUE.to_string()),
            propagate_at_launch: Some(MANAGED_GROUP_TAG_PROPAGATES),
        });
    }
}

/// The `autoScalingGroupProvider` a capacity provider is STORED with: the one the request
/// carried, with its reference resolved to the ARN Amazon EC2 Auto Scaling minted for the
/// group it names — and `Err` when that service holds no such group in this Region, which is
/// the refusal the provider earns ([`AUTO_SCALING_GROUP_NOT_FOUND_MESSAGE`]).
///
/// `Ok(None)` is a request that names no group at all (a managed-instances provider), which
/// has nothing to resolve.
///
/// The group is resolved by NAME ([`auto_scaling_group_key`]), because naming one group by
/// ARN and by its bare name names the same group — the same reading the reuse rule compares
/// two providers on. The lookup is made in the Region the request was signed for: a group's
/// instances run in the Region it was created in, so a reference that names a group of
/// another Region names none of this one.
///
/// LOCK ORDER: this takes the Auto Scaling store's lock, holds no other while it does, and
/// hands back owned values — so it is called with NO guard of this service's store held,
/// which is what keeps a lock cycle between the two stores unreachable. The same contract
/// [`unresolved_subnet_refusal`] has.
///
/// An engine carrying no Auto Scaling store at all refuses nothing and resolves nothing: a
/// refusal has to be grounded in what the account's Auto Scaling state answered, never
/// assumed — the same reading every cross-service resolution in this crate is written to —
/// and so does the ARN a provider reports, so the reference is left exactly as the request
/// spelled it.
pub fn auto_scaling_group_binding<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    provider: Option<&AutoScalingGroupProvider>,
) -> Result<Option<AutoScalingGroupProvider>, &'static str> {
    let Some(provider) = provider else {
        return Ok(None);
    };
    let name = auto_scaling_group_key(&provider.auto_scaling_group_arn).to_string();
    let Some(auto_scaling) =
        ::service_com_amazonaws_autoscaling_autoscaling_2011_01_01::state::service_state(engine)
    else {
        return Ok(Some(provider.clone()));
    };
    let held = {
        let store = auto_scaling.lock();
        store
            .groups
            .values()
            .find(|group| group.name == name && group.region == region)
            .map(|group| group.arn.clone())
    };
    let Some(arn) = held else {
        return Err(AUTO_SCALING_GROUP_NOT_FOUND_MESSAGE);
    };
    let mut bound = provider.clone();
    bound.auto_scaling_group_arn = arn;
    Ok(Some(bound))
}
