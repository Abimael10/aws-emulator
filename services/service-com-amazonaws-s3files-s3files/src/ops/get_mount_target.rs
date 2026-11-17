// SPDX-License-Identifier: CC0-1.0

//! @op GetMountTarget
//!
//! Reports one stored mount target. A reporting operation: the only state it touches is
//! the transition the clock already owed, and it answers through the same projection the
//! create that stored the record answered with
//! ([`crate::state::MountTargetView`]), so a read cannot report a mount target
//! differently from the create. A mount target this service does not hold — including one
//! a `DeleteMountTarget` has already removed — is the modelled 404.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `GetMountTarget`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-s3files-s3files/smithy-model/com-amazonaws-s3files-s3files.json")
//! ```
//!
//! Returns detailed resource information for the specified mount target including network
//! configuration.
//!
//! ## Input
//!
//! * `mountTargetId` (MountTargetId, len 22..45, pattern ^fsmt-[0-9a-f]{17,40}$) **required**
//!   — The ID of the mount target to retrieve information for.
//!
//! ## Output
//!
//! * `availabilityZoneId` (AvailabilityZoneId) — The Availability Zone ID where the mount
//!   target is located.
//! * `fileSystemId` (FileSystemId, len ..128, pattern
//!   ^(arn:aws[-a-z]*:s3files:[0-9a-z-:]+:file-system/fs-[0-9a-f]{17,40}|fs-[0-9a-f]{17,40})$)
//!   — The ID of the file system.
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
// `GetMountTargetError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-s3files-s3files.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `GetMountTargetError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.s3files#GetMountTarget")]
#[derive(Debug)]
pub enum GetMountTargetErrorModeled {
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::get_mount_target::GetMountTargetError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type GetMountTargetError = crate::scaffolded::Scaffolded<GetMountTargetErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<GetMountTargetErrorModeled> for GetMountTargetError {
    fn from(modeled: GetMountTargetErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `GetMountTarget` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct GetMountTarget<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> GetMountTarget<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for GetMountTarget<Eng>
{
    type Input = GetMountTargetInput;
    type Output = GetMountTargetOutput;
    type Error = GetMountTargetError;

    const NAME: &'static str = "GetMountTarget";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_MOUNT_TARGET_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_MOUNT_TARGET_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let _ = ctx;
        let now = ::std::time::SystemTime::now();

        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            GetMountTargetErrorModeled::InternalServerException(crate::state::store_unavailable())
        })?;
        let mut store = state.lock();
        // The only write a reporting operation makes: the transition the clock already
        // owed at this instant, whether or not anybody called.
        store.settle(now);

        let stored = store.mount_targets.get(&input.mount_target_id).ok_or_else(|| {
            GetMountTargetErrorModeled::ResourceNotFoundException(crate::state::not_found(
                crate::state::MOUNT_TARGET_NOT_FOUND_MESSAGE,
            ))
        })?;

        Ok(crate::state::MountTargetView::from(stored).into())
    }
}
