// SPDX-License-Identifier: CC0-1.0

//! @op CreateMountTarget
//!
//! Places a mount target for an existing file system in the subnet the request names and
//! persists it in [`crate::state`], answering from the stored record so what this returns
//! is exactly what `GetMountTarget` and `ListMountTargets` read back.
//!
//! The subnet is a resource of Amazon EC2, so the VPC, the Availability Zone and the
//! address range the mount target reports are resolved against EC2 — the service that
//! owns them — before anything is stored ([`crate::state::resolve_subnet`]); a subnet the
//! account does not have is the modelled 404. A file system holds at most one mount target
//! per Availability Zone, so a second create in a zone it already has one in is the
//! modelled 409. A new mount target is reported `creating` and the clock owes it
//! `available` ([`crate::state::PROVISIONING_SPAN`]).
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `CreateMountTarget`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-s3files-s3files/smithy-model/com-amazonaws-s3files-s3files.json")
//! ```
//!
//! Creates a mount target resource as an endpoint for mounting the S3 File System from
//! compute resources in a specific Availability Zone and VPC. Mount targets provide network
//! access to the file system.
//!
//! ## Input
//!
//! * `fileSystemId` (FileSystemId, len ..128, pattern
//!   ^(arn:aws[-a-z]*:s3files:[0-9a-z-:]+:file-system/fs-[0-9a-f]{17,40}|fs-[0-9a-f]{17,40})$)
//!   **required** — The ID or Amazon Resource Name (ARN) of the S3 File System to create the
//!   mount target for.
//! * `ipAddressType` (IpAddressType, one of: DUAL_STACK | IPV4_ONLY | IPV6_ONLY) — The IP
//!   address type for the mount target. If not specified, IPV4_ONLY is used. The IP address
//!   type must match the IP configuration of the specified subnet.
//! * `ipv4Address` (Ipv4Address, len 7..15, pattern
//!   ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$) — A specific IPv4 address to assign to
//!   the mount target. If not specified and the IP address type supports IPv4, an address is
//!   automatically assigned from the subnet's available IPv4 address range. The address must
//!   be within the subnet's CIDR block and not already in use.
//! * `ipv6Address` (Ipv6Address, len 3..39) — A specific IPv6 address to assign to the mount
//!   target. If not specified and the IP address type supports IPv6, an address is
//!   automatically assigned from the subnet's available IPv6 address range. The address must
//!   be within the subnet's IPv6 CIDR block and not already in use.
//! * `securityGroups` (SecurityGroups, len ..100) — An array of VPC security group IDs to
//!   associate with the mount target's network interface. These security groups control
//!   network access to the mount target. If not specified, the default security group for the
//!   subnet's VPC is used. All security groups must belong to the same VPC as the subnet.
//! * `subnetId` (SubnetId, len 15..47, pattern ^subnet-[0-9a-f]{8,40}$) **required** — The ID
//!   of the subnet where the mount target will be created. The subnet must be in the same
//!   Amazon Web Services Region as the file system. For file systems with regional
//!   availability, you can create mount targets in any subnet within the Region. The subnet
//!   determines the Availability Zone where the mount target will be located.
//!
//! ## Output
//!
//! * `availabilityZoneId` (AvailabilityZoneId) — The unique and consistent identifier of the
//!   Availability Zone where the mount target is located. For example, use1-az1 is an
//!   Availability Zone ID for the us-east-1 Amazon Web Services Region, and it has the same
//!   location in every Amazon Web Services account.
//! * `fileSystemId` (FileSystemId, len ..128, pattern
//!   ^(arn:aws[-a-z]*:s3files:[0-9a-z-:]+:file-system/fs-[0-9a-f]{17,40}|fs-[0-9a-f]{17,40})$)
//!   — The ID of the S3 File System associated with the mount target.
//! * `ipv4Address` (Ipv4Address, len 7..15, pattern
//!   ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$) — The IPv4 address assigned to the
//!   mount target.
//! * `ipv6Address` (Ipv6Address, len 3..39) — The IPv6 address assigned to the mount target.
//! * `mountTargetId` (MountTargetId, len 22..45, pattern ^fsmt-[0-9a-f]{17,40}$) **required**
//!   — The ID of the mount target, assigned by S3 Files. This ID is used to reference the
//!   mount target in subsequent API calls.
//! * `networkInterfaceId` (NetworkInterfaceId) — The ID of the network interface that S3
//!   Files created when it created the mount target. This network interface is managed by the
//!   service.
//! * `ownerId` (AwsAccountId, len ..12, pattern ^(\d{12})|(\d{4}-{4}-\d{4})$) **required** —
//!   The Amazon Web Services account ID of the mount target owner.
//! * `securityGroups` (SecurityGroups, len ..100) — The security groups associated with the
//!   mount target's network interface.
//! * `status` (LifeCycleState, one of: available | creating | deleted | deleting | error |
//!   misconfigured | updating) — The lifecycle state of the mount target. Valid values are:
//!   AVAILABLE (the mount target is available for use), CREATING (the mount target is being
//!   created), DELETING (the mount target is being deleted), DELETED (the mount target has
//!   been deleted), or ERROR (the mount target is in an error state), or UPDATING (the mount
//!   target is being updated).
//! * `statusMessage` (StatusMessage) — Additional information about the mount target status.
//!   This field provides more details when the status is ERROR, or during state transitions.
//! * `subnetId` (SubnetId, len 15..47, pattern ^subnet-[0-9a-f]{8,40}$) **required** — The ID
//!   of the subnet where the mount target is located.
//! * `vpcId` (VpcId) — The ID of the VPC where the mount target is located.
//!
//! ## Errors
//!
//! * `ConflictException` (HTTP 409) — The request conflicts with the current state of the
//!   resource. This can occur when trying to create a resource that already exists or delete
//!   a resource that is in use.
//! * `InternalServerException` (HTTP 500) — An internal server error occurred. Retry your
//!   request.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource was not found. Verify
//!   that the resource exists and that you have permission to access it.
//! * `ServiceQuotaExceededException` (HTTP 402) — The request would exceed a service quota.
//!   Review your service quotas and either delete resources or request a quota increase.
//! * `ValidationException` (HTTP 400) — The input parameters are not valid. Check the
//!   parameter values and try again.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `CreateMountTargetError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-s3files-s3files.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `CreateMountTargetError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.s3files#CreateMountTarget")]
#[derive(Debug)]
pub enum CreateMountTargetErrorModeled {
    ConflictException(ConflictException),
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceQuotaExceededException(ServiceQuotaExceededException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::create_mount_target::CreateMountTargetError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type CreateMountTargetError = crate::scaffolded::Scaffolded<CreateMountTargetErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<CreateMountTargetErrorModeled> for CreateMountTargetError {
    fn from(modeled: CreateMountTargetErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `CreateMountTarget` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct CreateMountTarget<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> CreateMountTarget<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for CreateMountTarget<Eng>
{
    type Input = CreateMountTargetInput;
    type Output = CreateMountTargetOutput;
    type Error = CreateMountTargetError;

    const NAME: &'static str = "CreateMountTarget";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::CREATE_MOUNT_TARGET_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::CREATE_MOUNT_TARGET_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let now = ::std::time::SystemTime::now();
        let region = crate::state::request_region(ctx);

        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            CreateMountTargetErrorModeled::InternalServerException(
                crate::state::store_unavailable(),
            )
        })?;

        // `fileSystemId` accepts the bare id or the ARN; both name the same record.
        let file_system_id = crate::state::file_system_id_of(&input.file_system_id)
            .ok_or_else(|| {
                CreateMountTargetErrorModeled::ValidationException(crate::state::invalid(format!(
                    "fileSystemId must be a file system id (fs-<hex>) or its ARN, got {:?}",
                    input.file_system_id
                )))
            })?
            .to_string();

        // The parent is resolved before the network is asked about, so a request naming no
        // file system this service holds is refused without a hop. The guard is released
        // again at the end of this block: the questions below are put to another service,
        // and this store's lock is never held across them.
        {
            let mut store = state.lock();
            store.settle(now);
            if !store.file_systems.contains_key(&file_system_id) {
                return Err(CreateMountTargetErrorModeled::ResourceNotFoundException(
                    crate::state::not_found(crate::state::FILE_SYSTEM_NOT_FOUND_MESSAGE),
                )
                .into());
            }
        }

        // `subnetId` names a resource of ANOTHER service: the subnet "determines the
        // Availability Zone where the mount target will be located", its VPC and the
        // address range it is given one out of, so every one of those facts is read off
        // the record EC2 — the owner of subnets — answers with, before anything is
        // written. A subnet the account does not have cannot be placed in at all, which is
        // the modelled 404.
        let Some(placement) = crate::state::resolve_subnet(ctx, &region, &input.subnet_id).await
        else {
            return Err(CreateMountTargetErrorModeled::ResourceNotFoundException(
                crate::state::not_found(crate::state::SUBNET_NOT_FOUND_MESSAGE),
            )
            .into());
        };

        // "If not specified, the default security group for the subnet's VPC is used" —
        // which is a group of that VPC, so it too is asked of the service that owns it.
        let requested_groups = input.security_groups.unwrap_or_default();
        let security_groups = if requested_groups.is_empty() {
            crate::state::default_security_groups(ctx, &region, &placement.vpc_id).await
        } else {
            // "All security groups must belong to the same VPC as the subnet": a group
            // EC2 reports in another VPC is refused, and a group it reports nothing about
            // refuses nothing — a refusal has to be grounded in what the owner answered.
            let elsewhere = crate::state::security_groups_outside_vpc(
                ctx,
                &region,
                &placement.vpc_id,
                &requested_groups,
            )
            .await;
            if !elsewhere.is_empty() {
                return Err(CreateMountTargetErrorModeled::ValidationException(
                    crate::state::invalid(format!(
                        "every security group must belong to VPC {} of subnet {}, but {} \
                         belong(s) to another VPC: {}",
                        placement.vpc_id,
                        placement.subnet_id,
                        elsewhere.len(),
                        elsewhere.join(", ")
                    )),
                )
                .into());
            }
            requested_groups
        };

        // Every refusal that depends only on this service's own records, and the addresses
        // the request may ask the owner of the subnet for, decided under one short hold of
        // the guard — which is released again before the owner is asked anything.
        let (requested_ipv4, requested_ipv6, interface_description) = {
            let mut store = state.lock();
            store.settle(now);

            // Re-resolved under the guard this request writes through: the parent could
            // have been deleted while the network was being asked about, and a mount
            // target is never stored against a file system this store no longer holds. The
            // zone rule and the store's ceiling are asked here too, so nothing is placed
            // in EC2 for a request that is already refused.
            if let Some(refusal) = placement_rejection(&store, &file_system_id, &placement) {
                return Err(refusal);
            }

            // The addresses the request named, held to the rule both members document:
            // "The address must be within the subnet's CIDR block", and "The IP address
            // type must match the IP configuration of the specified subnet" — the subnet's
            // blocks being the ones EC2 reported for it. Whether an address is FREE is not
            // asked here: the owner of the subnet holds every address of it, so that is
            // its answer to give, below.
            let ip_address_type = input.ip_address_type.as_deref();
            let requested_ipv4 = requested_address(
                &placement,
                AddressFamily::V4,
                crate::state::wants_ipv4(ip_address_type),
                input.ipv4_address.as_deref(),
            )?;
            let requested_ipv6 = requested_address(
                &placement,
                AddressFamily::V6,
                crate::state::wants_ipv6(ip_address_type),
                input.ipv6_address.as_deref(),
            )?;
            (
                requested_ipv4,
                requested_ipv6,
                crate::state::mount_target_interface_description(&file_system_id),
            )
        };
        let wants_ipv6 = crate::state::wants_ipv6(input.ip_address_type.as_deref());
        let carries_ipv4 = crate::state::wants_ipv4(input.ip_address_type.as_deref());

        // The interface the mount target answers on is an EC2 resource, so it is CREATED
        // there and this service records only the identifier and addresses EC2 answered
        // with. The owner's verdict on the address the request named is what the caller is
        // answered with: an address something already holds is the modelled conflict, one
        // the subnet cannot assign at all the modelled 400.
        let interface = crate::state::create_mount_target_interface(
            ctx,
            &region,
            &placement.subnet_id,
            &interface_description,
            &security_groups,
            requested_ipv4.as_deref(),
            wants_ipv6.then(|| match requested_ipv6.clone() {
                Some(address) => crate::state::RequestedIpv6::Named(address),
                None => crate::state::RequestedIpv6::Assigned,
            }),
        )
        .await
        .map_err(|refusal| {
            interface_refusal(&placement, &requested_ipv4, &requested_ipv6, refusal)
        })?;

        // The record itself, written under one hold of the guard: the placement is asked
        // once more because the zone could have been taken, or the parent deleted, while
        // the interface was being placed.
        let stored = {
            let mut store = state.lock();
            store.settle(now);
            match placement_rejection(&store, &file_system_id, &placement) {
                Some(refusal) => Err(refusal),
                None => {
                    let mount_target_id = store.mint_mount_target_id();
                    let sequence = store.next_sequence();
                    let record = crate::state::MountTarget {
                        mount_target_id: mount_target_id.clone(),
                        file_system_id,
                        subnet_id: placement.subnet_id.clone(),
                        vpc_id: placement.vpc_id.clone(),
                        availability_zone_id: placement.availability_zone_id.clone(),
                        availability_zone_name: placement.availability_zone_name.clone(),
                        // The addresses and the interface id are the OWNER's, read off its
                        // own answer — and only for the families the request's
                        // `ipAddressType` carries, so a mount target never reports an
                        // address of a family its type excludes, whatever else the
                        // interface was given.
                        ipv4_address: carries_ipv4
                            .then(|| interface.ipv4_address.clone())
                            .flatten(),
                        ipv6_address: wants_ipv6.then(|| interface.ipv6_address.clone()).flatten(),
                        network_interface_id: interface.network_interface_id.clone(),
                        // The groups the interface SITS IN, as the owner reported them
                        // once it was created, so the member can never name a group the
                        // interface is not in.
                        security_groups: interface.security_groups.clone(),
                        owner_id: ::engine::emulator_account_id().to_string(),
                        // A new mount target is not usable yet: the clock owes it
                        // `available`.
                        status: crate::state::STATUS_CREATING.to_string(),
                        status_message: None,
                        available_due_at: now + crate::state::PROVISIONING_SPAN,
                        sequence,
                    };
                    store.mount_targets.insert(mount_target_id.clone(), record);
                    // Answered from the stored record, so the response cannot drift from
                    // what a later GetMountTarget / ListMountTargets reads.
                    store
                        .mount_targets
                        .get(&mount_target_id)
                        // ONE projection for every operation that reports a mount target
                        // (`crate::state::MountTargetView`).
                        .map(crate::state::MountTargetView::from)
                        .ok_or_else(|| {
                            CreateMountTargetErrorModeled::InternalServerException(
                                crate::state::store_unavailable(),
                            )
                            .into()
                        })
                }
            }
        };

        match stored {
            Ok(view) => Ok(view.into()),
            Err(refusal) => {
                // The interface was placed for a mount target that is not going to exist,
                // so it is released again rather than left holding an address of the
                // caller's subnet.
                crate::state::release_mount_target_interface(
                    ctx,
                    &region,
                    &interface.network_interface_id,
                )
                .await;
                Err(refusal)
            }
        }
    }
}

/// Why this store cannot hold a mount target for `file_system_id` in `placement` right now,
/// or `None` when it can.
///
/// The refusals that depend only on this service's own records, asked under its guard:
/// the parent must be held, the file system must have no mount target in that Availability
/// Zone, and the store must have room. Asked BEFORE anything is placed in EC2 and again
/// before the record is written, so neither a refused create nor a racing one leaves an
/// interface behind.
fn placement_rejection(
    store: &crate::state::ComAmazonawsS3filesS3filesState,
    file_system_id: &str,
    placement: &crate::state::SubnetPlacement,
) -> Option<CreateMountTargetError> {
    if !store.file_systems.contains_key(file_system_id) {
        return Some(
            CreateMountTargetErrorModeled::ResourceNotFoundException(crate::state::not_found(
                crate::state::FILE_SYSTEM_NOT_FOUND_MESSAGE,
            ))
            .into(),
        );
    }
    // A mount target is the endpoint every compute resource of ONE Availability Zone
    // mounts the file system through, so a file system has at most one per zone.
    if let Some(existing) =
        store.mount_target_in_zone(file_system_id, &placement.availability_zone_id)
    {
        return Some(
            CreateMountTargetErrorModeled::ConflictException(
                crate::state::mount_target_zone_in_use(existing),
            )
            .into(),
        );
    }
    // Caller-driven growth needs a ceiling, and this operation models the quota error for
    // refusing past one. See `state::MAX_MOUNT_TARGETS_PER_FILE_SYSTEM`.
    if store.mount_target_ids_of(file_system_id).len()
        >= crate::state::MAX_MOUNT_TARGETS_PER_FILE_SYSTEM
    {
        return Some(
            CreateMountTargetErrorModeled::ServiceQuotaExceededException(
                crate::state::quota_exceeded(format!(
                    "the number of mount targets on {file_system_id} would exceed the limit \
                     of {}",
                    crate::state::MAX_MOUNT_TARGETS_PER_FILE_SYSTEM
                )),
            )
            .into(),
        );
    }
    None
}

/// Which of a subnet's two address ranges an address is taken out of.
#[derive(Debug, Clone, Copy)]
enum AddressFamily {
    V4,
    V6,
}

impl AddressFamily {
    /// The member of `CreateMountTargetRequest` that names an address of this family, for
    /// the refusals below to name the member a caller sent.
    fn member(self) -> &'static str {
        match self {
            Self::V4 => "ipv4Address",
            Self::V6 => "ipv6Address",
        }
    }
}

/// The address of `family` the create asks the owner of the subnet to assign: the
/// `requested` one when the request named one, or `None` for the owner to choose.
///
/// Three rules, all the model's own, decide what may be asked for at all:
///
/// * `ipAddressType` "must match the IP configuration of the specified subnet", so a family
///   the subnet has no range for has no address to assign and the request is refused rather
///   than answered with a mount target carrying none;
/// * an address member the chosen `ipAddressType` does not carry is refused rather than
///   discarded — a request naming one asked for something it cannot be given;
/// * "The address must be within the subnet's CIDR block", the block being the one EC2
///   reported for that subnet.
///
/// Whether the address is already in use is NOT decided here: every address of the subnet
/// belongs to the owner of the subnet, so that is its answer to give
/// ([`crate::state::create_mount_target_interface`]).
fn requested_address(
    placement: &crate::state::SubnetPlacement,
    family: AddressFamily,
    wanted: bool,
    requested: Option<&str>,
) -> Result<Option<String>, CreateMountTargetError> {
    let member = family.member();
    let subnet_id = placement.subnet_id.as_str();
    let block = match family {
        AddressFamily::V4 => placement.cidr_block.as_deref(),
        AddressFamily::V6 => placement.ipv6_cidr_block.as_deref(),
    };
    if !wanted {
        if requested.is_some() {
            return Err(CreateMountTargetErrorModeled::ValidationException(
                crate::state::invalid(format!(
                    "{member} must not be named for ipAddressType {}, which carries no \
                     address of that kind",
                    ip_address_type_label(family)
                )),
            )
            .into());
        }
        return Ok(None);
    }
    let Some(block) = block else {
        return Err(CreateMountTargetErrorModeled::ValidationException(
            crate::state::invalid(format!(
                "ipAddressType must match the IP configuration of the subnet: {subnet_id} has \
                 no CIDR block {member} could be assigned from"
            )),
        )
        .into());
    };
    let Some(address) = requested else {
        return Ok(None);
    };
    let within = match family {
        AddressFamily::V4 => crate::state::ipv4_within(block, address),
        AddressFamily::V6 => crate::state::ipv6_within(block, address),
    };
    if !within {
        return Err(CreateMountTargetErrorModeled::ValidationException(
            crate::state::invalid(format!(
                "{member} must be within the CIDR block {block} of subnet {subnet_id}, got \
                 {address:?}"
            )),
        )
        .into());
    }
    Ok(Some(address.to_string()))
}

/// The `ipAddressType` values that carry no address of `family`, for the refusal of an
/// address member the chosen type cannot hold.
fn ip_address_type_label(family: AddressFamily) -> &'static str {
    match family {
        AddressFamily::V4 => crate::state::IP_ADDRESS_TYPE_IPV6_ONLY,
        AddressFamily::V6 => crate::state::IP_ADDRESS_TYPE_IPV4_ONLY,
    }
}

/// The modelled failure for the owner of network interfaces having placed none: its own
/// verdict, in this service's error channel.
///
/// An address something in the account already holds is the modelled 409 — the condition
/// `CreateMountTargetRequest$ipv4Address` states as "not already in use" — one the subnet
/// will not assign and a security group the account does not have are the modelled 400, a
/// subnet the owner no longer holds the modelled 404, and anything else is this emulator
/// failing to place the interface at all, which is the modelled 500 rather than a refusal of
/// the request.
fn interface_refusal(
    placement: &crate::state::SubnetPlacement,
    requested_ipv4: &Option<String>,
    requested_ipv6: &Option<String>,
    refusal: crate::state::InterfaceNotPlaced,
) -> CreateMountTargetError {
    let subnet_id = placement.subnet_id.as_str();
    let named = requested_ipv4.as_deref().or(requested_ipv6.as_deref());
    match refusal {
        crate::state::InterfaceNotPlaced::AddressInUse => {
            // The address the request named, when it named one; otherwise the one the
            // owner picked, which it did not report, so the sentence states the condition
            // without an empty slot in it.
            let message = match named {
                Some(address) => {
                    format!("The address {address} is already in use in subnet {subnet_id}.")
                }
                None => format!(
                    "The address assigned to the mount target in subnet {subnet_id} is \
                     already in use."
                ),
            };
            CreateMountTargetErrorModeled::ConflictException(crate::state::conflict(
                subnet_id,
                crate::state::RESOURCE_TYPE_MOUNT_TARGET,
                message,
            ))
            .into()
        }
        crate::state::InterfaceNotPlaced::AddressRefused => {
            CreateMountTargetErrorModeled::ValidationException(crate::state::invalid(format!(
                "the address {} is not one subnet {subnet_id} can assign to a mount target",
                named.unwrap_or("requested")
            )))
            .into()
        }
        crate::state::InterfaceNotPlaced::GroupRefused => {
            CreateMountTargetErrorModeled::ValidationException(crate::state::invalid(format!(
                "every security group must be a group of VPC {} that the account has",
                placement.vpc_id
            )))
            .into()
        }
        crate::state::InterfaceNotPlaced::SubnetMissing => {
            CreateMountTargetErrorModeled::ResourceNotFoundException(crate::state::not_found(
                crate::state::SUBNET_NOT_FOUND_MESSAGE,
            ))
            .into()
        }
        crate::state::InterfaceNotPlaced::Unavailable => {
            CreateMountTargetErrorModeled::InternalServerException(
                crate::state::interface_unavailable(subnet_id),
            )
            .into()
        }
    }
}
