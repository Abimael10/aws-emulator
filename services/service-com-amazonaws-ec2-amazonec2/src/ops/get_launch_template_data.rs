// SPDX-License-Identifier: CC0-1.0

//! @op GetLaunchTemplateData
//!
//! Implemented: reports the LAUNCH PARAMETERS one instance of the calling account is
//! running under, as the modelled `ResponseLaunchTemplateData` — "retrieves the
//! configuration data of the specified instance. You can use this data to create a launch
//! template". Nothing is invented and nothing is echoed from the request: every member is
//! read off the stored instance the request names, through the ONE projection of a stored
//! instance ([`crate::state::ComAmazonawsEc2Amazonec2State::instance_view`]) and the stores
//! behind the describes this operation's own doc says it calls on ("this action calls on
//! other describe actions to get instance information … DescribeSpotInstanceRequests,
//! DescribeInstanceCreditSpecifications, DescribeVolumes, and DescribeInstanceAttribute"),
//! so this report and those describes cannot disagree about one instance.
//!
//! An id outside the form EC2 issues is `InvalidInstanceID.Malformed` and an id inside that
//! form which names no instance of the account is `InvalidInstanceID.NotFound` (both from
//! [`crate::state::require_instance_record`], which is where that form is read), answered
//! before `DryRun`, so a dry run reports whether the request WOULD have succeeded.
//!
//! This operation REPORTS; it writes nothing at all. Two calls about one instance answer
//! the same thing, and a read can never change what a later caller sees.
//!
//! ## Where each member is read from
//!
//! * the AMI, instance type, key pair, kernel, RAM disk, EBS optimization, placement, CPU
//!   layout, metadata, enclave, hibernation, maintenance, hostname, network-performance and
//!   Capacity Reservation options: the instance's own projection, which is what
//!   `DescribeInstances` answers with;
//! * `BlockDeviceMappings`: the volumes the instance HOLDS, each entry's EBS settings read
//!   off that volume in the volume store (`DescribeVolumes`), so the mapping names the size,
//!   type, IOPS, throughput, encryption, key and snapshot the volume really has;
//! * `NetworkInterfaces`: the interfaces attached to the instance, with the groups,
//!   addresses and prefixes they carry — which is where a VPC instance's security groups
//!   live, so `SecurityGroupIds`/`SecurityGroups` are NOT reported beside them: "if you
//!   specify a network interface, you must specify any security groups as part of the
//!   network interface instead of using this parameter"
//!   (`RunInstancesRequest$SecurityGroupIds`);
//! * `Monitoring`, whose `Enabled` means DETAILED monitoring, read off the instance's
//!   monitoring state through the one reading of it
//!   ([`crate::state::instance_telemetry::is_detailed_monitoring`]);
//! * `UserData`, `DisableApiTermination`, `DisableApiStop` and
//!   `InstanceInitiatedShutdownBehavior`: the instance attributes `DescribeInstanceAttribute`
//!   reports, read off the same record it reads;
//! * `CreditSpecification`: the credit option for CPU usage of a BURSTABLE PERFORMANCE
//!   instance ([`crate::state::instance_cpu_credits`]). An instance of any other type has no
//!   credit option — "if you specify an instance ID that is not a burstable performance
//!   instance, Amazon EC2 returns the standard credit option"
//!   (`DescribeInstanceCreditSpecifications`) — and a launch template that named one for
//!   such a type could not launch, so the member is reported only for a burstable type;
//! * `InstanceMarketOptions`: the Spot Instance request the instance was launched to fulfill,
//!   read off the request store (`DescribeSpotInstanceRequests`) — its maximum price, request
//!   type, interruption behaviour and validity. An on-demand instance fulfills no request and
//!   reports no market option;
//! * `TagSpecifications`: the tags the instance carries, under the `instance` resource type,
//!   so a launch from this data tags the instance it creates the way this one is tagged.
//!
//! A member no state of this instance answers is left out rather than defaulted:
//! `ElasticGpuSpecifications` ("Amazon Elastic Graphics reached end of life") and
//! `ElasticInferenceAccelerators` ("Amazon Elastic Inference is no longer available") are
//! attached to no instance here; `LicenseSpecifications` is empty for an instance no license
//! configuration is associated with, which is every instance here; `InstanceRequirements` is
//! attribute-based instance-type selection, which an instance already placed on a concrete
//! type states nothing about; `SecondaryInterfaces` is empty for an instance that holds none
//! (`Instance$SecondaryInterfaces`); and `Operator` is "the entity that manages the launch
//! template", of which there is none — this data was derived from an instance, not read back
//! off a stored template.
//!
//! ## Shape of an operation
//!
//! Each operation is a struct that owns the shared engine and implements
//! [`smithy_server_runtime::Operation`]. The framework constructs one per service via
//! `GetLaunchTemplateData::new(engine)` and drives it through the centralized request walk:
//! deserialize → validate → `handle` → serialize. You write ONLY the `handle` body.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request — so
//! engine access is guaranteed by the time any request runs. Reach the store through
//! [`crate::state::service_state`] and read and write it through the guard it hands back:
//!
//! ```ignore
//!     let state = crate::state::service_state(&*self.engine)
//!         .ok_or(/* a modelled server error */)?;
//!     state.lock().your_field = /* ... */;
//! ```
//!
//! `ctx` ([`smithy_server_runtime::OperationContext`]) carries request metadata only —
//! headers, URI, method, and anything middleware placed in `ctx.parts().extensions`.
//! The engine is NOT reached through `ctx`.
//!
//! @generated by the emulator core scaffold op-stub generator.
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `GetLaunchTemplateData`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json")
//! ```
//!
//! Retrieves the configuration data of the specified instance. You can use this data to
//! create a launch template. This action calls on other describe actions to get instance
//! information. Depending on your instance configuration, you may need to allow the
//! following actions in your IAM policy: DescribeSpotInstanceRequests,
//! DescribeInstanceCreditSpecifications, DescribeVolumes, and DescribeInstanceAttribute.
//! Or, you can allow describe* depending on your instance requirements.
//!
//! ## Input
//!
//! * `DryRun` (Boolean) — Checks whether you have the required permissions for the action,
//!   without actually making the request, and provides an error response. If you have the
//!   required permissions, the error response is DryRunOperation. Otherwise, it is
//!   UnauthorizedOperation.
//! * `InstanceId` (InstanceId) **required** — The ID of the instance.
//!
//! ## Output
//!
//! * `LaunchTemplateData` (ResponseLaunchTemplateData) — The instance data.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `GetLaunchTemplateDataError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-ec2-amazonec2.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `GetLaunchTemplateDataError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.ec2#GetLaunchTemplateData")]
#[derive(Debug)]
pub enum GetLaunchTemplateDataErrorModeled {
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::get_launch_template_data::GetLaunchTemplateDataError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type GetLaunchTemplateDataError = crate::scaffolded::Scaffolded<GetLaunchTemplateDataErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<GetLaunchTemplateDataErrorModeled> for GetLaunchTemplateDataError {
    fn from(modeled: GetLaunchTemplateDataErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `MarketType` enum's `spot` member — the market an instance launched to fulfill a Spot
/// Instance request was purchased on, and the only member of that catalog
/// ([`MARKET_TYPE_VALUES`]) any launch path of this crate reaches.
const MARKET_TYPE_SPOT: &str = "spot";

/// The device index of an instance's PRIMARY network interface, the one a launch's
/// auto-assigned public IPv4 address is attached to: "the position of the network interface in
/// the attachment order" (`InstanceNetworkInterfaceAttachment$DeviceIndex`), which is 0 for
/// the interface a launch creates.
const PRIMARY_DEVICE_INDEX: i32 = 0;

/// The launch parameters of one stored instance, as `ResponseLaunchTemplateData`.
///
/// Reads the instance through the one projection of a stored instance, so this report and
/// `DescribeInstances` cannot disagree about one instance, and reads the few settings that
/// projection carries no member for — the instance attributes, the credit option and the
/// Spot Instance request — off the same stores their own describes read. The module docs
/// name every member's source and say why each unreported one has no answer here.
fn launch_template_data_of(
    store: &crate::state::ComAmazonawsEc2Amazonec2State,
    instance: &crate::state::InstanceRecord,
) -> ResponseLaunchTemplateData {
    let view = store.instance_view(instance);

    // The volumes the instance holds, each entry's EBS settings read off the volume store.
    // An instance that holds none (a launch still `pending`, whose root volume is attached as
    // part of that launch completing) has no mapping to report rather than an empty one.
    let block_device_mappings = view
        .block_device_mappings
        .as_deref()
        .map(|mappings| {
            mappings
                .iter()
                .map(|mapping| launch_template_block_device_mapping(store, mapping))
                .collect::<Vec<LaunchTemplateBlockDeviceMapping>>()
        })
        .filter(|mappings| !mappings.is_empty());

    // The interfaces attached to the instance, in the order the instance reports them.
    let network_interfaces = view
        .network_interfaces
        .as_deref()
        .map(|interfaces| {
            interfaces
                .iter()
                .map(|interface| launch_template_network_interface(instance, interface))
                .collect::<Vec<LaunchTemplateInstanceNetworkInterfaceSpecification>>()
        })
        .filter(|interfaces| !interfaces.is_empty());

    // "The tags that are applied to the resources that are created during instance launch"
    // (`ResponseLaunchTemplateData$TagSpecifications`): the tags this instance carries, under
    // the `instance` resource type, so a launch from this data tags the instance it creates
    // the way this one is tagged. An untagged instance names no resource to tag.
    let tag_specifications = view
        .tags
        .as_deref()
        .filter(|tags| !tags.is_empty())
        .map(|tags| {
            vec![LaunchTemplateTagSpecification {
                resource_type: Some(crate::state::RESOURCE_TYPE_INSTANCE.to_string()),
                tags: Some(tags.to_vec()),
            }]
        });

    // The credit option for CPU usage, reported only for an instance of a burstable
    // performance type — see the module docs.
    let credit_specification = crate::state::burstable_instance_family(&instance.instance_type)
        .map(|_| CreditSpecification {
            cpu_credits: Some(crate::state::instance_cpu_credits(instance).to_string()),
        });

    // The market option the instance was purchased under: the Spot Instance request it was
    // launched to fulfill, read off the request store.
    let instance_market_options = instance
        .spot_instance_request_id
        .as_deref()
        .and_then(|request_id| store.spot_instance_requests.get(request_id))
        .map(|record| LaunchTemplateInstanceMarketOptions {
            market_type: Some(MARKET_TYPE_SPOT.to_string()),
            spot_options: Some(LaunchTemplateSpotMarketOptions {
                // "The maximum price per unit hour that you are willing to pay for a Spot
                // Instance" (`SpotInstanceRequest$SpotPrice`) is the `MaxPrice` a launch
                // template's Spot options carry.
                max_price: record.view.spot_price.clone(),
                spot_instance_type: record.view.r#type.clone(),
                block_duration_minutes: record.view.block_duration_minutes,
                valid_until: record.view.valid_until,
                instance_interruption_behavior: record
                    .view
                    .instance_interruption_behavior
                    .clone(),
            }),
        });

    ResponseLaunchTemplateData {
        kernel_id: view.kernel_id,
        ebs_optimized: view.ebs_optimized,
        // The profile the instance runs as, by ARN: an association holds the ARN of the
        // profile it attached, and the launch template specification names a profile by ARN
        // or by name, so the ARN is what this instance answers with.
        iam_instance_profile: view.iam_instance_profile.map(|profile| {
            LaunchTemplateIamInstanceProfileSpecification {
                arn: profile.arn,
                name: None,
            }
        }),
        block_device_mappings,
        network_interfaces,
        image_id: view.image_id,
        instance_type: view.instance_type,
        key_name: view.key_name,
        // "Specify `true` to enable detailed monitoring" (`LaunchTemplatesMonitoring$Enabled`):
        // the instance's monitoring state read through the one reading of it, so an instance
        // on its way to or from detailed monitoring is reported the same way everywhere.
        monitoring: Some(LaunchTemplatesMonitoring {
            enabled: Some(crate::state::instance_telemetry::is_detailed_monitoring(
                &instance.monitoring_state,
            )),
        }),
        placement: view.placement.map(|placement| LaunchTemplatePlacement {
            availability_zone: placement.availability_zone,
            availability_zone_id: placement.availability_zone_id,
            affinity: placement.affinity,
            group_name: placement.group_name,
            host_id: placement.host_id,
            tenancy: placement.tenancy,
            spread_domain: placement.spread_domain,
            host_resource_group_arn: placement.host_resource_group_arn,
            partition_number: placement.partition_number,
            group_id: placement.group_id,
        }),
        ram_disk_id: view.ramdisk_id,
        disable_api_termination: Some(instance.disable_api_termination),
        instance_initiated_shutdown_behavior: Some(
            instance.detail.instance_initiated_shutdown_behavior.to_string(),
        ),
        // The user data the launch supplied, exactly as it is stored and exactly as
        // `DescribeInstanceAttribute` reports it, and nothing for an instance whose launch
        // supplied none.
        user_data: instance.user_data.clone(),
        tag_specifications,
        elastic_gpu_specifications: None,
        elastic_inference_accelerators: None,
        security_group_ids: None,
        security_groups: None,
        instance_market_options,
        credit_specification,
        cpu_options: view.cpu_options.map(|options| LaunchTemplateCpuOptions {
            core_count: options.core_count,
            threads_per_core: options.threads_per_core,
            amd_sev_snp: options.amd_sev_snp,
            nested_virtualization: options.nested_virtualization,
        }),
        capacity_reservation_specification: view.capacity_reservation_specification.map(
            |specification| LaunchTemplateCapacityReservationSpecificationResponse {
                capacity_reservation_preference: specification.capacity_reservation_preference,
                capacity_reservation_target: specification.capacity_reservation_target,
            },
        ),
        license_specifications: None,
        hibernation_options: view
            .hibernation_options
            .map(|options| LaunchTemplateHibernationOptions {
                configured: options.configured,
            }),
        metadata_options: view
            .metadata_options
            .map(|options| LaunchTemplateInstanceMetadataOptions {
                // "The state of the metadata option changes"
                // (`LaunchTemplateInstanceMetadataOptions$State`): the state the INSTANCE's
                // own options are in, which is what this data was read from.
                state: options.state,
                http_tokens: options.http_tokens,
                http_put_response_hop_limit: options.http_put_response_hop_limit,
                http_endpoint: options.http_endpoint,
                http_protocol_ipv6: options.http_protocol_ipv6,
                instance_metadata_tags: options.instance_metadata_tags,
            }),
        enclave_options: view
            .enclave_options
            .map(|options| LaunchTemplateEnclaveOptions {
                enabled: options.enabled,
            }),
        instance_requirements: None,
        private_dns_name_options: view.private_dns_name_options.map(|options| {
            LaunchTemplatePrivateDnsNameOptions {
                hostname_type: options.hostname_type,
                enable_resource_name_dns_a_record: options.enable_resource_name_dns_a_record,
                enable_resource_name_dns_aaaa_record: options.enable_resource_name_dns_aaaa_record,
            }
        }),
        // The launch template's maintenance options carry the auto-recovery setting alone;
        // `RebootMigration` is an instance-only member ("this setting only applies to
        // supported instances that have a scheduled reboot event") with no member on this
        // shape to report it under.
        maintenance_options: view.maintenance_options.map(|options| {
            LaunchTemplateInstanceMaintenanceOptions {
                auto_recovery: options.auto_recovery,
            }
        }),
        disable_api_stop: Some(instance.detail.disable_api_stop),
        operator: None,
        network_performance_options: view.network_performance_options.map(|options| {
            LaunchTemplateNetworkPerformanceOptions {
                bandwidth_weighting: options.bandwidth_weighting,
            }
        }),
        secondary_interfaces: None,
    }
}

/// One block device mapping of the instance as launch data: the device it is attached at,
/// with the EBS settings of the volume attached there read off the volume store.
///
/// The instance's own mapping carries the attachment facts (the device, whether the volume is
/// deleted on termination, which card it is on); the volume carries what a launch template's
/// mapping has to name to create the same volume again — its size, type, IOPS, throughput,
/// encryption, key and the snapshot it was created from. A volume the store no longer holds
/// contributes none of those rather than invented ones.
fn launch_template_block_device_mapping(
    store: &crate::state::ComAmazonawsEc2Amazonec2State,
    mapping: &InstanceBlockDeviceMapping,
) -> LaunchTemplateBlockDeviceMapping {
    let attached = mapping.ebs.as_ref();
    let volume = attached
        .and_then(|ebs| ebs.volume_id.as_deref())
        .and_then(|volume_id| store.volumes.get(volume_id))
        .map(|record| &record.view);
    LaunchTemplateBlockDeviceMapping {
        device_name: mapping.device_name.clone(),
        // "The virtual device name (ephemeralN)" (`LaunchTemplateBlockDeviceMapping$VirtualName`)
        // names an INSTANCE STORE volume, and `NoDevice` suppresses a device the AMI maps.
        // Neither is a fact of a volume the instance holds: every entry here IS a device the
        // instance holds, and no instance store volume is attached to one.
        virtual_name: None,
        ebs: attached.map(|ebs| LaunchTemplateEbsBlockDevice {
            encrypted: volume.and_then(|volume| volume.encrypted),
            delete_on_termination: ebs.delete_on_termination,
            iops: volume.and_then(|volume| volume.iops),
            kms_key_id: volume.and_then(|volume| volume.kms_key_id.clone()),
            snapshot_id: volume.and_then(|volume| volume.snapshot_id.clone()),
            volume_size: volume.and_then(|volume| volume.size),
            volume_type: volume.and_then(|volume| volume.volume_type.clone()),
            throughput: volume.and_then(|volume| volume.throughput),
            volume_initialization_rate: volume.and_then(|volume| volume.volume_initialization_rate),
            ebs_card_index: ebs.ebs_card_index,
        }),
        no_device: None,
    }
}

/// One attached network interface of the instance as launch data.
///
/// The interface itself answers for the subnet, the groups, the addresses and the prefixes it
/// carries; its ATTACHMENT answers for the device index, the network card and whether the
/// interface is deleted when the instance terminates. A count member
/// (`Ipv6AddressCount`, `SecondaryPrivateIpAddressCount`, `Ipv4PrefixCount`,
/// `Ipv6PrefixCount`) asks a launch to pick that many addresses for itself, so it is left out
/// where the addresses THEMSELVES are reported: a launch from this data gets the same
/// addresses rather than that many new ones.
fn launch_template_network_interface(
    instance: &crate::state::InstanceRecord,
    interface: &InstanceNetworkInterface,
) -> LaunchTemplateInstanceNetworkInterfaceSpecification {
    let attachment = interface.attachment.as_ref();
    let device_index = attachment.and_then(|attachment| attachment.device_index);
    LaunchTemplateInstanceNetworkInterfaceSpecification {
        // "Indicates whether to associate a Carrier IP address with eth0"
        // (`LaunchTemplateInstanceNetworkInterfaceSpecification$AssociateCarrierIpAddress`):
        // a Carrier IP belongs to a Wavelength Zone launch, which nothing here makes.
        associate_carrier_ip_address: None,
        // Whether AMAZON maps a public IPv4 address to this instance — the launch asked for
        // one, or the subnet it is in assigns one — which applies to the interface at device
        // index 0, the one a launch creates. A further interface something attached carries
        // no such setting of its own.
        associate_public_ip_address: (device_index == Some(PRIMARY_DEVICE_INDEX))
            .then_some(instance.mapped_public_ipv4),
        delete_on_termination: attachment.and_then(|attachment| attachment.delete_on_termination),
        description: interface.description.clone(),
        device_index,
        // "The IDs of one or more security groups": the groups the interface carries, which is
        // where a VPC instance's groups live.
        groups: interface.groups.as_ref().map(|groups| {
            groups
                .iter()
                .filter_map(|group| group.group_id.clone())
                .collect()
        }),
        interface_type: interface.interface_type.clone(),
        ipv6_address_count: None,
        ipv6_addresses: interface
            .ipv6_addresses
            .as_deref()
            .filter(|addresses| !addresses.is_empty())
            .map(<[InstanceIpv6Address]>::to_vec),
        network_interface_id: interface.network_interface_id.clone(),
        private_ip_address: interface.private_ip_address.clone(),
        private_ip_addresses: interface
            .private_ip_addresses
            .as_deref()
            .filter(|addresses| !addresses.is_empty())
            .map(|addresses| {
                addresses
                    .iter()
                    .map(|address| PrivateIpAddressSpecification {
                        primary: address.primary,
                        private_ip_address: address.private_ip_address.clone(),
                    })
                    .collect()
            }),
        secondary_private_ip_address_count: None,
        subnet_id: interface.subnet_id.clone(),
        network_card_index: attachment.and_then(|attachment| attachment.network_card_index),
        ipv4_prefixes: interface
            .ipv4_prefixes
            .as_deref()
            .filter(|prefixes| !prefixes.is_empty())
            .map(|prefixes| {
                prefixes
                    .iter()
                    .map(|prefix| Ipv4PrefixSpecificationResponse {
                        ipv4_prefix: prefix.ipv4_prefix.clone(),
                    })
                    .collect()
            }),
        ipv4_prefix_count: None,
        ipv6_prefixes: interface
            .ipv6_prefixes
            .as_deref()
            .filter(|prefixes| !prefixes.is_empty())
            .map(|prefixes| {
                prefixes
                    .iter()
                    .map(|prefix| Ipv6PrefixSpecificationResponse {
                        ipv6_prefix: prefix.ipv6_prefix.clone(),
                    })
                    .collect()
            }),
        ipv6_prefix_count: None,
        // "The primary IPv6 address of the network interface"
        // (`…$PrimaryIpv6`): reported only where one of the interface's own IPv6 addresses is
        // marked as that address.
        primary_ipv6: interface
            .ipv6_addresses
            .iter()
            .flatten()
            .any(|address| address.is_primary_ipv6 == Some(true))
            .then_some(true),
        ena_srd_specification: attachment
            .and_then(|attachment| attachment.ena_srd_specification.as_ref())
            .map(|ena_srd| LaunchTemplateEnaSrdSpecification {
                ena_srd_enabled: ena_srd.ena_srd_enabled,
                ena_srd_udp_specification: ena_srd.ena_srd_udp_specification.as_ref().map(|udp| {
                    LaunchTemplateEnaSrdUdpSpecification {
                        ena_srd_udp_enabled: udp.ena_srd_udp_enabled,
                    }
                }),
            }),
        connection_tracking_specification: interface
            .connection_tracking_configuration
            .as_ref()
            .map(|tracking| ConnectionTrackingSpecification {
                tcp_established_timeout: tracking.tcp_established_timeout,
                udp_timeout: tracking.udp_timeout,
                udp_stream_timeout: tracking.udp_stream_timeout,
            }),
        ena_queue_count: attachment.and_then(|attachment| attachment.ena_queue_count),
    }
}

/// The `GetLaunchTemplateData` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct GetLaunchTemplateData<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> GetLaunchTemplateData<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for GetLaunchTemplateData<Eng>
{
    type Input = GetLaunchTemplateDataInput;
    type Output = GetLaunchTemplateDataOutput;
    type Error = GetLaunchTemplateDataError;

    const NAME: &'static str = "GetLaunchTemplateData";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_LAUNCH_TEMPLATE_DATA_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_LAUNCH_TEMPLATE_DATA_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        use crate::scaffolded::Scaffolded;
        use crate::state::{require_instance_record, require_service_state};

        // Nothing about this answer depends on the request's Region or headers: the launch
        // parameters of an instance are facts of that instance.
        let _ = ctx;

        let state = require_service_state(&*self.engine)?;
        // A SHARED read: this operation reports and never writes, so it takes the guard
        // only to read the instance and the stores its data is derived from through it.
        let store = state.lock();

        // A malformed id before an unknown one, the order EC2 answers the two in.
        let instance = require_instance_record(&store, &input.instance_id)?;

        // DryRun sits AFTER the instance has been resolved: the flag asks whether the
        // request WOULD have succeeded, so one naming an instance that does not exist is
        // refused with that reason instead.
        if input.dry_run == Some(true) {
            return Err(Scaffolded::dry_run_operation());
        }

        Ok(GetLaunchTemplateDataOutput {
            launch_template_data: Some(launch_template_data_of(&store, instance)),
        })
    }
}
