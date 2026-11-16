// SPDX-License-Identifier: CC0-1.0

//! What a recording pass OBSERVES in Amazon EC2: the instances this account holds in the
//! Region the recorder records.
//!
//! Read through Amazon EC2's own API over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), exactly as the Amazon S3, Amazon
//! SQS and IAM observations beside it are (`super::recording`, `super::observed_iam`): this
//! crate names no other service crate, and a member Amazon EC2 does not report is not
//! recorded at all rather than recorded as something else.
//!
//! An instance exists for as long as Amazon EC2 reports it in a state other than
//! `terminated`: a terminated instance is gone, so it is not among the resources a pass
//! observes, and the inventory records its DELETION the way it records any resource the
//! pass no longer sees ([`super::ComAmazonawsConfigserviceStarlingdoveserviceState::record_observed`]).

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::DiscoveredResourceEntry;

/// The resource type Config records an Amazon EC2 instance under, a
/// `com.amazonaws.configservice#ResourceType` value.
pub const EC2_INSTANCE_RESOURCE_TYPE: &str = "AWS::EC2::Instance";

/// How many pages of instances one pass reads before it stops asking for more.
///
/// A HOST bound, like the one on the pages of queues a pass reads
/// ([`super::recording`]): the inventory a pass writes into is bounded
/// ([`super::DISCOVERED_RESOURCES_KEPT`]), so one pass reads at most the pages that can
/// fill it rather than following a token without end.
const MAX_INSTANCE_PAGES: usize = 20;

/// The configuration items the recorder observes for this Region's Amazon EC2 instances,
/// or `None` when Amazon EC2 could not be read in full — which is not the same as an
/// account with no instances.
///
/// Every member of the recorded configuration document comes from the Amazon EC2 read
/// named on it; nothing else is recorded, because nothing else is observed. An instance is
/// identified by the instance id Amazon EC2 minted for it, and it carries no custom name:
/// `Instance` has no name member, so the item records none rather than one derived from a
/// tag.
pub async fn observed_ec2_instances(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    now: SystemTime,
) -> Option<Vec<DiscoveredResourceEntry>> {
    let client = ec2_client(callback, region);

    // "Describes the specified instances or all instances" (`DescribeInstances`), which for
    // a client built for one Region are that Region's — the Region whose resources this
    // recorder records.
    //
    // The listing is PAGED (`DescribeInstancesResult$NextToken`), and a pass follows the
    // token so an account holding more instances than one page is observed in full. A
    // listing this pass could not read to its end says nothing about what the account
    // holds — every instance past the last page it read would look gone — so it is answered
    // as an unread listing rather than as a complete one.
    let mut reservations: Vec<::aws_sdk_ec2::types::Reservation> = Vec::new();
    let mut next_token: Option<String> = None;
    let mut read_in_full = false;
    for _ in 0..MAX_INSTANCE_PAGES {
        let page = match client
            .describe_instances()
            .set_next_token(next_token.clone())
            .send()
            .await
        {
            Ok(page) => page,
            Err(error) => {
                ::tracing::warn!(
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the configuration recorder could not read this account's Amazon EC2 \
                     instances"
                );
                return None;
            }
        };
        reservations.extend(page.reservations.unwrap_or_default());
        next_token = page.next_token;
        if next_token.is_none() {
            read_in_full = true;
            break;
        }
    }
    if !read_in_full {
        ::tracing::warn!(
            pages = MAX_INSTANCE_PAGES,
            "the configuration recorder stopped short of this account's whole Amazon EC2 \
             instance listing"
        );
        return None;
    }

    let mut observed = Vec::new();
    for reservation in &reservations {
        for instance in reservation.instances() {
            // An instance Amazon EC2 reported without the id that identifies it is not one
            // a configuration item could be recorded under.
            let Some(instance_id) = instance
                .instance_id
                .clone()
                .filter(|instance_id| !instance_id.is_empty())
            else {
                continue;
            };
            // A TERMINATED instance no longer exists ("the instance has been terminated",
            // `InstanceStateName`), so it is not among the resources this pass observed;
            // the inventory records the deletion of a resource a pass no longer sees.
            if instance
                .state
                .as_ref()
                .and_then(|state| state.name.as_ref())
                .is_some_and(|name| name == &::aws_sdk_ec2::types::InstanceStateName::Terminated)
            {
                continue;
            }

            // "Any tags assigned to the instance" (`Instance$Tags`): the tags a rule's
            // scope can narrow by. A tag Amazon EC2 reported with no key narrows nothing.
            let tags: BTreeMap<String, String> = instance
                .tags()
                .iter()
                .filter_map(|tag| {
                    Some((
                        tag.key.clone().filter(|key| !key.is_empty())?,
                        tag.value.clone().unwrap_or_default(),
                    ))
                })
                .collect();

            observed.push(DiscoveredResourceEntry {
                resource_type: EC2_INSTANCE_RESOURCE_TYPE.to_string(),
                resource_id: instance_id,
                // Amazon EC2 reports no custom name for an instance — `Instance` carries
                // no name member — so the item records none rather than one read off a
                // tag.
                resource_name: None,
                // `DescribeInstances` reports no ARN for an instance, so the item's ARN is
                // the one the recorded type and id derive
                // ([`super::recorded_resource_arn`]) rather than one invented here.
                arn: None,
                deleted_at: None,
                // The schema version of a configuration item Config itself recorded is not
                // one a caller supplied, and this emulator has no catalogue of them, so
                // none is reported rather than an invented one.
                schema_version_id: None,
                configuration: Some(recorded_instance_configuration(instance).to_string()),
                // Amazon EC2 reports an instance's whole configuration through the one
                // describe above, so there is no further block to supplement it with.
                supplementary_configuration: BTreeMap::new(),
                tags,
                first_recorded_at: Some(now),
                last_recorded_at: Some(now),
                // What this pass reads of an instance is the instance itself: the other
                // resources it is linked to are recorded on those resources' own
                // configuration items, so the item records no relationship rather than one
                // nothing observed.
                relationships: Vec::new(),
                // The states this instance has already left are kept by the store's own
                // write path, which carries them over from the item this one replaces.
                history: Vec::new(),
            });
        }
    }
    Some(observed)
}

/// The configuration document Config records for one Amazon EC2 instance: the members
/// `DescribeInstances` reports for it, each keyed as that member is named and named on the
/// member it comes from.
///
/// Rendered from the instance alone, so two observations of an unchanged instance render
/// the same document.
fn recorded_instance_configuration(
    instance: &::aws_sdk_ec2::types::Instance,
) -> ::serde_json::Value {
    ::serde_json::json!({
        // "The ID of the instance" (`Instance$InstanceId`).
        "instanceId": instance.instance_id,
        // "The ID of the AMI used to launch the instance" (`Instance$ImageId`).
        "imageId": instance.image_id,
        // "The instance type" (`Instance$InstanceType`).
        "instanceType": instance.instance_type.as_ref().map(|held| held.as_str()),
        // "The current state of the instance" (`Instance$State`), as its own two members
        // report it (`InstanceState$Code`, `$Name`).
        "state": instance.state.as_ref().map(|state| ::serde_json::json!({
            "code": state.code,
            "name": state.name.as_ref().map(|name| name.as_str()),
        })),
        // "The reason for the most recent state transition"
        // (`Instance$StateTransitionReason`).
        "stateTransitionReason": instance.state_transition_reason,
        // "The time that the instance was last launched" (`Instance$LaunchTime`).
        "launchTime": instance
            .launch_time
            .as_ref()
            .and_then(|at| at.fmt(::aws_smithy_types::date_time::Format::DateTime).ok()),
        // "The AMI launch index, which can be used to find this instance in the launch
        // group" (`Instance$AmiLaunchIndex`).
        "amiLaunchIndex": instance.ami_launch_index,
        // "The name of the key pair, if this instance was launched with an associated key
        // pair" (`Instance$KeyName`).
        "keyName": instance.key_name,
        // The addresses and hostnames Amazon EC2 assigned the instance
        // (`Instance$PrivateIpAddress`, `$PublicIpAddress`, `$PrivateDnsName`,
        // `$PublicDnsName`).
        "privateIpAddress": instance.private_ip_address,
        "publicIpAddress": instance.public_ip_address,
        "privateDnsName": instance.private_dns_name,
        "publicDnsName": instance.public_dns_name,
        // "The ID of the subnet in which the instance is running" and "the ID of the VPC in
        // which the instance is running" (`Instance$SubnetId`, `$VpcId`).
        "subnetId": instance.subnet_id,
        "vpcId": instance.vpc_id,
        // "The location where the instance launched" (`Instance$Placement`), as the members
        // of `Placement` report it.
        "placement": instance.placement.as_ref().map(|placement| ::serde_json::json!({
            "availabilityZone": placement.availability_zone,
            "groupName": placement.group_name,
            "tenancy": placement.tenancy.as_ref().map(|tenancy| tenancy.as_str()),
        })),
        // "The monitoring for the instance" (`Instance$Monitoring`), whose own member is
        // the monitoring state (`Monitoring$State`).
        "monitoring": instance.monitoring.as_ref().map(|monitoring| ::serde_json::json!({
            "state": monitoring.state.as_ref().map(|state| state.as_str()),
        })),
        // "The IAM instance profile associated with the instance, if applicable"
        // (`Instance$IamInstanceProfile`).
        "iamInstanceProfile": instance
            .iam_instance_profile
            .as_ref()
            .map(|profile| ::serde_json::json!({
                "arn": profile.arn,
                "id": profile.id,
            })),
        // "The architecture of the image" (`Instance$Architecture`).
        "architecture": instance.architecture.as_ref().map(|held| held.as_str()),
        // "The root device type used by the AMI" and "the device name of the root device
        // volume" (`Instance$RootDeviceType`, `$RootDeviceName`).
        "rootDeviceType": instance.root_device_type.as_ref().map(|held| held.as_str()),
        "rootDeviceName": instance.root_device_name,
        // "The virtualization type of the instance" and "the hypervisor type of the
        // instance" (`Instance$VirtualizationType`, `$Hypervisor`).
        "virtualizationType": instance.virtualization_type.as_ref().map(|held| held.as_str()),
        "hypervisor": instance.hypervisor.as_ref().map(|held| held.as_str()),
        // "Indicates whether the instance is optimized for Amazon EBS I/O",
        // "specifies whether enhanced networking with ENA is enabled" and "indicates
        // whether source/destination checking is enabled" (`Instance$EbsOptimized`,
        // `$EnaSupport`, `$SourceDestCheck`).
        "ebsOptimized": instance.ebs_optimized,
        "enaSupport": instance.ena_support,
        "sourceDestCheck": instance.source_dest_check,
        // "The platform. This value is `windows` for Windows instances; otherwise, it is
        // empty" (`Instance$Platform`).
        "platform": instance.platform.as_ref().map(|held| held.as_str()),
        // "The security groups for the instance" (`Instance$SecurityGroups`), each as its
        // own two members report it (`GroupIdentifier$GroupId`, `$GroupName`). Ordered by
        // the group id, so an unchanged instance renders the same document however the
        // groups were listed.
        "securityGroups": recorded_security_groups(instance),
        // "Any block device mapping entries for the instance"
        // (`Instance$BlockDeviceMappings`), each as the device it is attached as and the
        // EBS volume behind it (`InstanceBlockDeviceMapping$DeviceName`, `$Ebs`).
        "blockDeviceMappings": recorded_block_devices(instance),
        // "The network interfaces for the instance" (`Instance$NetworkInterfaces`).
        "networkInterfaces": recorded_network_interfaces(instance),
    })
}

/// The instance's security groups as its configuration item records them, ordered by the
/// group id.
fn recorded_security_groups(
    instance: &::aws_sdk_ec2::types::Instance,
) -> Vec<::serde_json::Value> {
    let mut groups: Vec<(String, ::serde_json::Value)> = instance
        .security_groups()
        .iter()
        .map(|group| {
            (
                group.group_id.clone().unwrap_or_default(),
                ::serde_json::json!({
                    "groupId": group.group_id,
                    "groupName": group.group_name,
                }),
            )
        })
        .collect();
    groups.sort_by(|left, right| left.0.cmp(&right.0));
    groups.into_iter().map(|(_, group)| group).collect()
}

/// The instance's block device mappings as its configuration item records them, ordered by
/// the device name.
fn recorded_block_devices(instance: &::aws_sdk_ec2::types::Instance) -> Vec<::serde_json::Value> {
    let mut devices: Vec<(String, ::serde_json::Value)> = instance
        .block_device_mappings()
        .iter()
        .map(|mapping| {
            (
                mapping.device_name.clone().unwrap_or_default(),
                ::serde_json::json!({
                    "deviceName": mapping.device_name,
                    // "Parameters used to automatically set up EBS volumes when the
                    // instance is launched" (`InstanceBlockDeviceMapping$Ebs`).
                    "ebs": mapping.ebs.as_ref().map(|ebs| ::serde_json::json!({
                        "volumeId": ebs.volume_id,
                        "status": ebs.status.as_ref().map(|status| status.as_str()),
                        "deleteOnTermination": ebs.delete_on_termination,
                        "attachTime": ebs
                            .attach_time
                            .as_ref()
                            .and_then(|at| {
                                at.fmt(::aws_smithy_types::date_time::Format::DateTime).ok()
                            }),
                    })),
                }),
            )
        })
        .collect();
    devices.sort_by(|left, right| left.0.cmp(&right.0));
    devices.into_iter().map(|(_, device)| device).collect()
}

/// The instance's network interfaces as its configuration item records them, ordered by the
/// interface id — the members `InstanceNetworkInterface` reports for each one.
fn recorded_network_interfaces(
    instance: &::aws_sdk_ec2::types::Instance,
) -> Vec<::serde_json::Value> {
    let mut interfaces: Vec<(String, ::serde_json::Value)> = instance
        .network_interfaces()
        .iter()
        .map(|interface| {
            (
                interface.network_interface_id.clone().unwrap_or_default(),
                ::serde_json::json!({
                    "networkInterfaceId": interface.network_interface_id,
                    "subnetId": interface.subnet_id,
                    "vpcId": interface.vpc_id,
                    "privateIpAddress": interface.private_ip_address,
                    "privateDnsName": interface.private_dns_name,
                    "status": interface.status.as_ref().map(|status| status.as_str()),
                    "sourceDestCheck": interface.source_dest_check,
                }),
            )
        })
        .collect();
    interfaces.sort_by(|left, right| left.0.cmp(&right.0));
    interfaces.into_iter().map(|(_, interface)| interface).collect()
}

/// An Amazon EC2 client over the in-process callback, built the way this crate's other
/// observation clients are: the callback's own HTTP client, so the endpoint below is only
/// the address that transport routes on IN PROCESS — no socket is opened and nothing leaves
/// this process — and the callback's credentials, which sign this in-process hop.
fn ec2_client(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_ec2::Client {
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        // The transport is in-process: nothing to retry, time out or watch for a stalled
        // stream, and each would need a sleep implementation this client is not given.
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_ec2::Client::from_conf(config)
}
