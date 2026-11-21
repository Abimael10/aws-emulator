// SPDX-License-Identifier: CC0-1.0

//! The LAUNCH-TEMPLATE shapes of this service: the launch parameters a template's version
//! holds as a request states them and as a response reports them, the template and the
//! version themselves, and the validation warning a write reports its findings in.
//!
//! Split out of `super` (the shapes more than one operation reaches) so that one file holds
//! one concern; every name is re-exported from there, so nothing about how they are reached
//! changes.
#![allow(unused_imports)]

use super::*;

smithy_macros::smithy_model!("smithy-model/com-amazonaws-ec2-amazonec2.json");

// `skip_validation = ["InstanceType"]`: a launch template is a set of launch PARAMETERS
// rather than a launch, so an instance type the published catalog does not hold is STORED by
// the write and reported in its `Warning` ([`crate::state::launch_template_warning`]) instead
// of refusing it — "the error codes and error messages that are returned for the parameters
// or parameter combinations that are not valid when a new launch template or new version of a
// launch template is created" (`ValidationWarning`), and neither write binds an error for it.
// The launch that tries to USE such a type is what refuses it
// (`crate::ops::run_instances`). `skip_validation` drops only this member's constraint traits:
// every other member of these parameters is still judged at the boundary, and every other
// shape that names the type a resource is PLACED as keeps the catalog as its value set — see
// `crate::service::launch_template_instance_types`, which pins both halves.
#[smithy_macros::smithy(
    shape = "com.amazonaws.ec2#RequestLaunchTemplateData",
    skip_validation = ["InstanceType"]
)]
#[derive(Debug, Clone, Default)]
pub struct RequestLaunchTemplateData {
    pub kernel_id: Option<String>,
    pub ebs_optimized: Option<bool>,
    pub iam_instance_profile: Option<LaunchTemplateIamInstanceProfileSpecificationRequest>,
    pub block_device_mappings: Option<Vec<LaunchTemplateBlockDeviceMappingRequest>>,
    pub network_interfaces: Option<Vec<LaunchTemplateInstanceNetworkInterfaceSpecificationRequest>>,
    pub image_id: Option<String>,
    pub instance_type: Option<String>,
    pub key_name: Option<String>,
    pub monitoring: Option<LaunchTemplatesMonitoringRequest>,
    pub placement: Option<LaunchTemplatePlacementRequest>,
    pub ram_disk_id: Option<String>,
    pub disable_api_termination: Option<bool>,
    pub instance_initiated_shutdown_behavior: Option<String>,
    pub user_data: Option<String>,
    pub tag_specifications: Option<Vec<LaunchTemplateTagSpecificationRequest>>,
    pub elastic_gpu_specifications: Option<Vec<ElasticGpuSpecification>>,
    pub elastic_inference_accelerators: Option<Vec<LaunchTemplateElasticInferenceAccelerator>>,
    pub security_group_ids: Option<Vec<String>>,
    pub security_groups: Option<Vec<String>>,
    pub instance_market_options: Option<LaunchTemplateInstanceMarketOptionsRequest>,
    pub credit_specification: Option<CreditSpecificationRequest>,
    pub cpu_options: Option<LaunchTemplateCpuOptionsRequest>,
    pub capacity_reservation_specification: Option<LaunchTemplateCapacityReservationSpecificationRequest>,
    pub license_specifications: Option<Vec<LaunchTemplateLicenseConfigurationRequest>>,
    pub hibernation_options: Option<LaunchTemplateHibernationOptionsRequest>,
    pub metadata_options: Option<LaunchTemplateInstanceMetadataOptionsRequest>,
    pub enclave_options: Option<LaunchTemplateEnclaveOptionsRequest>,
    pub instance_requirements: Option<InstanceRequirementsRequest>,
    pub private_dns_name_options: Option<LaunchTemplatePrivateDnsNameOptionsRequest>,
    pub maintenance_options: Option<LaunchTemplateInstanceMaintenanceOptionsRequest>,
    pub disable_api_stop: Option<bool>,
    pub operator: Option<OperatorRequest>,
    pub network_performance_options: Option<LaunchTemplateNetworkPerformanceOptionsRequest>,
    pub secondary_interfaces: Option<Vec<LaunchTemplateInstanceSecondaryInterfaceSpecificationRequest>>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateIamInstanceProfileSpecificationRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateIamInstanceProfileSpecificationRequest {
    pub arn: Option<String>,
    pub name: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateBlockDeviceMappingRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateBlockDeviceMappingRequest {
    pub device_name: Option<String>,
    pub virtual_name: Option<String>,
    pub ebs: Option<LaunchTemplateEbsBlockDeviceRequest>,
    pub no_device: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateEbsBlockDeviceRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateEbsBlockDeviceRequest {
    pub encrypted: Option<bool>,
    pub delete_on_termination: Option<bool>,
    pub iops: Option<i32>,
    pub kms_key_id: Option<String>,
    pub snapshot_id: Option<String>,
    pub volume_size: Option<i32>,
    pub volume_type: Option<String>,
    pub throughput: Option<i32>,
    pub volume_initialization_rate: Option<i32>,
    pub ebs_card_index: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateInstanceNetworkInterfaceSpecificationRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateInstanceNetworkInterfaceSpecificationRequest {
    pub associate_carrier_ip_address: Option<bool>,
    pub associate_public_ip_address: Option<bool>,
    pub delete_on_termination: Option<bool>,
    pub description: Option<String>,
    pub device_index: Option<i32>,
    pub groups: Option<Vec<String>>,
    pub interface_type: Option<String>,
    pub ipv6_address_count: Option<i32>,
    pub ipv6_addresses: Option<Vec<InstanceIpv6AddressRequest>>,
    pub network_interface_id: Option<String>,
    pub private_ip_address: Option<String>,
    pub private_ip_addresses: Option<Vec<PrivateIpAddressSpecification>>,
    pub secondary_private_ip_address_count: Option<i32>,
    pub subnet_id: Option<String>,
    pub network_card_index: Option<i32>,
    pub ipv4_prefixes: Option<Vec<Ipv4PrefixSpecificationRequest>>,
    pub ipv4_prefix_count: Option<i32>,
    pub ipv6_prefixes: Option<Vec<Ipv6PrefixSpecificationRequest>>,
    pub ipv6_prefix_count: Option<i32>,
    pub primary_ipv6: Option<bool>,
    pub ena_srd_specification: Option<EnaSrdSpecificationRequest>,
    pub connection_tracking_specification: Option<ConnectionTrackingSpecificationRequest>,
    pub ena_queue_count: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#InstanceIpv6AddressRequest")]
#[derive(Debug, Clone, Default)]
pub struct InstanceIpv6AddressRequest {
    pub ipv6_address: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#PrivateIpAddressSpecification")]
#[derive(Debug, Clone, Default)]
pub struct PrivateIpAddressSpecification {
    pub primary: Option<bool>,
    pub private_ip_address: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#Ipv4PrefixSpecificationRequest")]
#[derive(Debug, Clone, Default)]
pub struct Ipv4PrefixSpecificationRequest {
    pub ipv4_prefix: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#Ipv6PrefixSpecificationRequest")]
#[derive(Debug, Clone, Default)]
pub struct Ipv6PrefixSpecificationRequest {
    pub ipv6_prefix: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#EnaSrdSpecificationRequest")]
#[derive(Debug, Clone, Default)]
pub struct EnaSrdSpecificationRequest {
    pub ena_srd_enabled: Option<bool>,
    pub ena_srd_udp_specification: Option<EnaSrdUdpSpecificationRequest>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#EnaSrdUdpSpecificationRequest")]
#[derive(Debug, Clone, Default)]
pub struct EnaSrdUdpSpecificationRequest {
    pub ena_srd_udp_enabled: Option<bool>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#ConnectionTrackingSpecificationRequest")]
#[derive(Debug, Clone, Default)]
pub struct ConnectionTrackingSpecificationRequest {
    pub tcp_established_timeout: Option<i32>,
    pub udp_stream_timeout: Option<i32>,
    pub udp_timeout: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplatesMonitoringRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplatesMonitoringRequest {
    pub enabled: Option<bool>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplatePlacementRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplatePlacementRequest {
    pub availability_zone: Option<String>,
    pub availability_zone_id: Option<String>,
    pub affinity: Option<String>,
    pub group_name: Option<String>,
    pub host_id: Option<String>,
    pub tenancy: Option<String>,
    pub spread_domain: Option<String>,
    pub host_resource_group_arn: Option<String>,
    pub partition_number: Option<i32>,
    pub group_id: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateTagSpecificationRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateTagSpecificationRequest {
    pub resource_type: Option<String>,
    pub tags: Option<Vec<Tag>>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#ElasticGpuSpecification")]
#[derive(Debug, Clone, Default)]
pub struct ElasticGpuSpecification {
    pub r#type: String,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateElasticInferenceAccelerator")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateElasticInferenceAccelerator {
    pub r#type: String,
    pub count: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateInstanceMarketOptionsRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateInstanceMarketOptionsRequest {
    pub market_type: Option<String>,
    pub spot_options: Option<LaunchTemplateSpotMarketOptionsRequest>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateSpotMarketOptionsRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateSpotMarketOptionsRequest {
    pub max_price: Option<String>,
    pub spot_instance_type: Option<String>,
    pub block_duration_minutes: Option<i32>,
    pub valid_until: Option<::aws_smithy_types::DateTime>,
    pub instance_interruption_behavior: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#CreditSpecificationRequest")]
#[derive(Debug, Clone, Default)]
pub struct CreditSpecificationRequest {
    pub cpu_credits: String,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateCpuOptionsRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateCpuOptionsRequest {
    pub core_count: Option<i32>,
    pub threads_per_core: Option<i32>,
    pub amd_sev_snp: Option<String>,
    pub nested_virtualization: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateCapacityReservationSpecificationRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateCapacityReservationSpecificationRequest {
    pub capacity_reservation_preference: Option<String>,
    pub capacity_reservation_target: Option<CapacityReservationTarget>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#CapacityReservationTarget")]
#[derive(Debug, Clone, Default)]
pub struct CapacityReservationTarget {
    pub capacity_reservation_id: Option<String>,
    pub capacity_reservation_resource_group_arn: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateLicenseConfigurationRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateLicenseConfigurationRequest {
    pub license_configuration_arn: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateHibernationOptionsRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateHibernationOptionsRequest {
    pub configured: Option<bool>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateInstanceMetadataOptionsRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateInstanceMetadataOptionsRequest {
    pub http_tokens: Option<String>,
    pub http_put_response_hop_limit: Option<i32>,
    pub http_endpoint: Option<String>,
    pub http_protocol_ipv6: Option<String>,
    pub instance_metadata_tags: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateEnclaveOptionsRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateEnclaveOptionsRequest {
    pub enabled: Option<bool>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplatePrivateDnsNameOptionsRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplatePrivateDnsNameOptionsRequest {
    pub hostname_type: Option<String>,
    pub enable_resource_name_dns_a_record: Option<bool>,
    pub enable_resource_name_dns_aaaa_record: Option<bool>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateInstanceMaintenanceOptionsRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateInstanceMaintenanceOptionsRequest {
    pub auto_recovery: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#OperatorRequest")]
#[derive(Debug, Clone, Default)]
pub struct OperatorRequest {
    pub principal: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateNetworkPerformanceOptionsRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateNetworkPerformanceOptionsRequest {
    pub bandwidth_weighting: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateInstanceSecondaryInterfaceSpecificationRequest")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateInstanceSecondaryInterfaceSpecificationRequest {
    pub delete_on_termination: Option<bool>,
    pub device_index: Option<i32>,
    pub private_ip_addresses: Option<Vec<SecondaryInterfacePrivateIpAddressSpecificationRequest>>,
    pub private_ip_address_count: Option<i32>,
    pub secondary_subnet_id: Option<String>,
    pub interface_type: Option<String>,
    pub network_card_index: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#SecondaryInterfacePrivateIpAddressSpecificationRequest")]
#[derive(Debug, Clone, Default)]
pub struct SecondaryInterfacePrivateIpAddressSpecificationRequest {
    pub private_ip_address: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplate")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplate {
    pub launch_template_id: Option<String>,
    pub launch_template_name: Option<String>,
    pub create_time: Option<::aws_smithy_types::DateTime>,
    pub created_by: Option<String>,
    pub default_version_number: Option<i64>,
    pub latest_version_number: Option<i64>,
    pub tags: Option<Vec<Tag>>,
    pub operator: Option<OperatorResponse>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#ValidationWarning")]
#[derive(Debug, Clone, Default)]
pub struct ValidationWarning {
    pub errors: Option<Vec<ValidationError>>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#ValidationError")]
#[derive(Debug, Clone, Default)]
pub struct ValidationError {
    pub code: Option<String>,
    pub message: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateVersion")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateVersion {
    pub launch_template_id: Option<String>,
    pub launch_template_name: Option<String>,
    pub version_number: Option<i64>,
    pub version_description: Option<String>,
    pub create_time: Option<::aws_smithy_types::DateTime>,
    pub created_by: Option<String>,
    pub default_version: Option<bool>,
    pub launch_template_data: Option<ResponseLaunchTemplateData>,
    pub operator: Option<OperatorResponse>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#ResponseLaunchTemplateData")]
#[derive(Debug, Clone, Default)]
pub struct ResponseLaunchTemplateData {
    pub kernel_id: Option<String>,
    pub ebs_optimized: Option<bool>,
    pub iam_instance_profile: Option<LaunchTemplateIamInstanceProfileSpecification>,
    pub block_device_mappings: Option<Vec<LaunchTemplateBlockDeviceMapping>>,
    pub network_interfaces: Option<Vec<LaunchTemplateInstanceNetworkInterfaceSpecification>>,
    pub image_id: Option<String>,
    pub instance_type: Option<String>,
    pub key_name: Option<String>,
    pub monitoring: Option<LaunchTemplatesMonitoring>,
    pub placement: Option<LaunchTemplatePlacement>,
    pub ram_disk_id: Option<String>,
    pub disable_api_termination: Option<bool>,
    pub instance_initiated_shutdown_behavior: Option<String>,
    pub user_data: Option<String>,
    pub tag_specifications: Option<Vec<LaunchTemplateTagSpecification>>,
    pub elastic_gpu_specifications: Option<Vec<ElasticGpuSpecificationResponse>>,
    pub elastic_inference_accelerators: Option<Vec<LaunchTemplateElasticInferenceAcceleratorResponse>>,
    pub security_group_ids: Option<Vec<String>>,
    pub security_groups: Option<Vec<String>>,
    pub instance_market_options: Option<LaunchTemplateInstanceMarketOptions>,
    pub credit_specification: Option<CreditSpecification>,
    pub cpu_options: Option<LaunchTemplateCpuOptions>,
    pub capacity_reservation_specification: Option<LaunchTemplateCapacityReservationSpecificationResponse>,
    pub license_specifications: Option<Vec<LaunchTemplateLicenseConfiguration>>,
    pub hibernation_options: Option<LaunchTemplateHibernationOptions>,
    pub metadata_options: Option<LaunchTemplateInstanceMetadataOptions>,
    pub enclave_options: Option<LaunchTemplateEnclaveOptions>,
    pub instance_requirements: Option<InstanceRequirements>,
    pub private_dns_name_options: Option<LaunchTemplatePrivateDnsNameOptions>,
    pub maintenance_options: Option<LaunchTemplateInstanceMaintenanceOptions>,
    pub disable_api_stop: Option<bool>,
    pub operator: Option<OperatorResponse>,
    pub network_performance_options: Option<LaunchTemplateNetworkPerformanceOptions>,
    pub secondary_interfaces: Option<Vec<LaunchTemplateInstanceSecondaryInterfaceSpecification>>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateIamInstanceProfileSpecification")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateIamInstanceProfileSpecification {
    pub arn: Option<String>,
    pub name: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateBlockDeviceMapping")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateBlockDeviceMapping {
    pub device_name: Option<String>,
    pub virtual_name: Option<String>,
    pub ebs: Option<LaunchTemplateEbsBlockDevice>,
    pub no_device: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateEbsBlockDevice")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateEbsBlockDevice {
    pub encrypted: Option<bool>,
    pub delete_on_termination: Option<bool>,
    pub iops: Option<i32>,
    pub kms_key_id: Option<String>,
    pub snapshot_id: Option<String>,
    pub volume_size: Option<i32>,
    pub volume_type: Option<String>,
    pub throughput: Option<i32>,
    pub volume_initialization_rate: Option<i32>,
    pub ebs_card_index: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateInstanceNetworkInterfaceSpecification")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateInstanceNetworkInterfaceSpecification {
    pub associate_carrier_ip_address: Option<bool>,
    pub associate_public_ip_address: Option<bool>,
    pub delete_on_termination: Option<bool>,
    pub description: Option<String>,
    pub device_index: Option<i32>,
    pub groups: Option<Vec<String>>,
    pub interface_type: Option<String>,
    pub ipv6_address_count: Option<i32>,
    pub ipv6_addresses: Option<Vec<InstanceIpv6Address>>,
    pub network_interface_id: Option<String>,
    pub private_ip_address: Option<String>,
    pub private_ip_addresses: Option<Vec<PrivateIpAddressSpecification>>,
    pub secondary_private_ip_address_count: Option<i32>,
    pub subnet_id: Option<String>,
    pub network_card_index: Option<i32>,
    pub ipv4_prefixes: Option<Vec<Ipv4PrefixSpecificationResponse>>,
    pub ipv4_prefix_count: Option<i32>,
    pub ipv6_prefixes: Option<Vec<Ipv6PrefixSpecificationResponse>>,
    pub ipv6_prefix_count: Option<i32>,
    pub primary_ipv6: Option<bool>,
    pub ena_srd_specification: Option<LaunchTemplateEnaSrdSpecification>,
    pub connection_tracking_specification: Option<ConnectionTrackingSpecification>,
    pub ena_queue_count: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#InstanceIpv6Address")]
#[derive(Debug, Clone, Default)]
pub struct InstanceIpv6Address {
    pub ipv6_address: Option<String>,
    pub is_primary_ipv6: Option<bool>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#Ipv4PrefixSpecificationResponse")]
#[derive(Debug, Clone, Default)]
pub struct Ipv4PrefixSpecificationResponse {
    pub ipv4_prefix: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#Ipv6PrefixSpecificationResponse")]
#[derive(Debug, Clone, Default)]
pub struct Ipv6PrefixSpecificationResponse {
    pub ipv6_prefix: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateEnaSrdSpecification")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateEnaSrdSpecification {
    pub ena_srd_enabled: Option<bool>,
    pub ena_srd_udp_specification: Option<LaunchTemplateEnaSrdUdpSpecification>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateEnaSrdUdpSpecification")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateEnaSrdUdpSpecification {
    pub ena_srd_udp_enabled: Option<bool>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#ConnectionTrackingSpecification")]
#[derive(Debug, Clone, Default)]
pub struct ConnectionTrackingSpecification {
    pub tcp_established_timeout: Option<i32>,
    pub udp_timeout: Option<i32>,
    pub udp_stream_timeout: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplatesMonitoring")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplatesMonitoring {
    pub enabled: Option<bool>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplatePlacement")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplatePlacement {
    pub availability_zone: Option<String>,
    pub availability_zone_id: Option<String>,
    pub affinity: Option<String>,
    pub group_name: Option<String>,
    pub host_id: Option<String>,
    pub tenancy: Option<String>,
    pub spread_domain: Option<String>,
    pub host_resource_group_arn: Option<String>,
    pub partition_number: Option<i32>,
    pub group_id: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateTagSpecification")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateTagSpecification {
    pub resource_type: Option<String>,
    pub tags: Option<Vec<Tag>>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#ElasticGpuSpecificationResponse")]
#[derive(Debug, Clone, Default)]
pub struct ElasticGpuSpecificationResponse {
    pub r#type: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateElasticInferenceAcceleratorResponse")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateElasticInferenceAcceleratorResponse {
    pub r#type: Option<String>,
    pub count: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateInstanceMarketOptions")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateInstanceMarketOptions {
    pub market_type: Option<String>,
    pub spot_options: Option<LaunchTemplateSpotMarketOptions>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateSpotMarketOptions")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateSpotMarketOptions {
    pub max_price: Option<String>,
    pub spot_instance_type: Option<String>,
    pub block_duration_minutes: Option<i32>,
    pub valid_until: Option<::aws_smithy_types::DateTime>,
    pub instance_interruption_behavior: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#CreditSpecification")]
#[derive(Debug, Clone, Default)]
pub struct CreditSpecification {
    pub cpu_credits: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateCpuOptions")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateCpuOptions {
    pub core_count: Option<i32>,
    pub threads_per_core: Option<i32>,
    pub amd_sev_snp: Option<String>,
    pub nested_virtualization: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateCapacityReservationSpecificationResponse")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateCapacityReservationSpecificationResponse {
    pub capacity_reservation_preference: Option<String>,
    pub capacity_reservation_target: Option<CapacityReservationTargetResponse>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#CapacityReservationTargetResponse")]
#[derive(Debug, Clone, Default)]
pub struct CapacityReservationTargetResponse {
    pub capacity_reservation_id: Option<String>,
    pub capacity_reservation_resource_group_arn: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateLicenseConfiguration")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateLicenseConfiguration {
    pub license_configuration_arn: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateHibernationOptions")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateHibernationOptions {
    pub configured: Option<bool>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateInstanceMetadataOptions")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateInstanceMetadataOptions {
    pub state: Option<String>,
    pub http_tokens: Option<String>,
    pub http_put_response_hop_limit: Option<i32>,
    pub http_endpoint: Option<String>,
    pub http_protocol_ipv6: Option<String>,
    pub instance_metadata_tags: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateEnclaveOptions")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateEnclaveOptions {
    pub enabled: Option<bool>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplatePrivateDnsNameOptions")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplatePrivateDnsNameOptions {
    pub hostname_type: Option<String>,
    pub enable_resource_name_dns_a_record: Option<bool>,
    pub enable_resource_name_dns_aaaa_record: Option<bool>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateInstanceMaintenanceOptions")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateInstanceMaintenanceOptions {
    pub auto_recovery: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateNetworkPerformanceOptions")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateNetworkPerformanceOptions {
    pub bandwidth_weighting: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#LaunchTemplateInstanceSecondaryInterfaceSpecification")]
#[derive(Debug, Clone, Default)]
pub struct LaunchTemplateInstanceSecondaryInterfaceSpecification {
    pub delete_on_termination: Option<bool>,
    pub device_index: Option<i32>,
    pub private_ip_addresses: Option<Vec<SecondaryInterfacePrivateIpAddressSpecification>>,
    pub private_ip_address_count: Option<i32>,
    pub secondary_subnet_id: Option<String>,
    pub interface_type: Option<String>,
    pub network_card_index: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#SecondaryInterfacePrivateIpAddressSpecification")]
#[derive(Debug, Clone, Default)]
pub struct SecondaryInterfacePrivateIpAddressSpecification {
    pub private_ip_address: Option<String>,
}
