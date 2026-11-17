// SPDX-License-Identifier: CC0-1.0

//! @op UpdateMountTarget
//!
//! Replaces the security groups of one stored mount target —
//! `UpdateMountTargetRequest$securityGroups` is "An array of VPC security group IDs to
//! associate with the mount target's network interface. This replaces the existing
//! security groups" — and answers through the same projection the create and every read
//! answer with ([`crate::state::MountTargetView`]), so the groups this returns are the
//! ones `GetMountTarget` reads back. A mount target this service does not hold is the
//! modelled 404.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `UpdateMountTarget`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-s3files-s3files/smithy-model/com-amazonaws-s3files-s3files.json")
//! ```
//!
//! Updates the mount target resource, specifically security group configurations.
//!
//! ## Input
//!
//! * `mountTargetId` (MountTargetId, len 22..45, pattern ^fsmt-[0-9a-f]{17,40}$) **required**
//!   — The ID of the mount target to update.
//! * `securityGroups` (SecurityGroups, len ..100) **required** — An array of VPC security
//!   group IDs to associate with the mount target's network interface. This replaces the
//!   existing security groups. All security groups must belong to the same VPC as the mount
//!   target's subnet.
//!
//! ## Output
//!
//! * `availabilityZoneId` (AvailabilityZoneId) — The Availability Zone ID where the mount
//!   target is located.
//! * `fileSystemId` (FileSystemId, len ..128, pattern
//!   ^(arn:aws[-a-z]*:s3files:[0-9a-z-:]+:file-system/fs-[0-9a-f]{17,40}|fs-[0-9a-f]{17,40})$)
//!   — The ID of the S3 File System.
//! * `ipv4Address` (Ipv4Address, len 7..15, pattern
//!   ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$) — The IPv4 address of the mount
//!   target.
//! * `ipv6Address` (Ipv6Address, len 3..39) — The IPv6 address of the mount target.
//! * `mountTargetId` (MountTargetId, len 22..45, pattern ^fsmt-[0-9a-f]{17,40}$) **required**
//!   — The ID of the mount target.
//! * `networkInterfaceId` (NetworkInterfaceId) — The ID of the network interface associated
//!   with the mount target.
//! * `ownerId` (AwsAccountId, len ..12, pattern ^(\d{12})|(\d{4}-{4}-\d{4})$) **required** —
//!   The Amazon Web Services account ID of the mount target owner.
//! * `securityGroups` (SecurityGroups, len ..100) — The security groups associated with the
//!   mount target.
//! * `status` (LifeCycleState, one of: available | creating | deleted | deleting | error |
//!   misconfigured | updating) — The current status of the mount target.
//! * `statusMessage` (StatusMessage) — Additional information about the mount target status.
//! * `subnetId` (SubnetId, len 15..47, pattern ^subnet-[0-9a-f]{8,40}$) **required** — The ID
//!   of the subnet where the mount target is located.
//! * `vpcId` (VpcId) — The ID of the VPC where the mount target is located.
//!
//! ## Errors
//!
//! * `InternalServerException` (HTTP 500) — An internal server error occurred. Retry your
//!   request.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource was not found. Verify
//!   that the resource exists and that you have permission to access it.
//! * `ValidationException` (HTTP 400) — The input parameters are not valid. Check the
//!   parameter values and try again.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `UpdateMountTargetError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-s3files-s3files.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `UpdateMountTargetError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.s3files#UpdateMountTarget")]
#[derive(Debug)]
pub enum UpdateMountTargetErrorModeled {
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::update_mount_target::UpdateMountTargetError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type UpdateMountTargetError = crate::scaffolded::Scaffolded<UpdateMountTargetErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<UpdateMountTargetErrorModeled> for UpdateMountTargetError {
    fn from(modeled: UpdateMountTargetErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `UpdateMountTarget` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct UpdateMountTarget<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> UpdateMountTarget<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for UpdateMountTarget<Eng>
{
    type Input = UpdateMountTargetInput;
    type Output = UpdateMountTargetOutput;
    type Error = UpdateMountTargetError;

    const NAME: &'static str = "UpdateMountTarget";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::UPDATE_MOUNT_TARGET_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::UPDATE_MOUNT_TARGET_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let now = ::std::time::SystemTime::now();

        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            UpdateMountTargetErrorModeled::InternalServerException(
                crate::state::store_unavailable(),
            )
        })?;

        // The VPC the mount target sits in and the interface the groups belong to, read
        // under a guard that is released again before another service is asked anything:
        // this store's lock is never held across a hop.
        let (vpc_id, network_interface_id) = {
            let mut store = state.lock();
            store.settle(now);
            let stored = store
                .mount_targets
                .get(&input.mount_target_id)
                .ok_or_else(|| {
                    UpdateMountTargetErrorModeled::ResourceNotFoundException(
                        crate::state::not_found(crate::state::MOUNT_TARGET_NOT_FOUND_MESSAGE),
                    )
                })?;
            (
                stored.vpc_id.clone(),
                stored.network_interface_id.clone(),
            )
        };
        let region = crate::state::request_region(ctx);

        // "All security groups must belong to the same VPC as the mount target's subnet":
        // a group's VPC is a fact of the group, which EC2 owns, so the refusal is grounded
        // in what EC2 answered — a group it reports nothing about refuses nothing.
        let elsewhere = crate::state::security_groups_outside_vpc(
            ctx,
            &region,
            &vpc_id,
            &input.security_groups,
        )
        .await;
        if !elsewhere.is_empty() {
            return Err(UpdateMountTargetErrorModeled::ValidationException(
                crate::state::invalid(format!(
                    "every security group must belong to VPC {vpc_id} of the mount target's \
                     subnet, but {} belong(s) to another VPC: {}",
                    elsewhere.len(),
                    elsewhere.join(", ")
                )),
            )
            .into());
        }

        // The groups are an attribute of the mount target's network interface, which is
        // EC2's resource, so the replacement is applied THERE before this service reports
        // it: what a read answers is then the set the interface really carries. A set the
        // owner did not apply is not one the mount target may report, so the request is
        // refused and the stored set is left as it was. An EMPTY set is one the owner will
        // not apply at all — "You must specify at least one group"
        // (`ModifyNetworkInterfaceAttribute$Groups`), because an interface always sits in
        // one — so it is refused here rather than recorded as a set the interface does not
        // have.
        if input.security_groups.is_empty() {
            return Err(UpdateMountTargetErrorModeled::ValidationException(
                crate::state::invalid(format!(
                    "securityGroups must name at least one security group of VPC {vpc_id}: \
                     the mount target's network interface always sits in one"
                )),
            )
            .into());
        }
        if !crate::state::apply_mount_target_interface_groups(
            ctx,
            &region,
            &network_interface_id,
            &input.security_groups,
        )
        .await
        {
            return Err(UpdateMountTargetErrorModeled::ValidationException(
                crate::state::invalid(format!(
                    "the mount target's network interface {network_interface_id} could not be \
                     put in the security groups the request names; every one of them must be \
                     a group of VPC {vpc_id}"
                )),
            )
            .into());
        }

        let mut store = state.lock();
        store.settle(now);

        let stored = store
            .mount_targets
            .get_mut(&input.mount_target_id)
            .ok_or_else(|| {
                UpdateMountTargetErrorModeled::ResourceNotFoundException(
                    crate::state::not_found(crate::state::MOUNT_TARGET_NOT_FOUND_MESSAGE),
                )
            })?;
        // The whole set is REPLACED, not merged: the request names the groups the
        // interface is to sit in, so a shorter list takes groups away.
        stored.security_groups = input.security_groups;

        // Answered from the record as it now stands, so the response cannot report groups
        // the store does not hold.
        Ok(crate::state::MountTargetView::from(&*stored).into())
    }
}
