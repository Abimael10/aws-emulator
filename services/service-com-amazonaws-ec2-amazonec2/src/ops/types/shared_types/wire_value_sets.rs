// SPDX-License-Identifier: CC0-1.0

//! The value sets this model's enums accept on the wire: the generated catalogs every reader
//! of one compares against.
//!
//! Split out of `super` (the shapes more than one operation reaches) so that one file holds
//! one concern; every name is re-exported from there, so nothing about how they are reached
//! changes.
#![allow(unused_imports)]

use super::*;

/// The values `AcceleratorManufacturer` accepts on the wire.
pub const ACCELERATOR_MANUFACTURER_VALUES: &[&str] = &["amazon-web-services", "amd", "habana", "nvidia", "xilinx"];


/// The values `AcceleratorName` accepts on the wire.
pub const ACCELERATOR_NAME_VALUES: &[&str] = &["a100", "a10g", "gaudi-hl-205", "h100", "inferentia", "inferentia2", "k520", "k80", "l4", "l40s", "m60", "radeon-pro-v520", "t4", "t4g", "trainium", "trainium2", "u30", "v100", "vu9p"];


/// The values `AcceleratorType` accepts on the wire.
pub const ACCELERATOR_TYPE_VALUES: &[&str] = &["fpga", "gpu", "inference", "media"];


/// The values `AccountAttributeName` accepts on the wire.
pub const ACCOUNT_ATTRIBUTE_NAME_VALUES: &[&str] = &["default-vpc", "supported-platforms"];


/// The values `AccountVpcEncryptionControlMode` accepts on the wire.
pub const ACCOUNT_VPC_ENCRYPTION_CONTROL_MODE_VALUES: &[&str] = &["attempt-enforce", "attempt-monitor", "unmanaged"];


/// The values `AccountVpcEncryptionControlState` accepts on the wire.
pub const ACCOUNT_VPC_ENCRYPTION_CONTROL_STATE_VALUES: &[&str] = &["default-state", "transitions-failed", "transitions-in-progress", "transitions-partially-successful", "transitions-successful"];


/// The values `ActivityStatus` accepts on the wire.
pub const ACTIVITY_STATUS_VALUES: &[&str] = &["error", "fulfilled", "pending_fulfillment", "pending_termination"];


/// The values `AddressAttributeName` accepts on the wire.
pub const ADDRESS_ATTRIBUTE_NAME_VALUES: &[&str] = &["domain-name"];


/// The values `AddressFamily` accepts on the wire.
pub const ADDRESS_FAMILY_VALUES: &[&str] = &["ipv4", "ipv6"];


/// The values `AddressTransferStatus` accepts on the wire.
pub const ADDRESS_TRANSFER_STATUS_VALUES: &[&str] = &["accepted", "disabled", "pending"];


/// The values `Affinity` accepts on the wire.
pub const AFFINITY_VALUES: &[&str] = &["default", "host"];


/// The values `AggregationStatusEnum` accepts on the wire.
pub const AGGREGATION_STATUS_ENUM_VALUES: &[&str] = &["excluded", "included"];


/// The values `AllocationState` accepts on the wire.
pub const ALLOCATION_STATE_VALUES: &[&str] = &["available", "configuring", "pending", "permanent-failure", "released", "released-permanent-failure", "under-assessment"];


/// The values `AllocationStrategy` accepts on the wire.
pub const ALLOCATION_STRATEGY_VALUES: &[&str] = &["capacityOptimized", "capacityOptimizedPrioritized", "diversified", "lowestPrice", "priceCapacityOptimized"];


/// The values `AllocationType` accepts on the wire.
pub const ALLOCATION_TYPE_VALUES: &[&str] = &["cancelling", "future", "used"];


/// The values `AllowedImagesSettingsDisabledState` accepts on the wire.
pub const ALLOWED_IMAGES_SETTINGS_DISABLED_STATE_VALUES: &[&str] = &["disabled"];


/// The values `AllowedImagesSettingsEnabledState` accepts on the wire.
pub const ALLOWED_IMAGES_SETTINGS_ENABLED_STATE_VALUES: &[&str] = &["audit-mode", "enabled"];


/// The values `AllowsMultipleInstanceTypes` accepts on the wire.
pub const ALLOWS_MULTIPLE_INSTANCE_TYPES_VALUES: &[&str] = &["off", "on"];


/// The values `AmdSevSnp` accepts on the wire.
pub const AMD_SEV_SNP_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `AmdSevSnpSpecification` accepts on the wire.
pub const AMD_SEV_SNP_SPECIFICATION_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `AnalysisStatus` accepts on the wire.
pub const ANALYSIS_STATUS_VALUES: &[&str] = &["failed", "running", "succeeded"];


/// The values `ApplianceModeSupportValue` accepts on the wire.
pub const APPLIANCE_MODE_SUPPORT_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `ApplicationStatusCheckEnum` accepts on the wire.
pub const APPLICATION_STATUS_CHECK_ENUM_VALUES: &[&str] = &["failed", "initializing", "insufficient-data", "not-applicable", "passed"];


/// The values `ApplicationStatusEnum` accepts on the wire.
pub const APPLICATION_STATUS_ENUM_VALUES: &[&str] = &["impaired", "initializing", "insufficient-data", "not-applicable", "ok", "suppressed"];


/// The values `ApplyCancellationCharges` accepts on the wire.
pub const APPLY_CANCELLATION_CHARGES_VALUES: &[&str] = &["commitment-wind-down"];


/// The values `ArchitectureType` accepts on the wire.
pub const ARCHITECTURE_TYPE_VALUES: &[&str] = &["arm64", "arm64_mac", "i386", "x86_64", "x86_64_mac"];


/// The values `ArchitectureValues` accepts on the wire.
pub const ARCHITECTURE_VALUES_VALUES: &[&str] = &["arm64", "arm64_mac", "i386", "x86_64", "x86_64_mac"];


/// The values `AsnAssociationState` accepts on the wire.
pub const ASN_ASSOCIATION_STATE_VALUES: &[&str] = &["associated", "disassociated", "failed-association", "failed-disassociation", "pending-association", "pending-disassociation"];


/// The values `AsnState` accepts on the wire.
pub const ASN_STATE_VALUES: &[&str] = &["deprovisioned", "failed-deprovision", "failed-provision", "pending-deprovision", "pending-provision", "provisioned"];


/// The values `AssociatedNetworkType` accepts on the wire.
pub const ASSOCIATED_NETWORK_TYPE_VALUES: &[&str] = &["vpc"];


/// The values `AssociationStatusCode` accepts on the wire.
pub const ASSOCIATION_STATUS_CODE_VALUES: &[&str] = &["associated", "associating", "association-failed", "disassociated", "disassociating"];


/// The values `AssociationTypeEnum` accepts on the wire.
pub const ASSOCIATION_TYPE_ENUM_VALUES: &[&str] = &["instance-id", "tag"];


/// The values `AttachmentLimitType` accepts on the wire.
pub const ATTACHMENT_LIMIT_TYPE_VALUES: &[&str] = &["dedicated", "shared"];


/// The values `AttachmentStatus` accepts on the wire.
pub const ATTACHMENT_STATUS_VALUES: &[&str] = &["attached", "attaching", "detached", "detaching"];


/// The values `AutoAcceptSharedAssociationsValue` accepts on the wire.
pub const AUTO_ACCEPT_SHARED_ASSOCIATIONS_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `AutoAcceptSharedAttachmentsValue` accepts on the wire.
pub const AUTO_ACCEPT_SHARED_ATTACHMENTS_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `AutoPlacement` accepts on the wire.
pub const AUTO_PLACEMENT_VALUES: &[&str] = &["off", "on"];


/// The values `AutoProvisionZonesState` accepts on the wire.
pub const AUTO_PROVISION_ZONES_STATE_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `AutoScalingIpsState` accepts on the wire.
pub const AUTO_SCALING_IPS_STATE_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `AvailabilityMode` accepts on the wire.
pub const AVAILABILITY_MODE_VALUES: &[&str] = &["regional", "zonal"];


/// The values `AvailabilityZoneOptInStatus` accepts on the wire.
pub const AVAILABILITY_ZONE_OPT_IN_STATUS_VALUES: &[&str] = &["not-opted-in", "opt-in-not-required", "opted-in"];


/// The values `AvailabilityZoneState` accepts on the wire.
pub const AVAILABILITY_ZONE_STATE_VALUES: &[&str] = &["available", "constrained", "impaired", "information", "unavailable"];


/// The values `BandwidthWeightingType` accepts on the wire.
pub const BANDWIDTH_WEIGHTING_TYPE_VALUES: &[&str] = &["default", "ebs-1", "vpc-1"];


/// The values `BareMetal` accepts on the wire.
pub const BARE_METAL_VALUES: &[&str] = &["excluded", "included", "required"];


/// The values `BatchState` accepts on the wire.
pub const BATCH_STATE_VALUES: &[&str] = &["active", "cancelled", "cancelled_running", "cancelled_terminating", "failed", "modifying", "submitted"];


/// The values `BgpStatus` accepts on the wire.
pub const BGP_STATUS_VALUES: &[&str] = &["down", "up"];


/// The values `BlockPublicAccessMode` accepts on the wire.
pub const BLOCK_PUBLIC_ACCESS_MODE_VALUES: &[&str] = &["block-bidirectional", "block-ingress", "off"];


/// The values `BootModeOverrideValues` accepts on the wire.
pub const BOOT_MODE_OVERRIDE_VALUES_VALUES: &[&str] = &["uefi"];


/// The values `BootModeType` accepts on the wire.
pub const BOOT_MODE_TYPE_VALUES: &[&str] = &["legacy-bios", "uefi"];


/// The values `BootModeValues` accepts on the wire.
pub const BOOT_MODE_VALUES_VALUES: &[&str] = &["legacy-bios", "uefi", "uefi-preferred"];


/// The values `BundleTaskState` accepts on the wire.
pub const BUNDLE_TASK_STATE_VALUES: &[&str] = &["bundling", "cancelling", "complete", "failed", "pending", "storing", "waiting-for-shutdown"];


/// The values `BurstablePerformance` accepts on the wire.
pub const BURSTABLE_PERFORMANCE_VALUES: &[&str] = &["excluded", "included", "required"];


/// The values `ByoipCidrState` accepts on the wire.
pub const BYOIP_CIDR_STATE_VALUES: &[&str] = &["advertised", "deprovisioned", "failed-deprovision", "failed-provision", "pending-advertising", "pending-deprovision", "pending-provision", "pending-withdrawal", "provisioned", "provisioned-not-publicly-advertisable"];


/// The values `CallerRole` accepts on the wire.
pub const CALLER_ROLE_VALUES: &[&str] = &["odcr-owner", "unused-reservation-billing-owner"];


/// The values `CancelBatchErrorCode` accepts on the wire.
pub const CANCEL_BATCH_ERROR_CODE_VALUES: &[&str] = &["fleetRequestIdDoesNotExist", "fleetRequestIdMalformed", "fleetRequestNotInCancellableState", "unexpectedError"];


/// The values `CancelSpotInstanceRequestState` accepts on the wire.
pub const CANCEL_SPOT_INSTANCE_REQUEST_STATE_VALUES: &[&str] = &["active", "cancelled", "closed", "completed", "open"];


/// The values `CapacityBlockExtensionStatus` accepts on the wire.
pub const CAPACITY_BLOCK_EXTENSION_STATUS_VALUES: &[&str] = &["payment-failed", "payment-pending", "payment-succeeded"];


/// The values `CapacityBlockInterconnectStatus` accepts on the wire.
pub const CAPACITY_BLOCK_INTERCONNECT_STATUS_VALUES: &[&str] = &["impaired", "insufficient-data", "ok"];


/// The values `CapacityBlockResourceState` accepts on the wire.
pub const CAPACITY_BLOCK_RESOURCE_STATE_VALUES: &[&str] = &["active", "cancelled", "expired", "failed", "payment-failed", "payment-pending", "scheduled", "unavailable"];


/// The values `CapacityManagerDataExportStatus` accepts on the wire.
pub const CAPACITY_MANAGER_DATA_EXPORT_STATUS_VALUES: &[&str] = &["delivered", "failed", "in-progress", "pending"];


/// The values `CapacityManagerMonitoredTagKeyStatus` accepts on the wire.
pub const CAPACITY_MANAGER_MONITORED_TAG_KEY_STATUS_VALUES: &[&str] = &["activated", "activating", "deactivating", "suspended"];


/// The values `CapacityManagerStatus` accepts on the wire.
pub const CAPACITY_MANAGER_STATUS_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `CapacityReservationAdjustmentStatus` accepts on the wire.
pub const CAPACITY_RESERVATION_ADJUSTMENT_STATUS_VALUES: &[&str] = &["applied", "rejected", "requested"];


/// The values `CapacityReservationBillingRequestStatus` accepts on the wire.
pub const CAPACITY_RESERVATION_BILLING_REQUEST_STATUS_VALUES: &[&str] = &["accepted", "cancelled", "expired", "pending", "rejected", "revoked"];


/// The values `CapacityReservationCancellationQuoteState` accepts on the wire.
pub const CAPACITY_RESERVATION_CANCELLATION_QUOTE_STATE_VALUES: &[&str] = &["active", "expired", "pending"];


/// The values `CapacityReservationDeliveryPreference` accepts on the wire.
pub const CAPACITY_RESERVATION_DELIVERY_PREFERENCE_VALUES: &[&str] = &["fixed", "incremental"];


/// The values `CapacityReservationFleetState` accepts on the wire.
pub const CAPACITY_RESERVATION_FLEET_STATE_VALUES: &[&str] = &["active", "cancelled", "cancelling", "expired", "expiring", "failed", "modifying", "partially_fulfilled", "submitted"];


/// The values `CapacityReservationInstancePlatform` accepts on the wire.
pub const CAPACITY_RESERVATION_INSTANCE_PLATFORM_VALUES: &[&str] = &["Linux with SQL Server Enterprise", "Linux with SQL Server Standard", "Linux with SQL Server Web", "Linux/UNIX", "RHEL with HA", "RHEL with HA and SQL Server Enterprise", "RHEL with HA and SQL Server Standard", "RHEL with SQL Server Enterprise", "RHEL with SQL Server Standard", "RHEL with SQL Server Web", "Red Hat Enterprise Linux", "SUSE Linux", "Ubuntu Pro", "Windows", "Windows with SQL Server", "Windows with SQL Server Enterprise", "Windows with SQL Server Standard", "Windows with SQL Server Web"];


/// The values `CapacityReservationLaunchStatus` accepts on the wire.
pub const CAPACITY_RESERVATION_LAUNCH_STATUS_VALUES: &[&str] = &["launchable", "unlaunchable"];


/// The values `CapacityReservationModificationQuoteState` accepts on the wire.
pub const CAPACITY_RESERVATION_MODIFICATION_QUOTE_STATE_VALUES: &[&str] = &["active", "expired"];


/// The values `CapacityReservationPreference` accepts on the wire.
pub const CAPACITY_RESERVATION_PREFERENCE_VALUES: &[&str] = &["capacity-reservations-only", "none", "open"];


/// The values `CapacityReservationState` accepts on the wire.
pub const CAPACITY_RESERVATION_STATE_VALUES: &[&str] = &["active", "assessing", "cancelled", "cancelling", "delayed", "expired", "failed", "payment-failed", "payment-pending", "pending", "scheduled", "unavailable", "unsupported"];


/// The values `CapacityReservationTenancy` accepts on the wire.
pub const CAPACITY_RESERVATION_TENANCY_VALUES: &[&str] = &["dedicated", "default"];


/// The values `CapacityReservationType` accepts on the wire.
pub const CAPACITY_RESERVATION_TYPE_VALUES: &[&str] = &["capacity-block", "default"];


/// The values `CapacityTenancy` accepts on the wire.
pub const CAPACITY_TENANCY_VALUES: &[&str] = &["dedicated", "default"];


/// The values `CarrierGatewayState` accepts on the wire.
pub const CARRIER_GATEWAY_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "pending"];


/// The values `ChronologicalOrder` accepts on the wire.
pub const CHRONOLOGICAL_ORDER_VALUES: &[&str] = &["forward", "reverse"];


/// The values `ClientCertificateRevocationListStatusCode` accepts on the wire.
pub const CLIENT_CERTIFICATE_REVOCATION_LIST_STATUS_CODE_VALUES: &[&str] = &["active", "pending"];


/// The values `ClientVpnAuthenticationType` accepts on the wire.
pub const CLIENT_VPN_AUTHENTICATION_TYPE_VALUES: &[&str] = &["certificate-authentication", "directory-service-authentication", "federated-authentication"];


/// The values `ClientVpnAuthorizationPolicyShadowMode` accepts on the wire.
pub const CLIENT_VPN_AUTHORIZATION_POLICY_SHADOW_MODE_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `ClientVpnAuthorizationPolicyStatus` accepts on the wire.
pub const CLIENT_VPN_AUTHORIZATION_POLICY_STATUS_VALUES: &[&str] = &["active", "creating", "deleting", "failed", "updating"];


/// The values `ClientVpnAuthorizationRuleStatusCode` accepts on the wire.
pub const CLIENT_VPN_AUTHORIZATION_RULE_STATUS_CODE_VALUES: &[&str] = &["active", "authorizing", "failed", "revoking"];


/// The values `ClientVpnConnectionStatusCode` accepts on the wire.
pub const CLIENT_VPN_CONNECTION_STATUS_CODE_VALUES: &[&str] = &["active", "failed-to-terminate", "terminated", "terminating"];


/// The values `ClientVpnDeviceTrustProviderType` accepts on the wire.
pub const CLIENT_VPN_DEVICE_TRUST_PROVIDER_TYPE_VALUES: &[&str] = &["crowdstrike", "jamf", "jumpcloud"];


/// The values `ClientVpnEndpointAttributeStatusCode` accepts on the wire.
pub const CLIENT_VPN_ENDPOINT_ATTRIBUTE_STATUS_CODE_VALUES: &[&str] = &["applied", "applying"];


/// The values `ClientVpnEndpointStatusCode` accepts on the wire.
pub const CLIENT_VPN_ENDPOINT_STATUS_CODE_VALUES: &[&str] = &["available", "deleted", "deleting", "pending", "pending-associate"];


/// The values `ClientVpnRouteStatusCode` accepts on the wire.
pub const CLIENT_VPN_ROUTE_STATUS_CODE_VALUES: &[&str] = &["active", "creating", "deleting", "failed"];


/// The values `Comparison` accepts on the wire.
pub const COMPARISON_VALUES: &[&str] = &["equals", "in"];


/// The values `ConnectionNotificationState` accepts on the wire.
pub const CONNECTION_NOTIFICATION_STATE_VALUES: &[&str] = &["Disabled", "Enabled"];


/// The values `ConnectionNotificationType` accepts on the wire.
pub const CONNECTION_NOTIFICATION_TYPE_VALUES: &[&str] = &["Topic"];


/// The values `ConnectivityType` accepts on the wire.
pub const CONNECTIVITY_TYPE_VALUES: &[&str] = &["private", "public"];


/// The values `ContainerFormat` accepts on the wire.
pub const CONTAINER_FORMAT_VALUES: &[&str] = &["ova"];


/// The values `ConversionTaskState` accepts on the wire.
pub const CONVERSION_TASK_STATE_VALUES: &[&str] = &["active", "cancelled", "cancelling", "completed"];


/// The values `CopyTagsFromSource` accepts on the wire.
pub const COPY_TAGS_FROM_SOURCE_VALUES: &[&str] = &["volume"];


/// The values `CpuManufacturer` accepts on the wire.
pub const CPU_MANUFACTURER_VALUES: &[&str] = &["amazon-web-services", "amd", "apple", "intel"];


/// The values `CurrencyCodeValues` accepts on the wire.
pub const CURRENCY_CODE_VALUES_VALUES: &[&str] = &["USD"];


/// The values `DatafeedSubscriptionState` accepts on the wire.
pub const DATAFEED_SUBSCRIPTION_STATE_VALUES: &[&str] = &["Active", "Inactive"];


/// The values `DefaultHttpTokensEnforcedState` accepts on the wire.
pub const DEFAULT_HTTP_TOKENS_ENFORCED_STATE_VALUES: &[&str] = &["disabled", "enabled", "no-preference"];


/// The values `DefaultInstanceMetadataEndpointState` accepts on the wire.
pub const DEFAULT_INSTANCE_METADATA_ENDPOINT_STATE_VALUES: &[&str] = &["disabled", "enabled", "no-preference"];


/// The values `DefaultInstanceMetadataTagsState` accepts on the wire.
pub const DEFAULT_INSTANCE_METADATA_TAGS_STATE_VALUES: &[&str] = &["disabled", "enabled", "no-preference"];


/// The values `DefaultRouteTableAssociationValue` accepts on the wire.
pub const DEFAULT_ROUTE_TABLE_ASSOCIATION_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `DefaultRouteTablePropagationValue` accepts on the wire.
pub const DEFAULT_ROUTE_TABLE_PROPAGATION_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `DefaultTargetCapacityType` accepts on the wire.
pub const DEFAULT_TARGET_CAPACITY_TYPE_VALUES: &[&str] = &["capacity-block", "on-demand", "reserved-capacity", "spot"];


/// The values `DeleteFleetErrorCode` accepts on the wire.
pub const DELETE_FLEET_ERROR_CODE_VALUES: &[&str] = &["fleetIdDoesNotExist", "fleetIdMalformed", "fleetNotInDeletableState", "unexpectedError"];


/// The values `DeleteQueuedReservedInstancesErrorCode` accepts on the wire.
pub const DELETE_QUEUED_RESERVED_INSTANCES_ERROR_CODE_VALUES: &[&str] = &["reserved-instances-id-invalid", "reserved-instances-not-in-queued-state", "unexpected-error"];


/// The values `DestinationFileFormat` accepts on the wire.
pub const DESTINATION_FILE_FORMAT_VALUES: &[&str] = &["parquet", "plain-text"];


/// The values `DeviceTrustProviderType` accepts on the wire.
pub const DEVICE_TRUST_PROVIDER_TYPE_VALUES: &[&str] = &["crowdstrike", "jamf", "jumpcloud"];


/// The values `DeviceType` accepts on the wire.
pub const DEVICE_TYPE_VALUES: &[&str] = &["ebs", "instance-store"];


/// The values `DiskImageFormat` accepts on the wire.
pub const DISK_IMAGE_FORMAT_VALUES: &[&str] = &["RAW", "VHD", "VMDK"];


/// The values `DiskType` accepts on the wire.
pub const DISK_TYPE_VALUES: &[&str] = &["hdd", "ssd"];


/// The values `DnsNameState` accepts on the wire.
pub const DNS_NAME_STATE_VALUES: &[&str] = &["failed", "pendingVerification", "verified"];


/// The values `DnsRecordIpType` accepts on the wire.
pub const DNS_RECORD_IP_TYPE_VALUES: &[&str] = &["dualstack", "ipv4", "ipv6", "service-defined"];


/// The values `DnsSupportValue` accepts on the wire.
pub const DNS_SUPPORT_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `DomainType` accepts on the wire.
pub const DOMAIN_TYPE_VALUES: &[&str] = &["standard", "vpc"];


/// The values `DynamicRoutingValue` accepts on the wire.
pub const DYNAMIC_ROUTING_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `EbsEncryptionSupport` accepts on the wire.
pub const EBS_ENCRYPTION_SUPPORT_VALUES: &[&str] = &["supported", "unsupported"];


/// The values `EbsNvmeSupport` accepts on the wire.
pub const EBS_NVME_SUPPORT_VALUES: &[&str] = &["required", "supported", "unsupported"];


/// The values `EbsOptimizedSupport` accepts on the wire.
pub const EBS_OPTIMIZED_SUPPORT_VALUES: &[&str] = &["default", "supported", "unsupported"];


/// The values `Ec2InstanceConnectEndpointState` accepts on the wire.
pub const EC2_INSTANCE_CONNECT_ENDPOINT_STATE_VALUES: &[&str] = &["create-complete", "create-failed", "create-in-progress", "delete-complete", "delete-failed", "delete-in-progress", "update-complete", "update-failed", "update-in-progress"];


/// The values `EkPubKeyFormat` accepts on the wire.
pub const EK_PUB_KEY_FORMAT_VALUES: &[&str] = &["der", "tpmt"];


/// The values `EkPubKeyType` accepts on the wire.
pub const EK_PUB_KEY_TYPE_VALUES: &[&str] = &["ecc-sec-p384", "rsa-2048"];


/// The values `ElasticGpuState` accepts on the wire.
pub const ELASTIC_GPU_STATE_VALUES: &[&str] = &["ATTACHED"];


/// The values `ElasticGpuStatus` accepts on the wire.
pub const ELASTIC_GPU_STATUS_VALUES: &[&str] = &["IMPAIRED", "OK"];


/// The values `EnaSupport` accepts on the wire.
pub const ENA_SUPPORT_VALUES: &[&str] = &["required", "supported", "unsupported"];


/// The values `EncryptionStateValue` accepts on the wire.
pub const ENCRYPTION_STATE_VALUE_VALUES: &[&str] = &["disabled", "disabling", "enabled", "enabling"];


/// The values `EncryptionSupportOptionValue` accepts on the wire.
pub const ENCRYPTION_SUPPORT_OPTION_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `EndDateType` accepts on the wire.
pub const END_DATE_TYPE_VALUES: &[&str] = &["limited", "unlimited"];


/// The values `EndpointIpAddressType` accepts on the wire.
pub const ENDPOINT_IP_ADDRESS_TYPE_VALUES: &[&str] = &["dual-stack", "ipv4", "ipv6"];


/// The values `EphemeralNvmeSupport` accepts on the wire.
pub const EPHEMERAL_NVME_SUPPORT_VALUES: &[&str] = &["required", "supported", "unsupported"];


/// The values `EventCode` accepts on the wire.
pub const EVENT_CODE_VALUES: &[&str] = &["instance-reboot", "instance-retirement", "instance-stop", "system-maintenance", "system-reboot"];


/// The values `EventType` accepts on the wire.
pub const EVENT_TYPE_VALUES: &[&str] = &["error", "fleetRequestChange", "information", "instanceChange"];


/// The values `ExcessCapacityTerminationPolicy` accepts on the wire.
pub const EXCESS_CAPACITY_TERMINATION_POLICY_VALUES: &[&str] = &["default", "noTermination"];


/// The values `ExportEnvironment` accepts on the wire.
pub const EXPORT_ENVIRONMENT_VALUES: &[&str] = &["citrix", "microsoft", "vmware"];


/// The values `ExportTaskState` accepts on the wire.
pub const EXPORT_TASK_STATE_VALUES: &[&str] = &["active", "cancelled", "cancelling", "completed"];


/// The values `FastLaunchResourceType` accepts on the wire.
pub const FAST_LAUNCH_RESOURCE_TYPE_VALUES: &[&str] = &["snapshot"];


/// The values `FastLaunchStateCode` accepts on the wire.
pub const FAST_LAUNCH_STATE_CODE_VALUES: &[&str] = &["disabling", "disabling-failed", "enabled", "enabled-failed", "enabling", "enabling-failed"];


/// The values `FastSnapshotRestoreStateCode` accepts on the wire.
pub const FAST_SNAPSHOT_RESTORE_STATE_CODE_VALUES: &[&str] = &["disabled", "disabling", "enabled", "enabling", "optimizing"];


/// The values `FilterByDimension` accepts on the wire.
pub const FILTER_BY_DIMENSION_VALUES: &[&str] = &["account-id", "account-name", "availability-zone-id", "instance-family", "instance-platform", "instance-type", "reservation-arn", "reservation-create-timestamp", "reservation-end-date-type", "reservation-end-timestamp", "reservation-id", "reservation-instance-match-criteria", "reservation-start-timestamp", "reservation-state", "reservation-type", "reservation-unused-financial-owner", "resource-region", "tenancy"];


/// The values `FindingsFound` accepts on the wire.
pub const FINDINGS_FOUND_VALUES: &[&str] = &["false", "true", "unknown"];


/// The values `FleetActivityStatus` accepts on the wire.
pub const FLEET_ACTIVITY_STATUS_VALUES: &[&str] = &["error", "fulfilled", "pending_fulfillment", "pending_termination"];


/// The values `FleetCapacityReservationTenancy` accepts on the wire.
pub const FLEET_CAPACITY_RESERVATION_TENANCY_VALUES: &[&str] = &["default"];


/// The values `FleetCapacityReservationUsageStrategy` accepts on the wire.
pub const FLEET_CAPACITY_RESERVATION_USAGE_STRATEGY_VALUES: &[&str] = &["use-capacity-reservations-first"];


/// The values `FleetEventType` accepts on the wire.
pub const FLEET_EVENT_TYPE_VALUES: &[&str] = &["fleet-change", "instance-change", "service-error"];


/// The values `FleetExcessCapacityTerminationPolicy` accepts on the wire.
pub const FLEET_EXCESS_CAPACITY_TERMINATION_POLICY_VALUES: &[&str] = &["no-termination", "termination"];


/// The values `FleetHttpTokensState` accepts on the wire.
pub const FLEET_HTTP_TOKENS_STATE_VALUES: &[&str] = &["optional", "required"];


/// The values `FleetInstanceMatchCriteria` accepts on the wire.
pub const FLEET_INSTANCE_MATCH_CRITERIA_VALUES: &[&str] = &["open"];


/// The values `FleetInstanceMetadataEndpointState` accepts on the wire.
pub const FLEET_INSTANCE_METADATA_ENDPOINT_STATE_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `FleetOnDemandAllocationStrategy` accepts on the wire.
pub const FLEET_ON_DEMAND_ALLOCATION_STRATEGY_VALUES: &[&str] = &["lowest-price", "prioritized"];


/// The values `FleetReplacementStrategy` accepts on the wire.
pub const FLEET_REPLACEMENT_STRATEGY_VALUES: &[&str] = &["launch", "launch-before-terminate"];


/// The values `FleetReservationType` accepts on the wire.
pub const FLEET_RESERVATION_TYPE_VALUES: &[&str] = &["capacity-block", "interruptible-capacity-reservation", "on-demand-capacity-reservation"];


/// The values `FleetStateCode` accepts on the wire.
pub const FLEET_STATE_CODE_VALUES: &[&str] = &["active", "deleted", "deleted_running", "deleted_terminating", "failed", "modifying", "submitted"];


/// The values `FleetType` accepts on the wire.
pub const FLEET_TYPE_VALUES: &[&str] = &["instant", "maintain", "request"];


/// The values `FlexibleEnaQueuesSupport` accepts on the wire.
pub const FLEXIBLE_ENA_QUEUES_SUPPORT_VALUES: &[&str] = &["supported", "unsupported"];


/// The values `FlowLogsResourceType` accepts on the wire.
pub const FLOW_LOGS_RESOURCE_TYPE_VALUES: &[&str] = &["NetworkInterface", "RegionalNatGateway", "Subnet", "TransitGateway", "TransitGatewayAttachment", "VPC"];


/// The values `FpgaImageAttributeName` accepts on the wire.
pub const FPGA_IMAGE_ATTRIBUTE_NAME_VALUES: &[&str] = &["description", "loadPermission", "name", "productCodes"];


/// The values `FpgaImageStateCode` accepts on the wire.
pub const FPGA_IMAGE_STATE_CODE_VALUES: &[&str] = &["available", "failed", "pending", "unavailable"];


/// The values `GatewayAssociationState` accepts on the wire.
pub const GATEWAY_ASSOCIATION_STATE_VALUES: &[&str] = &["associated", "associating", "disassociating", "not-associated"];


/// The values `GatewayType` accepts on the wire.
pub const GATEWAY_TYPE_VALUES: &[&str] = &["ipsec.1"];


/// The values `GroupBy` accepts on the wire.
pub const GROUP_BY_VALUES: &[&str] = &["account-id", "account-name", "availability-zone-id", "instance-family", "instance-platform", "instance-type", "reservation-arn", "reservation-create-timestamp", "reservation-end-date-type", "reservation-end-timestamp", "reservation-id", "reservation-instance-match-criteria", "reservation-start-timestamp", "reservation-state", "reservation-type", "reservation-unused-financial-owner", "resource-region", "tenancy"];


/// The values `HaStatus` accepts on the wire.
pub const HA_STATUS_VALUES: &[&str] = &["active", "invalid", "processing", "standby"];


/// The values `HostMaintenance` accepts on the wire.
pub const HOST_MAINTENANCE_VALUES: &[&str] = &["off", "on"];


/// The values `HostRecovery` accepts on the wire.
pub const HOST_RECOVERY_VALUES: &[&str] = &["off", "on"];


/// The values `HostTenancy` accepts on the wire.
pub const HOST_TENANCY_VALUES: &[&str] = &["dedicated", "default", "host"];


/// The values `HostnameType` accepts on the wire.
pub const HOSTNAME_TYPE_VALUES: &[&str] = &["ip-name", "resource-name"];


/// The values `HttpTokensEnforcedState` accepts on the wire.
pub const HTTP_TOKENS_ENFORCED_STATE_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `HttpTokensState` accepts on the wire.
pub const HTTP_TOKENS_STATE_VALUES: &[&str] = &["optional", "required"];


/// The values `HypervisorType` accepts on the wire.
pub const HYPERVISOR_TYPE_VALUES: &[&str] = &["ovm", "xen"];


/// The values `IamInstanceProfileAssociationState` accepts on the wire.
pub const IAM_INSTANCE_PROFILE_ASSOCIATION_STATE_VALUES: &[&str] = &["associated", "associating", "disassociated", "disassociating"];


/// The values `Igmpv2SupportValue` accepts on the wire.
pub const IGMPV2_SUPPORT_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `ImageAttributeName` accepts on the wire.
pub const IMAGE_ATTRIBUTE_NAME_VALUES: &[&str] = &["blockDeviceMapping", "bootMode", "deregistrationProtection", "description", "imdsSupport", "kernel", "lastLaunchedTime", "launchPermission", "productCodes", "ramdisk", "sriovNetSupport", "tpmSupport", "uefiData"];


/// The values `ImageBlockPublicAccessDisabledState` accepts on the wire.
pub const IMAGE_BLOCK_PUBLIC_ACCESS_DISABLED_STATE_VALUES: &[&str] = &["unblocked"];


/// The values `ImageBlockPublicAccessEnabledState` accepts on the wire.
pub const IMAGE_BLOCK_PUBLIC_ACCESS_ENABLED_STATE_VALUES: &[&str] = &["block-new-sharing"];


/// The values `ImageReferenceOptionName` accepts on the wire.
pub const IMAGE_REFERENCE_OPTION_NAME_VALUES: &[&str] = &["state-name", "version-depth"];


/// The values `ImageReferenceResourceType` accepts on the wire.
pub const IMAGE_REFERENCE_RESOURCE_TYPE_VALUES: &[&str] = &["ec2:Instance", "ec2:LaunchTemplate", "imagebuilder:ContainerRecipe", "imagebuilder:ImageRecipe", "ssm:Parameter"];


/// The values `ImageState` accepts on the wire.
pub const IMAGE_STATE_VALUES: &[&str] = &["available", "deregistered", "disabled", "error", "failed", "invalid", "pending", "transient"];


/// The values `ImageTypeValues` accepts on the wire.
pub const IMAGE_TYPE_VALUES_VALUES: &[&str] = &["kernel", "machine", "ramdisk"];


/// The values `ImdsSupportValues` accepts on the wire.
pub const IMDS_SUPPORT_VALUES_VALUES: &[&str] = &["v2.0"];


/// The values `IngestionStatus` accepts on the wire.
pub const INGESTION_STATUS_VALUES: &[&str] = &["ingestion-complete", "ingestion-failed", "initial-ingestion-in-progress"];


/// The values `InitializationType` accepts on the wire.
pub const INITIALIZATION_TYPE_VALUES: &[&str] = &["default", "provisioned-rate", "volume-copy"];


/// The values `InstanceAttributeName` accepts on the wire.
pub const INSTANCE_ATTRIBUTE_NAME_VALUES: &[&str] = &["blockDeviceMapping", "disableApiStop", "disableApiTermination", "ebsOptimized", "enaSupport", "enclaveOptions", "groupSet", "instanceInitiatedShutdownBehavior", "instanceType", "kernel", "productCodes", "ramdisk", "rootDeviceName", "sourceDestCheck", "sriovNetSupport", "userData"];


/// The values `InstanceAutoRecoveryState` accepts on the wire.
pub const INSTANCE_AUTO_RECOVERY_STATE_VALUES: &[&str] = &["default", "disabled"];


/// The values `InstanceBandwidthWeighting` accepts on the wire.
pub const INSTANCE_BANDWIDTH_WEIGHTING_VALUES: &[&str] = &["default", "ebs-1", "vpc-1"];


/// The values `InstanceBootModeValues` accepts on the wire.
pub const INSTANCE_BOOT_MODE_VALUES_VALUES: &[&str] = &["legacy-bios", "uefi"];


/// The values `InstanceEventWindowState` accepts on the wire.
pub const INSTANCE_EVENT_WINDOW_STATE_VALUES: &[&str] = &["active", "creating", "deleted", "deleting"];


/// The values `InstanceGeneration` accepts on the wire.
pub const INSTANCE_GENERATION_VALUES: &[&str] = &["current", "previous"];


/// The values `InstanceHealthStatus` accepts on the wire.
pub const INSTANCE_HEALTH_STATUS_VALUES: &[&str] = &["healthy", "unhealthy"];


/// The values `InstanceInterruptionBehavior` accepts on the wire.
pub const INSTANCE_INTERRUPTION_BEHAVIOR_VALUES: &[&str] = &["hibernate", "stop", "terminate"];


/// The values `InstanceLifecycle` accepts on the wire.
pub const INSTANCE_LIFECYCLE_VALUES: &[&str] = &["capacity-block", "interruptible-capacity-reservation", "on-demand", "spot"];


/// The values `InstanceLifecycleType` accepts on the wire.
pub const INSTANCE_LIFECYCLE_TYPE_VALUES: &[&str] = &["capacity-block", "interruptible-capacity-reservation", "scheduled", "spot"];


/// The values `InstanceMatchCriteria` accepts on the wire.
pub const INSTANCE_MATCH_CRITERIA_VALUES: &[&str] = &["open", "targeted"];


/// The values `InstanceMetadataEndpointState` accepts on the wire.
pub const INSTANCE_METADATA_ENDPOINT_STATE_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `InstanceMetadataOptionsState` accepts on the wire.
pub const INSTANCE_METADATA_OPTIONS_STATE_VALUES: &[&str] = &["applied", "pending"];


/// The values `InstanceMetadataProtocolState` accepts on the wire.
pub const INSTANCE_METADATA_PROTOCOL_STATE_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `InstanceMetadataTagsState` accepts on the wire.
pub const INSTANCE_METADATA_TAGS_STATE_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `InstanceRebootMigrationState` accepts on the wire.
pub const INSTANCE_REBOOT_MIGRATION_STATE_VALUES: &[&str] = &["default", "disabled"];


/// The values `InstanceStateName` accepts on the wire.
pub const INSTANCE_STATE_NAME_VALUES: &[&str] = &["pending", "running", "shutting-down", "stopped", "stopping", "terminated"];


/// The values `InstanceStorageEncryptionSupport` accepts on the wire.
pub const INSTANCE_STORAGE_ENCRYPTION_SUPPORT_VALUES: &[&str] = &["required", "unsupported"];


/// The values `InstanceType` accepts on the wire.
pub const INSTANCE_TYPE_VALUES: &[&str] = &["a1.2xlarge", "a1.4xlarge", "a1.large", "a1.medium", "a1.metal", "a1.xlarge", "c1.medium", "c1.xlarge", "c3.2xlarge", "c3.4xlarge", "c3.8xlarge", "c3.large", "c3.xlarge", "c4.2xlarge", "c4.4xlarge", "c4.8xlarge", "c4.large", "c4.xlarge", "c5.12xlarge", "c5.18xlarge", "c5.24xlarge", "c5.2xlarge", "c5.4xlarge", "c5.9xlarge", "c5.large", "c5.metal", "c5.xlarge", "c5a.12xlarge", "c5a.16xlarge", "c5a.24xlarge", "c5a.2xlarge", "c5a.4xlarge", "c5a.8xlarge", "c5a.large", "c5a.xlarge", "c5ad.12xlarge", "c5ad.16xlarge", "c5ad.24xlarge", "c5ad.2xlarge", "c5ad.4xlarge", "c5ad.8xlarge", "c5ad.large", "c5ad.xlarge", "c5d.12xlarge", "c5d.18xlarge", "c5d.24xlarge", "c5d.2xlarge", "c5d.4xlarge", "c5d.9xlarge", "c5d.large", "c5d.metal", "c5d.xlarge", "c5n.18xlarge", "c5n.2xlarge", "c5n.4xlarge", "c5n.9xlarge", "c5n.large", "c5n.metal", "c5n.xlarge", "c6a.12xlarge", "c6a.16xlarge", "c6a.24xlarge", "c6a.2xlarge", "c6a.32xlarge", "c6a.48xlarge", "c6a.4xlarge", "c6a.8xlarge", "c6a.large", "c6a.metal", "c6a.xlarge", "c6g.12xlarge", "c6g.16xlarge", "c6g.2xlarge", "c6g.4xlarge", "c6g.8xlarge", "c6g.large", "c6g.medium", "c6g.metal", "c6g.xlarge", "c6gd.12xlarge", "c6gd.16xlarge", "c6gd.2xlarge", "c6gd.4xlarge", "c6gd.8xlarge", "c6gd.large", "c6gd.medium", "c6gd.metal", "c6gd.xlarge", "c6gn.12xlarge", "c6gn.16xlarge", "c6gn.2xlarge", "c6gn.4xlarge", "c6gn.8xlarge", "c6gn.large", "c6gn.medium", "c6gn.xlarge", "c6i.12xlarge", "c6i.16xlarge", "c6i.24xlarge", "c6i.2xlarge", "c6i.32xlarge", "c6i.4xlarge", "c6i.8xlarge", "c6i.large", "c6i.metal", "c6i.xlarge", "c6id.12xlarge", "c6id.16xlarge", "c6id.24xlarge", "c6id.2xlarge", "c6id.32xlarge", "c6id.4xlarge", "c6id.8xlarge", "c6id.large", "c6id.metal", "c6id.xlarge", "c6in.12xlarge", "c6in.16xlarge", "c6in.24xlarge", "c6in.2xlarge", "c6in.32xlarge", "c6in.4xlarge", "c6in.8xlarge", "c6in.large", "c6in.metal", "c6in.xlarge", "c7a.12xlarge", "c7a.16xlarge", "c7a.24xlarge", "c7a.2xlarge", "c7a.32xlarge", "c7a.48xlarge", "c7a.4xlarge", "c7a.8xlarge", "c7a.large", "c7a.medium", "c7a.metal-48xl", "c7a.xlarge", "c7g.12xlarge", "c7g.16xlarge", "c7g.2xlarge", "c7g.4xlarge", "c7g.8xlarge", "c7g.large", "c7g.medium", "c7g.metal", "c7g.xlarge", "c7gd.12xlarge", "c7gd.16xlarge", "c7gd.2xlarge", "c7gd.4xlarge", "c7gd.8xlarge", "c7gd.large", "c7gd.medium", "c7gd.metal", "c7gd.xlarge", "c7gn.12xlarge", "c7gn.16xlarge", "c7gn.2xlarge", "c7gn.4xlarge", "c7gn.8xlarge", "c7gn.large", "c7gn.medium", "c7gn.metal", "c7gn.xlarge", "c7i-flex.12xlarge", "c7i-flex.16xlarge", "c7i-flex.2xlarge", "c7i-flex.4xlarge", "c7i-flex.8xlarge", "c7i-flex.large", "c7i-flex.xlarge", "c7i.12xlarge", "c7i.16xlarge", "c7i.24xlarge", "c7i.2xlarge", "c7i.48xlarge", "c7i.4xlarge", "c7i.8xlarge", "c7i.large", "c7i.metal-24xl", "c7i.metal-48xl", "c7i.xlarge", "c8a.12xlarge", "c8a.16xlarge", "c8a.24xlarge", "c8a.2xlarge", "c8a.48xlarge", "c8a.4xlarge", "c8a.8xlarge", "c8a.large", "c8a.medium", "c8a.metal-24xl", "c8a.metal-48xl", "c8a.xlarge", "c8g.12xlarge", "c8g.16xlarge", "c8g.24xlarge", "c8g.2xlarge", "c8g.48xlarge", "c8g.4xlarge", "c8g.8xlarge", "c8g.large", "c8g.medium", "c8g.metal-24xl", "c8g.metal-48xl", "c8g.xlarge", "c8gb.12xlarge", "c8gb.16xlarge", "c8gb.24xlarge", "c8gb.2xlarge", "c8gb.48xlarge", "c8gb.4xlarge", "c8gb.8xlarge", "c8gb.large", "c8gb.medium", "c8gb.metal-24xl", "c8gb.metal-48xl", "c8gb.xlarge", "c8gd.12xlarge", "c8gd.16xlarge", "c8gd.24xlarge", "c8gd.2xlarge", "c8gd.48xlarge", "c8gd.4xlarge", "c8gd.8xlarge", "c8gd.large", "c8gd.medium", "c8gd.metal-24xl", "c8gd.metal-48xl", "c8gd.xlarge", "c8gn.12xlarge", "c8gn.16xlarge", "c8gn.24xlarge", "c8gn.2xlarge", "c8gn.48xlarge", "c8gn.4xlarge", "c8gn.8xlarge", "c8gn.large", "c8gn.medium", "c8gn.metal-24xl", "c8gn.metal-48xl", "c8gn.xlarge", "c8i-flex.12xlarge", "c8i-flex.16xlarge", "c8i-flex.2xlarge", "c8i-flex.4xlarge", "c8i-flex.8xlarge", "c8i-flex.large", "c8i-flex.xlarge", "c8i.12xlarge", "c8i.16xlarge", "c8i.24xlarge", "c8i.2xlarge", "c8i.32xlarge", "c8i.48xlarge", "c8i.4xlarge", "c8i.8xlarge", "c8i.96xlarge", "c8i.large", "c8i.metal-48xl", "c8i.metal-96xl", "c8i.xlarge", "c8ib.12xlarge", "c8ib.16xlarge", "c8ib.24xlarge", "c8ib.2xlarge", "c8ib.32xlarge", "c8ib.48xlarge", "c8ib.4xlarge", "c8ib.8xlarge", "c8ib.96xlarge", "c8ib.large", "c8ib.metal-48xl", "c8ib.metal-96xl", "c8ib.xlarge", "c8id.12xlarge", "c8id.16xlarge", "c8id.24xlarge", "c8id.2xlarge", "c8id.32xlarge", "c8id.48xlarge", "c8id.4xlarge", "c8id.8xlarge", "c8id.96xlarge", "c8id.large", "c8id.metal-48xl", "c8id.metal-96xl", "c8id.xlarge", "c8in.12xlarge", "c8in.16xlarge", "c8in.24xlarge", "c8in.2xlarge", "c8in.32xlarge", "c8in.48xlarge", "c8in.4xlarge", "c8in.8xlarge", "c8in.96xlarge", "c8in.large", "c8in.metal-48xl", "c8in.metal-96xl", "c8in.xlarge", "c8ine.12xlarge", "c8ine.2xlarge", "c8ine.4xlarge", "c8ine.8xlarge", "c8ine.large", "c8ine.xlarge", "c9g.12xlarge", "c9g.16xlarge", "c9g.24xlarge", "c9g.2xlarge", "c9g.48xlarge", "c9g.4xlarge", "c9g.8xlarge", "c9g.large", "c9g.medium", "c9g.metal-48xl", "c9g.xlarge", "c9gd.12xlarge", "c9gd.16xlarge", "c9gd.24xlarge", "c9gd.2xlarge", "c9gd.48xlarge", "c9gd.4xlarge", "c9gd.8xlarge", "c9gd.large", "c9gd.medium", "c9gd.metal-48xl", "c9gd.xlarge", "cc1.4xlarge", "cc2.8xlarge", "cg1.4xlarge", "cr1.8xlarge", "d2.2xlarge", "d2.4xlarge", "d2.8xlarge", "d2.xlarge", "d3.2xlarge", "d3.4xlarge", "d3.8xlarge", "d3.xlarge", "d3en.12xlarge", "d3en.2xlarge", "d3en.4xlarge", "d3en.6xlarge", "d3en.8xlarge", "d3en.xlarge", "dl1.24xlarge", "dl2q.24xlarge", "f1.16xlarge", "f1.2xlarge", "f1.4xlarge", "f2.12xlarge", "f2.48xlarge", "f2.6xlarge", "g2.2xlarge", "g2.8xlarge", "g3.16xlarge", "g3.4xlarge", "g3.8xlarge", "g3s.xlarge", "g4ad.16xlarge", "g4ad.2xlarge", "g4ad.4xlarge", "g4ad.8xlarge", "g4ad.xlarge", "g4dn.12xlarge", "g4dn.16xlarge", "g4dn.2xlarge", "g4dn.4xlarge", "g4dn.8xlarge", "g4dn.metal", "g4dn.xlarge", "g5.12xlarge", "g5.16xlarge", "g5.24xlarge", "g5.2xlarge", "g5.48xlarge", "g5.4xlarge", "g5.8xlarge", "g5.xlarge", "g5g.16xlarge", "g5g.2xlarge", "g5g.4xlarge", "g5g.8xlarge", "g5g.metal", "g5g.xlarge", "g6.12xlarge", "g6.16xlarge", "g6.24xlarge", "g6.2xlarge", "g6.48xlarge", "g6.4xlarge", "g6.8xlarge", "g6.xlarge", "g6e.12xlarge", "g6e.16xlarge", "g6e.24xlarge", "g6e.2xlarge", "g6e.48xlarge", "g6e.4xlarge", "g6e.8xlarge", "g6e.xlarge", "g6f.2xlarge", "g6f.4xlarge", "g6f.large", "g6f.xlarge", "g7.12xlarge", "g7.24xlarge", "g7.2xlarge", "g7.48xlarge", "g7.4xlarge", "g7.8xlarge", "g7e.12xlarge", "g7e.24xlarge", "g7e.2xlarge", "g7e.48xlarge", "g7e.4xlarge", "g7e.8xlarge", "gr6.4xlarge", "gr6.8xlarge", "gr6f.4xlarge", "h1.16xlarge", "h1.2xlarge", "h1.4xlarge", "h1.8xlarge", "hi1.4xlarge", "hpc6a.48xlarge", "hpc6id.32xlarge", "hpc7a.12xlarge", "hpc7a.24xlarge", "hpc7a.48xlarge", "hpc7a.96xlarge", "hpc7g.16xlarge", "hpc7g.4xlarge", "hpc7g.8xlarge", "hpc8a.96xlarge", "hs1.8xlarge", "i2.2xlarge", "i2.4xlarge", "i2.8xlarge", "i2.xlarge", "i3.16xlarge", "i3.2xlarge", "i3.4xlarge", "i3.8xlarge", "i3.large", "i3.metal", "i3.xlarge", "i3en.12xlarge", "i3en.24xlarge", "i3en.2xlarge", "i3en.3xlarge", "i3en.6xlarge", "i3en.large", "i3en.metal", "i3en.xlarge", "i4g.16xlarge", "i4g.2xlarge", "i4g.4xlarge", "i4g.8xlarge", "i4g.large", "i4g.xlarge", "i4i.12xlarge", "i4i.16xlarge", "i4i.24xlarge", "i4i.2xlarge", "i4i.32xlarge", "i4i.4xlarge", "i4i.8xlarge", "i4i.large", "i4i.metal", "i4i.xlarge", "i7i.12xlarge", "i7i.16xlarge", "i7i.24xlarge", "i7i.2xlarge", "i7i.48xlarge", "i7i.4xlarge", "i7i.8xlarge", "i7i.large", "i7i.metal-24xl", "i7i.metal-48xl", "i7i.xlarge", "i7ie.12xlarge", "i7ie.18xlarge", "i7ie.24xlarge", "i7ie.2xlarge", "i7ie.3xlarge", "i7ie.48xlarge", "i7ie.6xlarge", "i7ie.large", "i7ie.metal-24xl", "i7ie.metal-48xl", "i7ie.xlarge", "i8g.12xlarge", "i8g.16xlarge", "i8g.24xlarge", "i8g.2xlarge", "i8g.48xlarge", "i8g.4xlarge", "i8g.8xlarge", "i8g.large", "i8g.metal-24xl", "i8g.metal-48xl", "i8g.xlarge", "i8ge.12xlarge", "i8ge.18xlarge", "i8ge.24xlarge", "i8ge.2xlarge", "i8ge.3xlarge", "i8ge.48xlarge", "i8ge.6xlarge", "i8ge.large", "i8ge.metal-24xl", "i8ge.metal-48xl", "i8ge.xlarge", "im4gn.16xlarge", "im4gn.2xlarge", "im4gn.4xlarge", "im4gn.8xlarge", "im4gn.large", "im4gn.xlarge", "inf1.24xlarge", "inf1.2xlarge", "inf1.6xlarge", "inf1.xlarge", "inf2.24xlarge", "inf2.48xlarge", "inf2.8xlarge", "inf2.xlarge", "is4gen.2xlarge", "is4gen.4xlarge", "is4gen.8xlarge", "is4gen.large", "is4gen.medium", "is4gen.xlarge", "m1.large", "m1.medium", "m1.small", "m1.xlarge", "m2.2xlarge", "m2.4xlarge", "m2.xlarge", "m3.2xlarge", "m3.large", "m3.medium", "m3.xlarge", "m4.10xlarge", "m4.16xlarge", "m4.2xlarge", "m4.4xlarge", "m4.large", "m4.xlarge", "m5.12xlarge", "m5.16xlarge", "m5.24xlarge", "m5.2xlarge", "m5.4xlarge", "m5.8xlarge", "m5.large", "m5.metal", "m5.xlarge", "m5a.12xlarge", "m5a.16xlarge", "m5a.24xlarge", "m5a.2xlarge", "m5a.4xlarge", "m5a.8xlarge", "m5a.large", "m5a.xlarge", "m5ad.12xlarge", "m5ad.16xlarge", "m5ad.24xlarge", "m5ad.2xlarge", "m5ad.4xlarge", "m5ad.8xlarge", "m5ad.large", "m5ad.xlarge", "m5d.12xlarge", "m5d.16xlarge", "m5d.24xlarge", "m5d.2xlarge", "m5d.4xlarge", "m5d.8xlarge", "m5d.large", "m5d.metal", "m5d.xlarge", "m5dn.12xlarge", "m5dn.16xlarge", "m5dn.24xlarge", "m5dn.2xlarge", "m5dn.4xlarge", "m5dn.8xlarge", "m5dn.large", "m5dn.metal", "m5dn.xlarge", "m5n.12xlarge", "m5n.16xlarge", "m5n.24xlarge", "m5n.2xlarge", "m5n.4xlarge", "m5n.8xlarge", "m5n.large", "m5n.metal", "m5n.xlarge", "m5zn.12xlarge", "m5zn.2xlarge", "m5zn.3xlarge", "m5zn.6xlarge", "m5zn.large", "m5zn.metal", "m5zn.xlarge", "m6a.12xlarge", "m6a.16xlarge", "m6a.24xlarge", "m6a.2xlarge", "m6a.32xlarge", "m6a.48xlarge", "m6a.4xlarge", "m6a.8xlarge", "m6a.large", "m6a.metal", "m6a.xlarge", "m6g.12xlarge", "m6g.16xlarge", "m6g.2xlarge", "m6g.4xlarge", "m6g.8xlarge", "m6g.large", "m6g.medium", "m6g.metal", "m6g.xlarge", "m6gd.12xlarge", "m6gd.16xlarge", "m6gd.2xlarge", "m6gd.4xlarge", "m6gd.8xlarge", "m6gd.large", "m6gd.medium", "m6gd.metal", "m6gd.xlarge", "m6i.12xlarge", "m6i.16xlarge", "m6i.24xlarge", "m6i.2xlarge", "m6i.32xlarge", "m6i.4xlarge", "m6i.8xlarge", "m6i.large", "m6i.metal", "m6i.xlarge", "m6id.12xlarge", "m6id.16xlarge", "m6id.24xlarge", "m6id.2xlarge", "m6id.32xlarge", "m6id.4xlarge", "m6id.8xlarge", "m6id.large", "m6id.metal", "m6id.xlarge", "m6idn.12xlarge", "m6idn.16xlarge", "m6idn.24xlarge", "m6idn.2xlarge", "m6idn.32xlarge", "m6idn.4xlarge", "m6idn.8xlarge", "m6idn.large", "m6idn.metal", "m6idn.xlarge", "m6in.12xlarge", "m6in.16xlarge", "m6in.24xlarge", "m6in.2xlarge", "m6in.32xlarge", "m6in.4xlarge", "m6in.8xlarge", "m6in.large", "m6in.metal", "m6in.xlarge", "m7a.12xlarge", "m7a.16xlarge", "m7a.24xlarge", "m7a.2xlarge", "m7a.32xlarge", "m7a.48xlarge", "m7a.4xlarge", "m7a.8xlarge", "m7a.large", "m7a.medium", "m7a.metal-48xl", "m7a.xlarge", "m7g.12xlarge", "m7g.16xlarge", "m7g.2xlarge", "m7g.4xlarge", "m7g.8xlarge", "m7g.large", "m7g.medium", "m7g.metal", "m7g.xlarge", "m7gd.12xlarge", "m7gd.16xlarge", "m7gd.2xlarge", "m7gd.4xlarge", "m7gd.8xlarge", "m7gd.large", "m7gd.medium", "m7gd.metal", "m7gd.xlarge", "m7i-flex.12xlarge", "m7i-flex.16xlarge", "m7i-flex.2xlarge", "m7i-flex.4xlarge", "m7i-flex.8xlarge", "m7i-flex.large", "m7i-flex.xlarge", "m7i.12xlarge", "m7i.16xlarge", "m7i.24xlarge", "m7i.2xlarge", "m7i.48xlarge", "m7i.4xlarge", "m7i.8xlarge", "m7i.large", "m7i.metal-24xl", "m7i.metal-48xl", "m7i.xlarge", "m8a.12xlarge", "m8a.16xlarge", "m8a.24xlarge", "m8a.2xlarge", "m8a.48xlarge", "m8a.4xlarge", "m8a.8xlarge", "m8a.large", "m8a.medium", "m8a.metal-24xl", "m8a.metal-48xl", "m8a.xlarge", "m8azn.12xlarge", "m8azn.24xlarge", "m8azn.3xlarge", "m8azn.6xlarge", "m8azn.large", "m8azn.medium", "m8azn.metal-12xl", "m8azn.metal-24xl", "m8azn.xlarge", "m8g.12xlarge", "m8g.16xlarge", "m8g.24xlarge", "m8g.2xlarge", "m8g.48xlarge", "m8g.4xlarge", "m8g.8xlarge", "m8g.large", "m8g.medium", "m8g.metal-24xl", "m8g.metal-48xl", "m8g.xlarge", "m8gb.12xlarge", "m8gb.16xlarge", "m8gb.24xlarge", "m8gb.2xlarge", "m8gb.48xlarge", "m8gb.4xlarge", "m8gb.8xlarge", "m8gb.large", "m8gb.medium", "m8gb.metal-24xl", "m8gb.metal-48xl", "m8gb.xlarge", "m8gd.12xlarge", "m8gd.16xlarge", "m8gd.24xlarge", "m8gd.2xlarge", "m8gd.48xlarge", "m8gd.4xlarge", "m8gd.8xlarge", "m8gd.large", "m8gd.medium", "m8gd.metal-24xl", "m8gd.metal-48xl", "m8gd.xlarge", "m8gn.12xlarge", "m8gn.16xlarge", "m8gn.24xlarge", "m8gn.2xlarge", "m8gn.48xlarge", "m8gn.4xlarge", "m8gn.8xlarge", "m8gn.large", "m8gn.medium", "m8gn.metal-24xl", "m8gn.metal-48xl", "m8gn.xlarge", "m8i-flex.12xlarge", "m8i-flex.16xlarge", "m8i-flex.2xlarge", "m8i-flex.4xlarge", "m8i-flex.8xlarge", "m8i-flex.large", "m8i-flex.xlarge", "m8i.12xlarge", "m8i.16xlarge", "m8i.24xlarge", "m8i.2xlarge", "m8i.32xlarge", "m8i.48xlarge", "m8i.4xlarge", "m8i.8xlarge", "m8i.96xlarge", "m8i.large", "m8i.metal-48xl", "m8i.metal-96xl", "m8i.xlarge", "m8ib.12xlarge", "m8ib.16xlarge", "m8ib.24xlarge", "m8ib.2xlarge", "m8ib.32xlarge", "m8ib.48xlarge", "m8ib.4xlarge", "m8ib.8xlarge", "m8ib.96xlarge", "m8ib.large", "m8ib.metal-48xl", "m8ib.metal-96xl", "m8ib.xlarge", "m8id.12xlarge", "m8id.16xlarge", "m8id.24xlarge", "m8id.2xlarge", "m8id.32xlarge", "m8id.48xlarge", "m8id.4xlarge", "m8id.8xlarge", "m8id.96xlarge", "m8id.large", "m8id.metal-48xl", "m8id.metal-96xl", "m8id.xlarge", "m8idb.12xlarge", "m8idb.16xlarge", "m8idb.24xlarge", "m8idb.2xlarge", "m8idb.32xlarge", "m8idb.48xlarge", "m8idb.4xlarge", "m8idb.8xlarge", "m8idb.96xlarge", "m8idb.large", "m8idb.metal-48xl", "m8idb.metal-96xl", "m8idb.xlarge", "m8idn.12xlarge", "m8idn.16xlarge", "m8idn.24xlarge", "m8idn.2xlarge", "m8idn.32xlarge", "m8idn.48xlarge", "m8idn.4xlarge", "m8idn.8xlarge", "m8idn.96xlarge", "m8idn.large", "m8idn.metal-48xl", "m8idn.metal-96xl", "m8idn.xlarge", "m8in.12xlarge", "m8in.16xlarge", "m8in.24xlarge", "m8in.2xlarge", "m8in.32xlarge", "m8in.48xlarge", "m8in.4xlarge", "m8in.8xlarge", "m8in.96xlarge", "m8in.large", "m8in.metal-48xl", "m8in.metal-96xl", "m8in.xlarge", "m8ine.12xlarge", "m8ine.2xlarge", "m8ine.4xlarge", "m8ine.8xlarge", "m8ine.large", "m8ine.xlarge", "m9g.12xlarge", "m9g.16xlarge", "m9g.24xlarge", "m9g.2xlarge", "m9g.48xlarge", "m9g.4xlarge", "m9g.8xlarge", "m9g.large", "m9g.medium", "m9g.metal-24xl", "m9g.metal-48xl", "m9g.xlarge", "m9gd.12xlarge", "m9gd.16xlarge", "m9gd.24xlarge", "m9gd.2xlarge", "m9gd.48xlarge", "m9gd.4xlarge", "m9gd.8xlarge", "m9gd.large", "m9gd.medium", "m9gd.metal-24xl", "m9gd.metal-48xl", "m9gd.xlarge", "mac-m3ultra.metal", "mac-m4.metal", "mac-m4max.metal", "mac-m4pro.metal", "mac1.metal", "mac2-m1ultra.metal", "mac2-m2.metal", "mac2-m2pro.metal", "mac2.metal", "p2.16xlarge", "p2.8xlarge", "p2.xlarge", "p3.16xlarge", "p3.2xlarge", "p3.8xlarge", "p3dn.24xlarge", "p4d.24xlarge", "p4de.24xlarge", "p5.48xlarge", "p5.4xlarge", "p5e.48xlarge", "p5en.48xlarge", "p6-b200.48xlarge", "p6-b300.48xlarge", "p6e-gb200.36xlarge", "r3.2xlarge", "r3.4xlarge", "r3.8xlarge", "r3.large", "r3.xlarge", "r4.16xlarge", "r4.2xlarge", "r4.4xlarge", "r4.8xlarge", "r4.large", "r4.xlarge", "r5.12xlarge", "r5.16xlarge", "r5.24xlarge", "r5.2xlarge", "r5.4xlarge", "r5.8xlarge", "r5.large", "r5.metal", "r5.xlarge", "r5a.12xlarge", "r5a.16xlarge", "r5a.24xlarge", "r5a.2xlarge", "r5a.4xlarge", "r5a.8xlarge", "r5a.large", "r5a.xlarge", "r5ad.12xlarge", "r5ad.16xlarge", "r5ad.24xlarge", "r5ad.2xlarge", "r5ad.4xlarge", "r5ad.8xlarge", "r5ad.large", "r5ad.xlarge", "r5b.12xlarge", "r5b.16xlarge", "r5b.24xlarge", "r5b.2xlarge", "r5b.4xlarge", "r5b.8xlarge", "r5b.large", "r5b.metal", "r5b.xlarge", "r5d.12xlarge", "r5d.16xlarge", "r5d.24xlarge", "r5d.2xlarge", "r5d.4xlarge", "r5d.8xlarge", "r5d.large", "r5d.metal", "r5d.xlarge", "r5dn.12xlarge", "r5dn.16xlarge", "r5dn.24xlarge", "r5dn.2xlarge", "r5dn.4xlarge", "r5dn.8xlarge", "r5dn.large", "r5dn.metal", "r5dn.xlarge", "r5n.12xlarge", "r5n.16xlarge", "r5n.24xlarge", "r5n.2xlarge", "r5n.4xlarge", "r5n.8xlarge", "r5n.large", "r5n.metal", "r5n.xlarge", "r6a.12xlarge", "r6a.16xlarge", "r6a.24xlarge", "r6a.2xlarge", "r6a.32xlarge", "r6a.48xlarge", "r6a.4xlarge", "r6a.8xlarge", "r6a.large", "r6a.metal", "r6a.xlarge", "r6g.12xlarge", "r6g.16xlarge", "r6g.2xlarge", "r6g.4xlarge", "r6g.8xlarge", "r6g.large", "r6g.medium", "r6g.metal", "r6g.xlarge", "r6gd.12xlarge", "r6gd.16xlarge", "r6gd.2xlarge", "r6gd.4xlarge", "r6gd.8xlarge", "r6gd.large", "r6gd.medium", "r6gd.metal", "r6gd.xlarge", "r6i.12xlarge", "r6i.16xlarge", "r6i.24xlarge", "r6i.2xlarge", "r6i.32xlarge", "r6i.4xlarge", "r6i.8xlarge", "r6i.large", "r6i.metal", "r6i.xlarge", "r6id.12xlarge", "r6id.16xlarge", "r6id.24xlarge", "r6id.2xlarge", "r6id.32xlarge", "r6id.4xlarge", "r6id.8xlarge", "r6id.large", "r6id.metal", "r6id.xlarge", "r6idn.12xlarge", "r6idn.16xlarge", "r6idn.24xlarge", "r6idn.2xlarge", "r6idn.32xlarge", "r6idn.4xlarge", "r6idn.8xlarge", "r6idn.large", "r6idn.metal", "r6idn.xlarge", "r6in.12xlarge", "r6in.16xlarge", "r6in.24xlarge", "r6in.2xlarge", "r6in.32xlarge", "r6in.4xlarge", "r6in.8xlarge", "r6in.large", "r6in.metal", "r6in.xlarge", "r7a.12xlarge", "r7a.16xlarge", "r7a.24xlarge", "r7a.2xlarge", "r7a.32xlarge", "r7a.48xlarge", "r7a.4xlarge", "r7a.8xlarge", "r7a.large", "r7a.medium", "r7a.metal-48xl", "r7a.xlarge", "r7g.12xlarge", "r7g.16xlarge", "r7g.2xlarge", "r7g.4xlarge", "r7g.8xlarge", "r7g.large", "r7g.medium", "r7g.metal", "r7g.xlarge", "r7gd.12xlarge", "r7gd.16xlarge", "r7gd.2xlarge", "r7gd.4xlarge", "r7gd.8xlarge", "r7gd.large", "r7gd.medium", "r7gd.metal", "r7gd.xlarge", "r7i.12xlarge", "r7i.16xlarge", "r7i.24xlarge", "r7i.2xlarge", "r7i.48xlarge", "r7i.4xlarge", "r7i.8xlarge", "r7i.large", "r7i.metal-24xl", "r7i.metal-48xl", "r7i.xlarge", "r7iz.12xlarge", "r7iz.16xlarge", "r7iz.2xlarge", "r7iz.32xlarge", "r7iz.4xlarge", "r7iz.8xlarge", "r7iz.large", "r7iz.metal-16xl", "r7iz.metal-32xl", "r7iz.xlarge", "r8a.12xlarge", "r8a.16xlarge", "r8a.24xlarge", "r8a.2xlarge", "r8a.48xlarge", "r8a.4xlarge", "r8a.8xlarge", "r8a.large", "r8a.medium", "r8a.metal-24xl", "r8a.metal-48xl", "r8a.xlarge", "r8g.12xlarge", "r8g.16xlarge", "r8g.24xlarge", "r8g.2xlarge", "r8g.48xlarge", "r8g.4xlarge", "r8g.8xlarge", "r8g.large", "r8g.medium", "r8g.metal-24xl", "r8g.metal-48xl", "r8g.xlarge", "r8gb.12xlarge", "r8gb.16xlarge", "r8gb.24xlarge", "r8gb.2xlarge", "r8gb.48xlarge", "r8gb.4xlarge", "r8gb.8xlarge", "r8gb.large", "r8gb.medium", "r8gb.metal-24xl", "r8gb.metal-48xl", "r8gb.xlarge", "r8gd.12xlarge", "r8gd.16xlarge", "r8gd.24xlarge", "r8gd.2xlarge", "r8gd.48xlarge", "r8gd.4xlarge", "r8gd.8xlarge", "r8gd.large", "r8gd.medium", "r8gd.metal-24xl", "r8gd.metal-48xl", "r8gd.xlarge", "r8gn.12xlarge", "r8gn.16xlarge", "r8gn.24xlarge", "r8gn.2xlarge", "r8gn.48xlarge", "r8gn.4xlarge", "r8gn.8xlarge", "r8gn.large", "r8gn.medium", "r8gn.metal-24xl", "r8gn.metal-48xl", "r8gn.xlarge", "r8i-flex.12xlarge", "r8i-flex.16xlarge", "r8i-flex.2xlarge", "r8i-flex.4xlarge", "r8i-flex.8xlarge", "r8i-flex.large", "r8i-flex.xlarge", "r8i.12xlarge", "r8i.16xlarge", "r8i.24xlarge", "r8i.2xlarge", "r8i.32xlarge", "r8i.48xlarge", "r8i.4xlarge", "r8i.8xlarge", "r8i.96xlarge", "r8i.large", "r8i.metal-48xl", "r8i.metal-96xl", "r8i.xlarge", "r8ib.12xlarge", "r8ib.16xlarge", "r8ib.24xlarge", "r8ib.2xlarge", "r8ib.32xlarge", "r8ib.48xlarge", "r8ib.4xlarge", "r8ib.8xlarge", "r8ib.96xlarge", "r8ib.large", "r8ib.metal-48xl", "r8ib.metal-96xl", "r8ib.xlarge", "r8id.12xlarge", "r8id.16xlarge", "r8id.24xlarge", "r8id.2xlarge", "r8id.32xlarge", "r8id.48xlarge", "r8id.4xlarge", "r8id.8xlarge", "r8id.96xlarge", "r8id.large", "r8id.metal-48xl", "r8id.metal-96xl", "r8id.xlarge", "r8idb.12xlarge", "r8idb.16xlarge", "r8idb.24xlarge", "r8idb.2xlarge", "r8idb.32xlarge", "r8idb.48xlarge", "r8idb.4xlarge", "r8idb.8xlarge", "r8idb.96xlarge", "r8idb.large", "r8idb.metal-48xl", "r8idb.metal-96xl", "r8idb.xlarge", "r8idn.12xlarge", "r8idn.16xlarge", "r8idn.24xlarge", "r8idn.2xlarge", "r8idn.32xlarge", "r8idn.48xlarge", "r8idn.4xlarge", "r8idn.8xlarge", "r8idn.96xlarge", "r8idn.large", "r8idn.metal-48xl", "r8idn.metal-96xl", "r8idn.xlarge", "r8in.12xlarge", "r8in.16xlarge", "r8in.24xlarge", "r8in.2xlarge", "r8in.32xlarge", "r8in.48xlarge", "r8in.4xlarge", "r8in.8xlarge", "r8in.96xlarge", "r8in.large", "r8in.metal-48xl", "r8in.metal-96xl", "r8in.xlarge", "r9g.12xlarge", "r9g.16xlarge", "r9g.24xlarge", "r9g.2xlarge", "r9g.48xlarge", "r9g.4xlarge", "r9g.8xlarge", "r9g.large", "r9g.medium", "r9g.metal-48xl", "r9g.xlarge", "r9gd.12xlarge", "r9gd.16xlarge", "r9gd.24xlarge", "r9gd.2xlarge", "r9gd.48xlarge", "r9gd.4xlarge", "r9gd.8xlarge", "r9gd.large", "r9gd.medium", "r9gd.metal-48xl", "r9gd.xlarge", "t1.micro", "t2.2xlarge", "t2.large", "t2.medium", "t2.micro", "t2.nano", "t2.small", "t2.xlarge", "t3.2xlarge", "t3.large", "t3.medium", "t3.micro", "t3.nano", "t3.small", "t3.xlarge", "t3a.2xlarge", "t3a.large", "t3a.medium", "t3a.micro", "t3a.nano", "t3a.small", "t3a.xlarge", "t4g.2xlarge", "t4g.large", "t4g.medium", "t4g.micro", "t4g.nano", "t4g.small", "t4g.xlarge", "t8i.medium", "t8i.micro", "t8i.nano", "t8i.small", "trn1.2xlarge", "trn1.32xlarge", "trn1n.32xlarge", "trn2.3xlarge", "trn2.48xlarge", "trn2u.48xlarge", "u-12tb1.112xlarge", "u-12tb1.metal", "u-18tb1.112xlarge", "u-18tb1.metal", "u-24tb1.112xlarge", "u-24tb1.metal", "u-3tb1.56xlarge", "u-6tb1.112xlarge", "u-6tb1.56xlarge", "u-6tb1.metal", "u-9tb1.112xlarge", "u-9tb1.metal", "u7i-12tb.224xlarge", "u7i-6tb.112xlarge", "u7i-8tb.112xlarge", "u7ib-12tb.224xlarge", "u7in-16tb.224xlarge", "u7in-24tb.224xlarge", "u7in-32tb.224xlarge", "u7inh-32tb.480xlarge", "vt1.24xlarge", "vt1.3xlarge", "vt1.6xlarge", "x1.16xlarge", "x1.32xlarge", "x1e.16xlarge", "x1e.2xlarge", "x1e.32xlarge", "x1e.4xlarge", "x1e.8xlarge", "x1e.xlarge", "x2gd.12xlarge", "x2gd.16xlarge", "x2gd.2xlarge", "x2gd.4xlarge", "x2gd.8xlarge", "x2gd.large", "x2gd.medium", "x2gd.metal", "x2gd.xlarge", "x2idn.16xlarge", "x2idn.24xlarge", "x2idn.32xlarge", "x2idn.metal", "x2iedn.16xlarge", "x2iedn.24xlarge", "x2iedn.2xlarge", "x2iedn.32xlarge", "x2iedn.4xlarge", "x2iedn.8xlarge", "x2iedn.metal", "x2iedn.xlarge", "x2iezn.12xlarge", "x2iezn.2xlarge", "x2iezn.4xlarge", "x2iezn.6xlarge", "x2iezn.8xlarge", "x2iezn.metal", "x8aedz.12xlarge", "x8aedz.24xlarge", "x8aedz.3xlarge", "x8aedz.6xlarge", "x8aedz.large", "x8aedz.metal-12xl", "x8aedz.metal-24xl", "x8aedz.xlarge", "x8g.12xlarge", "x8g.16xlarge", "x8g.24xlarge", "x8g.2xlarge", "x8g.48xlarge", "x8g.4xlarge", "x8g.8xlarge", "x8g.large", "x8g.medium", "x8g.metal-24xl", "x8g.metal-48xl", "x8g.xlarge", "x8i.12xlarge", "x8i.16xlarge", "x8i.24xlarge", "x8i.2xlarge", "x8i.32xlarge", "x8i.48xlarge", "x8i.4xlarge", "x8i.64xlarge", "x8i.8xlarge", "x8i.96xlarge", "x8i.large", "x8i.metal-48xl", "x8i.metal-96xl", "x8i.xlarge", "z1d.12xlarge", "z1d.2xlarge", "z1d.3xlarge", "z1d.6xlarge", "z1d.large", "z1d.metal", "z1d.xlarge"];


/// The values `InstanceTypeHypervisor` accepts on the wire.
pub const INSTANCE_TYPE_HYPERVISOR_VALUES: &[&str] = &["nitro", "xen"];


/// The values `InterfacePermissionType` accepts on the wire.
pub const INTERFACE_PERMISSION_TYPE_VALUES: &[&str] = &["EIP-ASSOCIATE", "INSTANCE-ATTACH"];


/// The values `InterfaceProtocolType` accepts on the wire.
pub const INTERFACE_PROTOCOL_TYPE_VALUES: &[&str] = &["GRE", "VLAN"];


/// The values `InternetGatewayBlockMode` accepts on the wire.
pub const INTERNET_GATEWAY_BLOCK_MODE_VALUES: &[&str] = &["block-bidirectional", "block-ingress", "off"];


/// The values `InternetGatewayExclusionMode` accepts on the wire.
pub const INTERNET_GATEWAY_EXCLUSION_MODE_VALUES: &[&str] = &["allow-bidirectional", "allow-egress"];


/// The values `InterruptibleCapacityReservationAllocationStatus` accepts on the wire.
pub const INTERRUPTIBLE_CAPACITY_RESERVATION_ALLOCATION_STATUS_VALUES: &[&str] = &["active", "canceled", "canceling", "failed", "pending", "updating"];


/// The values `InterruptionType` accepts on the wire.
pub const INTERRUPTION_TYPE_VALUES: &[&str] = &["adhoc"];


/// The values `IpAddressType` accepts on the wire.
pub const IP_ADDRESS_TYPE_VALUES: &[&str] = &["dualstack", "ipv4", "ipv6"];


/// The values `IpScopeEnum` accepts on the wire.
pub const IP_SCOPE_ENUM_VALUES: &[&str] = &["private"];


/// The values `IpSource` accepts on the wire.
pub const IP_SOURCE_VALUES: &[&str] = &["amazon", "byoip", "none"];


/// The values `IpVersionEnum` accepts on the wire.
pub const IP_VERSION_ENUM_VALUES: &[&str] = &["ipv4", "ipv6"];


/// The values `IpamAddressHistoryResourceType` accepts on the wire.
pub const IPAM_ADDRESS_HISTORY_RESOURCE_TYPE_VALUES: &[&str] = &["eip", "instance", "network-interface", "subnet", "vpc"];


/// The values `IpamAssociatedResourceDiscoveryStatus` accepts on the wire.
pub const IPAM_ASSOCIATED_RESOURCE_DISCOVERY_STATUS_VALUES: &[&str] = &["active", "not-found"];


/// The values `IpamByoipAdvertisementType` accepts on the wire.
pub const IPAM_BYOIP_ADVERTISEMENT_TYPE_VALUES: &[&str] = &["global", "regional"];


/// The values `IpamByoipCidrState` accepts on the wire.
pub const IPAM_BYOIP_CIDR_STATE_VALUES: &[&str] = &["advertised", "deprovisioned", "failed-deprovision", "failed-provision", "pending-advertising", "pending-deprovision", "pending-provision", "pending-withdrawal", "provisioned", "provisioned-not-publicly-advertisable"];


/// The values `IpamComplianceStatus` accepts on the wire.
pub const IPAM_COMPLIANCE_STATUS_VALUES: &[&str] = &["compliant", "ignored", "noncompliant", "unmanaged"];


/// The values `IpamDiscoveryFailureCode` accepts on the wire.
pub const IPAM_DISCOVERY_FAILURE_CODE_VALUES: &[&str] = &["assume-role-failure", "throttling-failure", "unauthorized-failure"];


/// The values `IpamExternalResourceVerificationTokenState` accepts on the wire.
pub const IPAM_EXTERNAL_RESOURCE_VERIFICATION_TOKEN_STATE_VALUES: &[&str] = &["create-complete", "create-failed", "create-in-progress", "delete-complete", "delete-failed", "delete-in-progress"];


/// The values `IpamInternetRegistryAssociationState` accepts on the wire.
pub const IPAM_INTERNET_REGISTRY_ASSOCIATION_STATE_VALUES: &[&str] = &["create-failed", "create-in-progress", "delete-complete", "delete-failed", "delete-in-progress", "disable-complete", "disable-failed", "disable-in-progress", "enable-complete", "enable-failed", "enable-in-progress", "pending-enable"];


/// The values `IpamManagementState` accepts on the wire.
pub const IPAM_MANAGEMENT_STATE_VALUES: &[&str] = &["ignored", "managed", "unmanaged"];


/// The values `IpamMeteredAccount` accepts on the wire.
pub const IPAM_METERED_ACCOUNT_VALUES: &[&str] = &["ipam-owner", "resource-owner"];


/// The values `IpamNetworkInterfaceAttachmentStatus` accepts on the wire.
pub const IPAM_NETWORK_INTERFACE_ATTACHMENT_STATUS_VALUES: &[&str] = &["available", "in-use"];


/// The values `IpamOverlapStatus` accepts on the wire.
pub const IPAM_OVERLAP_STATUS_VALUES: &[&str] = &["ignored", "nonoverlapping", "overlapping"];


/// The values `IpamPolicyManagedBy` accepts on the wire.
pub const IPAM_POLICY_MANAGED_BY_VALUES: &[&str] = &["account", "delegated-administrator-for-ipam"];


/// The values `IpamPolicyResourceType` accepts on the wire.
pub const IPAM_POLICY_RESOURCE_TYPE_VALUES: &[&str] = &["alb", "eip", "rds", "rnat"];


/// The values `IpamPolicyState` accepts on the wire.
pub const IPAM_POLICY_STATE_VALUES: &[&str] = &["create-complete", "create-failed", "create-in-progress", "delete-complete", "delete-failed", "delete-in-progress", "isolate-complete", "isolate-in-progress", "modify-complete", "modify-failed", "modify-in-progress", "restore-in-progress"];


/// The values `IpamPoolAllocationResourceType` accepts on the wire.
pub const IPAM_POOL_ALLOCATION_RESOURCE_TYPE_VALUES: &[&str] = &["anycast-ip-list", "custom", "ec2-public-ipv4-pool", "eip", "ipam-pool", "subnet", "vpc"];


/// The values `IpamPoolAwsService` accepts on the wire.
pub const IPAM_POOL_AWS_SERVICE_VALUES: &[&str] = &["ec2", "global-services"];


/// The values `IpamPoolCidrFailureCode` accepts on the wire.
pub const IPAM_POOL_CIDR_FAILURE_CODE_VALUES: &[&str] = &["cidr-not-available", "limit-exceeded"];


/// The values `IpamPoolCidrState` accepts on the wire.
pub const IPAM_POOL_CIDR_STATE_VALUES: &[&str] = &["deprovisioned", "failed-deprovision", "failed-import", "failed-provision", "pending-deprovision", "pending-import", "pending-provision", "provisioned"];


/// The values `IpamPoolPublicIpSource` accepts on the wire.
pub const IPAM_POOL_PUBLIC_IP_SOURCE_VALUES: &[&str] = &["amazon", "byoip"];


/// The values `IpamPoolSourceResourceType` accepts on the wire.
pub const IPAM_POOL_SOURCE_RESOURCE_TYPE_VALUES: &[&str] = &["vpc"];


/// The values `IpamPoolState` accepts on the wire.
pub const IPAM_POOL_STATE_VALUES: &[&str] = &["create-complete", "create-failed", "create-in-progress", "delete-complete", "delete-failed", "delete-in-progress", "isolate-complete", "isolate-in-progress", "modify-complete", "modify-failed", "modify-in-progress", "restore-in-progress"];


/// The values `IpamPrefixListResolverRuleConditionOperation` accepts on the wire.
pub const IPAM_PREFIX_LIST_RESOLVER_RULE_CONDITION_OPERATION_VALUES: &[&str] = &["equals", "not-equals", "subnet-of"];


/// The values `IpamPrefixListResolverRuleType` accepts on the wire.
pub const IPAM_PREFIX_LIST_RESOLVER_RULE_TYPE_VALUES: &[&str] = &["ipam-pool-cidr", "ipam-resource-cidr", "static-cidr"];


/// The values `IpamPrefixListResolverState` accepts on the wire.
pub const IPAM_PREFIX_LIST_RESOLVER_STATE_VALUES: &[&str] = &["create-complete", "create-failed", "create-in-progress", "delete-complete", "delete-failed", "delete-in-progress", "isolate-complete", "isolate-in-progress", "modify-complete", "modify-failed", "modify-in-progress", "restore-in-progress"];


/// The values `IpamPrefixListResolverTargetState` accepts on the wire.
pub const IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_VALUES: &[&str] = &["create-complete", "create-failed", "create-in-progress", "delete-complete", "delete-failed", "delete-in-progress", "isolate-complete", "isolate-in-progress", "modify-complete", "modify-failed", "modify-in-progress", "restore-in-progress", "sync-complete", "sync-failed", "sync-in-progress"];


/// The values `IpamPrefixListResolverVersionCreationStatus` accepts on the wire.
pub const IPAM_PREFIX_LIST_RESOLVER_VERSION_CREATION_STATUS_VALUES: &[&str] = &["failure", "pending", "success"];


/// The values `IpamPublicAddressAssociationStatus` accepts on the wire.
pub const IPAM_PUBLIC_ADDRESS_ASSOCIATION_STATUS_VALUES: &[&str] = &["associated", "disassociated"];


/// The values `IpamPublicAddressAwsService` accepts on the wire.
pub const IPAM_PUBLIC_ADDRESS_AWS_SERVICE_VALUES: &[&str] = &["cloudfront", "database-migration-service", "elastic-container-service", "global-accelerator", "load-balancer", "nat-gateway", "other", "redshift", "relational-database-service", "site-to-site-vpn"];


/// The values `IpamPublicAddressType` accepts on the wire.
pub const IPAM_PUBLIC_ADDRESS_TYPE_VALUES: &[&str] = &["amazon-owned-contig", "amazon-owned-eip", "anycast-ip-list-ip", "byoip", "ec2-public-ip", "service-managed-byoip", "service-managed-ip"];


/// The values `IpamResourceCidrIpSource` accepts on the wire.
pub const IPAM_RESOURCE_CIDR_IP_SOURCE_VALUES: &[&str] = &["amazon", "byoip", "none"];


/// The values `IpamResourceDiscoveryAssociationState` accepts on the wire.
pub const IPAM_RESOURCE_DISCOVERY_ASSOCIATION_STATE_VALUES: &[&str] = &["associate-complete", "associate-failed", "associate-in-progress", "disassociate-complete", "disassociate-failed", "disassociate-in-progress", "isolate-complete", "isolate-in-progress", "restore-in-progress"];


/// The values `IpamResourceDiscoveryState` accepts on the wire.
pub const IPAM_RESOURCE_DISCOVERY_STATE_VALUES: &[&str] = &["create-complete", "create-failed", "create-in-progress", "delete-complete", "delete-failed", "delete-in-progress", "isolate-complete", "isolate-in-progress", "modify-complete", "modify-failed", "modify-in-progress", "restore-in-progress"];


/// The values `IpamResourceType` accepts on the wire.
pub const IPAM_RESOURCE_TYPE_VALUES: &[&str] = &["anycast-ip-list", "eip", "eni", "ipv6-pool", "public-ipv4-pool", "subnet", "vpc"];


/// The values `IpamRoutingPolicyRegistrationDeltaState` accepts on the wire.
pub const IPAM_ROUTING_POLICY_REGISTRATION_DELTA_STATE_VALUES: &[&str] = &["failed", "pending", "published"];


/// The values `IpamRoutingPolicyRegistrationState` accepts on the wire.
pub const IPAM_ROUTING_POLICY_REGISTRATION_STATE_VALUES: &[&str] = &["activate-failed", "create-complete", "create-in-progress", "delete-complete", "delete-in-progress", "pending-activate", "update-complete", "update-in-progress"];


/// The values `IpamRpkiStatus` accepts on the wire.
pub const IPAM_RPKI_STATUS_VALUES: &[&str] = &["invalid", "unknown", "valid"];


/// The values `IpamRpkiStrength` accepts on the wire.
pub const IPAM_RPKI_STRENGTH_VALUES: &[&str] = &["permissive", "strict"];


/// The values `IpamScopeExternalAuthorityType` accepts on the wire.
pub const IPAM_SCOPE_EXTERNAL_AUTHORITY_TYPE_VALUES: &[&str] = &["infoblox"];


/// The values `IpamScopeState` accepts on the wire.
pub const IPAM_SCOPE_STATE_VALUES: &[&str] = &["create-complete", "create-failed", "create-in-progress", "delete-complete", "delete-failed", "delete-in-progress", "isolate-complete", "isolate-in-progress", "modify-complete", "modify-failed", "modify-in-progress", "restore-in-progress"];


/// The values `IpamScopeType` accepts on the wire.
pub const IPAM_SCOPE_TYPE_VALUES: &[&str] = &["private", "public"];


/// The values `IpamState` accepts on the wire.
pub const IPAM_STATE_VALUES: &[&str] = &["create-complete", "create-failed", "create-in-progress", "delete-complete", "delete-failed", "delete-in-progress", "isolate-complete", "isolate-in-progress", "modify-complete", "modify-failed", "modify-in-progress", "restore-in-progress"];


/// The values `IpamTier` accepts on the wire.
pub const IPAM_TIER_VALUES: &[&str] = &["advanced", "free"];


/// The values `Ipv6AddressAttribute` accepts on the wire.
pub const IPV6_ADDRESS_ATTRIBUTE_VALUES: &[&str] = &["private", "public"];


/// The values `Ipv6SupportValue` accepts on the wire.
pub const IPV6_SUPPORT_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `KeyFormat` accepts on the wire.
pub const KEY_FORMAT_VALUES: &[&str] = &["pem", "ppk"];


/// The values `KeyType` accepts on the wire.
pub const KEY_TYPE_VALUES: &[&str] = &["ed25519", "rsa"];


/// The values `LaunchTemplateAutoRecoveryState` accepts on the wire.
pub const LAUNCH_TEMPLATE_AUTO_RECOVERY_STATE_VALUES: &[&str] = &["default", "disabled"];


/// The values `LaunchTemplateErrorCode` accepts on the wire.
pub const LAUNCH_TEMPLATE_ERROR_CODE_VALUES: &[&str] = &["launchTemplateIdDoesNotExist", "launchTemplateIdMalformed", "launchTemplateNameDoesNotExist", "launchTemplateNameMalformed", "launchTemplateVersionDoesNotExist", "unexpectedError"];


/// The values `LaunchTemplateHttpTokensState` accepts on the wire.
pub const LAUNCH_TEMPLATE_HTTP_TOKENS_STATE_VALUES: &[&str] = &["optional", "required"];


/// The values `LaunchTemplateInstanceMetadataEndpointState` accepts on the wire.
pub const LAUNCH_TEMPLATE_INSTANCE_METADATA_ENDPOINT_STATE_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `LaunchTemplateInstanceMetadataOptionsState` accepts on the wire.
pub const LAUNCH_TEMPLATE_INSTANCE_METADATA_OPTIONS_STATE_VALUES: &[&str] = &["applied", "pending"];


/// The values `LaunchTemplateInstanceMetadataProtocolIpv6` accepts on the wire.
pub const LAUNCH_TEMPLATE_INSTANCE_METADATA_PROTOCOL_IPV6_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `LaunchTemplateInstanceMetadataTagsState` accepts on the wire.
pub const LAUNCH_TEMPLATE_INSTANCE_METADATA_TAGS_STATE_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `ListingState` accepts on the wire.
pub const LISTING_STATE_VALUES: &[&str] = &["available", "cancelled", "pending", "sold"];


/// The values `ListingStatus` accepts on the wire.
pub const LISTING_STATUS_VALUES: &[&str] = &["active", "cancelled", "closed", "pending"];


/// The values `LocalGatewayRouteState` accepts on the wire.
pub const LOCAL_GATEWAY_ROUTE_STATE_VALUES: &[&str] = &["active", "blackhole", "deleted", "deleting", "pending"];


/// The values `LocalGatewayRouteTableMode` accepts on the wire.
pub const LOCAL_GATEWAY_ROUTE_TABLE_MODE_VALUES: &[&str] = &["coip", "direct-vpc-routing"];


/// The values `LocalGatewayRouteType` accepts on the wire.
pub const LOCAL_GATEWAY_ROUTE_TYPE_VALUES: &[&str] = &["propagated", "static"];


/// The values `LocalGatewayVirtualInterfaceConfigurationState` accepts on the wire.
pub const LOCAL_GATEWAY_VIRTUAL_INTERFACE_CONFIGURATION_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "pending"];


/// The values `LocalGatewayVirtualInterfaceGroupConfigurationState` accepts on the wire.
pub const LOCAL_GATEWAY_VIRTUAL_INTERFACE_GROUP_CONFIGURATION_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "incomplete", "pending"];


/// The values `LocalStorage` accepts on the wire.
pub const LOCAL_STORAGE_VALUES: &[&str] = &["excluded", "included", "required"];


/// The values `LocalStorageType` accepts on the wire.
pub const LOCAL_STORAGE_TYPE_VALUES: &[&str] = &["hdd", "ssd"];


/// The values `LocationType` accepts on the wire.
pub const LOCATION_TYPE_VALUES: &[&str] = &["availability-zone", "availability-zone-id", "outpost", "region"];


/// The values `LockMode` accepts on the wire.
pub const LOCK_MODE_VALUES: &[&str] = &["compliance", "governance"];


/// The values `LockState` accepts on the wire.
pub const LOCK_STATE_VALUES: &[&str] = &["compliance", "compliance-cooloff", "expired", "governance"];


/// The values `LogDestinationType` accepts on the wire.
pub const LOG_DESTINATION_TYPE_VALUES: &[&str] = &["cloud-watch-logs", "kinesis-data-firehose", "s3"];


/// The values `MacModificationTaskState` accepts on the wire.
pub const MAC_MODIFICATION_TASK_STATE_VALUES: &[&str] = &["failed", "in-progress", "pending", "successful"];


/// The values `MacModificationTaskType` accepts on the wire.
pub const MAC_MODIFICATION_TASK_TYPE_VALUES: &[&str] = &["sip-modification", "volume-ownership-delegation"];


/// The values `MacSystemIntegrityProtectionSettingStatus` accepts on the wire.
pub const MAC_SYSTEM_INTEGRITY_PROTECTION_SETTING_STATUS_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `ManagedBy` accepts on the wire.
pub const MANAGED_BY_VALUES: &[&str] = &["account", "declarative-policy"];


/// The values `ManagedResourceDefaultVisibility` accepts on the wire.
pub const MANAGED_RESOURCE_DEFAULT_VISIBILITY_VALUES: &[&str] = &["hidden", "visible"];


/// The values `MarketType` accepts on the wire.
pub const MARKET_TYPE_VALUES: &[&str] = &["capacity-block", "interruptible-capacity-reservation", "on-demand", "spot"];


/// The values `MembershipType` accepts on the wire.
pub const MEMBERSHIP_TYPE_VALUES: &[&str] = &["igmp", "static"];


/// The values `MetadataDefaultHttpTokensState` accepts on the wire.
pub const METADATA_DEFAULT_HTTP_TOKENS_STATE_VALUES: &[&str] = &["no-preference", "optional", "required"];


/// The values `Metric` accepts on the wire.
pub const METRIC_VALUES: &[&str] = &["reservation-avg-committed-size-inst", "reservation-avg-committed-size-vcpu", "reservation-avg-future-size-inst", "reservation-avg-future-size-vcpu", "reservation-avg-utilization-inst", "reservation-avg-utilization-vcpu", "reservation-max-committed-size-inst", "reservation-max-committed-size-vcpu", "reservation-max-future-size-inst", "reservation-max-future-size-vcpu", "reservation-max-size-inst", "reservation-max-size-vcpu", "reservation-max-unused-size-inst", "reservation-max-unused-size-vcpu", "reservation-max-utilization", "reservation-min-committed-size-inst", "reservation-min-committed-size-vcpu", "reservation-min-future-size-inst", "reservation-min-future-size-vcpu", "reservation-min-size-inst", "reservation-min-size-vcpu", "reservation-min-unused-size-inst", "reservation-min-unused-size-vcpu", "reservation-min-utilization", "reservation-total-capacity-hrs-inst", "reservation-total-capacity-hrs-vcpu", "reservation-total-count", "reservation-total-estimated-cost", "reservation-unused-total-capacity-hrs-inst", "reservation-unused-total-capacity-hrs-vcpu", "reservation-unused-total-estimated-cost", "reserved-total-estimated-cost", "reserved-total-usage-hrs-inst", "reserved-total-usage-hrs-vcpu", "spot-avg-run-time-before-interruption-inst", "spot-interruption-rate-inst", "spot-interruption-rate-vcpu", "spot-max-run-time-before-interruption-inst", "spot-min-run-time-before-interruption-inst", "spot-total-count-inst", "spot-total-count-vcpu", "spot-total-estimated-cost", "spot-total-interruptions-inst", "spot-total-interruptions-vcpu", "spot-total-usage-hrs-inst", "spot-total-usage-hrs-vcpu", "unreserved-total-estimated-cost", "unreserved-total-usage-hrs-inst", "unreserved-total-usage-hrs-vcpu"];


/// The values `MetricType` accepts on the wire.
pub const METRIC_TYPE_VALUES: &[&str] = &["aggregate-latency"];


/// The values `ModifyAvailabilityZoneOptInStatus` accepts on the wire.
pub const MODIFY_AVAILABILITY_ZONE_OPT_IN_STATUS_VALUES: &[&str] = &["not-opted-in", "opted-in"];


/// The values `MonitoringState` accepts on the wire.
pub const MONITORING_STATE_VALUES: &[&str] = &["disabled", "disabling", "enabled", "pending"];


/// The values `MoveStatus` accepts on the wire.
pub const MOVE_STATUS_VALUES: &[&str] = &["movingToVpc", "restoringToClassic"];


/// The values `MulticastSupportValue` accepts on the wire.
pub const MULTICAST_SUPPORT_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `NatGatewayAddressStatus` accepts on the wire.
pub const NAT_GATEWAY_ADDRESS_STATUS_VALUES: &[&str] = &["assigning", "associating", "disassociating", "failed", "succeeded", "unassigning"];


/// The values `NatGatewayApplianceModifyState` accepts on the wire.
pub const NAT_GATEWAY_APPLIANCE_MODIFY_STATE_VALUES: &[&str] = &["completed", "failed", "modifying"];


/// The values `NatGatewayApplianceState` accepts on the wire.
pub const NAT_GATEWAY_APPLIANCE_STATE_VALUES: &[&str] = &["attach-failed", "attached", "attaching", "detach-failed", "detached", "detaching"];


/// The values `NatGatewayApplianceType` accepts on the wire.
pub const NAT_GATEWAY_APPLIANCE_TYPE_VALUES: &[&str] = &["network-firewall-proxy"];


/// The values `NatGatewayState` accepts on the wire.
pub const NAT_GATEWAY_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "failed", "pending"];


/// The values `NestedVirtualizationSpecification` accepts on the wire.
pub const NESTED_VIRTUALIZATION_SPECIFICATION_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `NetworkCardInterfaceType` accepts on the wire.
pub const NETWORK_CARD_INTERFACE_TYPE_VALUES: &[&str] = &["efa", "efa-only", "interface", "secondary"];


/// The values `NetworkInterfaceAttribute` accepts on the wire.
pub const NETWORK_INTERFACE_ATTRIBUTE_VALUES: &[&str] = &["associatePublicIpAddress", "attachment", "description", "groupSet", "sourceDestCheck"];


/// The values `NetworkInterfaceCreationType` accepts on the wire.
pub const NETWORK_INTERFACE_CREATION_TYPE_VALUES: &[&str] = &["branch", "efa", "efa-only", "trunk"];


/// The values `NetworkInterfacePermissionStateCode` accepts on the wire.
pub const NETWORK_INTERFACE_PERMISSION_STATE_CODE_VALUES: &[&str] = &["granted", "pending", "revoked", "revoking"];


/// The values `NetworkInterfaceStatus` accepts on the wire.
pub const NETWORK_INTERFACE_STATUS_VALUES: &[&str] = &["associated", "attaching", "available", "detaching", "in-use"];


/// The values `NetworkInterfaceType` accepts on the wire.
pub const NETWORK_INTERFACE_TYPE_VALUES: &[&str] = &["api_gateway_managed", "aws_codestar_connections_managed", "branch", "efa", "efa-only", "gateway_load_balancer", "gateway_load_balancer_endpoint", "global_accelerator_managed", "interface", "iot_rules_managed", "lambda", "load_balancer", "natGateway", "network_load_balancer", "quicksight", "transit_gateway", "trunk", "vpc_endpoint"];


/// The values `NetworkProtocolEnum` accepts on the wire.
pub const NETWORK_PROTOCOL_ENUM_VALUES: &[&str] = &["http", "https"];


/// The values `NitroEnclavesSupport` accepts on the wire.
pub const NITRO_ENCLAVES_SUPPORT_VALUES: &[&str] = &["supported", "unsupported"];


/// The values `NitroTpmSupport` accepts on the wire.
pub const NITRO_TPM_SUPPORT_VALUES: &[&str] = &["supported", "unsupported"];


/// The values `OfferingClassType` accepts on the wire.
pub const OFFERING_CLASS_TYPE_VALUES: &[&str] = &["convertible", "standard"];


/// The values `OfferingTypeValues` accepts on the wire.
pub const OFFERING_TYPE_VALUES_VALUES: &[&str] = &["All Upfront", "Heavy Utilization", "Light Utilization", "Medium Utilization", "No Upfront", "Partial Upfront"];


/// The values `OnDemandAllocationStrategy` accepts on the wire.
pub const ON_DEMAND_ALLOCATION_STRATEGY_VALUES: &[&str] = &["lowestPrice", "prioritized"];


/// The values `OperationType` accepts on the wire.
pub const OPERATION_TYPE_VALUES: &[&str] = &["add", "remove"];


/// The values `OutputFormat` accepts on the wire.
pub const OUTPUT_FORMAT_VALUES: &[&str] = &["csv", "parquet"];


/// The values `PartitionLoadFrequency` accepts on the wire.
pub const PARTITION_LOAD_FREQUENCY_VALUES: &[&str] = &["daily", "monthly", "none", "weekly"];


/// The values `PayerResponsibility` accepts on the wire.
pub const PAYER_RESPONSIBILITY_VALUES: &[&str] = &["ServiceOwner"];


/// The values `PayerResponsibilityScope` accepts on the wire.
pub const PAYER_RESPONSIBILITY_SCOPE_VALUES: &[&str] = &["resource-gateway-charges", "vpc-endpoint-charges"];


/// The values `PayerResponsibilityType` accepts on the wire.
pub const PAYER_RESPONSIBILITY_TYPE_VALUES: &[&str] = &["resource-gateway-account", "vpc-endpoint-account", "vpc-endpoint-service-account"];


/// The values `PaymentOption` accepts on the wire.
pub const PAYMENT_OPTION_VALUES: &[&str] = &["AllUpfront", "NoUpfront", "PartialUpfront"];


/// The values `PeriodType` accepts on the wire.
pub const PERIOD_TYPE_VALUES: &[&str] = &["fifteen-minutes", "five-minutes", "one-day", "one-hour", "one-week", "three-hours"];


/// The values `PermissionGroup` accepts on the wire.
pub const PERMISSION_GROUP_VALUES: &[&str] = &["all"];


/// The values `PhcSupport` accepts on the wire.
pub const PHC_SUPPORT_VALUES: &[&str] = &["supported", "unsupported"];


/// The values `PlacementGroupState` accepts on the wire.
pub const PLACEMENT_GROUP_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "pending"];


/// The values `PlacementGroupStrategy` accepts on the wire.
pub const PLACEMENT_GROUP_STRATEGY_VALUES: &[&str] = &["cluster", "partition", "spread"];


/// The values `PlacementStrategy` accepts on the wire.
pub const PLACEMENT_STRATEGY_VALUES: &[&str] = &["cluster", "partition", "precision-time", "spread"];


/// The values `PlatformValues` accepts on the wire.
pub const PLATFORM_VALUES_VALUES: &[&str] = &["Windows"];


/// The values `PrefixListState` accepts on the wire.
pub const PREFIX_LIST_STATE_VALUES: &[&str] = &["create-complete", "create-failed", "create-in-progress", "delete-complete", "delete-failed", "delete-in-progress", "modify-complete", "modify-failed", "modify-in-progress", "restore-complete", "restore-failed", "restore-in-progress"];


/// The values `PrincipalType` accepts on the wire.
pub const PRINCIPAL_TYPE_VALUES: &[&str] = &["Account", "All", "OrganizationUnit", "Role", "Service", "User"];


/// The values `ProductCodeValues` accepts on the wire.
pub const PRODUCT_CODE_VALUES_VALUES: &[&str] = &["devpay", "marketplace"];


/// The values `Protocol` accepts on the wire.
pub const PROTOCOL_VALUES: &[&str] = &["tcp", "udp"];


/// The values `ProtocolValue` accepts on the wire.
pub const PROTOCOL_VALUE_VALUES: &[&str] = &["gre"];


/// The values `PublicIpDnsOption` accepts on the wire.
pub const PUBLIC_IP_DNS_OPTION_VALUES: &[&str] = &["public-dual-stack-dns-name", "public-ipv4-dns-name", "public-ipv6-dns-name"];


/// The values `RIProductDescription` accepts on the wire.
pub const R_I_PRODUCT_DESCRIPTION_VALUES: &[&str] = &["Linux/UNIX", "Linux/UNIX (Amazon VPC)", "Windows", "Windows (Amazon VPC)"];


/// The values `RebootMigrationSupport` accepts on the wire.
pub const REBOOT_MIGRATION_SUPPORT_VALUES: &[&str] = &["supported", "unsupported"];


/// The values `RecurringChargeFrequency` accepts on the wire.
pub const RECURRING_CHARGE_FREQUENCY_VALUES: &[&str] = &["Hourly"];


/// The values `ReplaceRootVolumeTaskState` accepts on the wire.
pub const REPLACE_ROOT_VOLUME_TASK_STATE_VALUES: &[&str] = &["failed", "failed-detached", "failing", "in-progress", "pending", "succeeded"];


/// The values `ReplacementStrategy` accepts on the wire.
pub const REPLACEMENT_STRATEGY_VALUES: &[&str] = &["launch", "launch-before-terminate"];


/// The values `ReportInstanceReasonCodes` accepts on the wire.
pub const REPORT_INSTANCE_REASON_CODES_VALUES: &[&str] = &["instance-stuck-in-state", "not-accepting-credentials", "other", "password-not-available", "performance-ebs-volume", "performance-instance-store", "performance-network", "performance-other", "unresponsive"];


/// The values `ReportState` accepts on the wire.
pub const REPORT_STATE_VALUES: &[&str] = &["cancelled", "complete", "error", "running"];


/// The values `ReportStatusType` accepts on the wire.
pub const REPORT_STATUS_TYPE_VALUES: &[&str] = &["impaired", "ok"];


/// The values `ReservationEndDateType` accepts on the wire.
pub const RESERVATION_END_DATE_TYPE_VALUES: &[&str] = &["limited", "unlimited"];


/// The values `ReservationState` accepts on the wire.
pub const RESERVATION_STATE_VALUES: &[&str] = &["active", "cancelled", "delayed", "expired", "failed", "payment-failed", "payment-pending", "pending", "retired", "scheduled", "unsupported"];


/// The values `ReservationType` accepts on the wire.
pub const RESERVATION_TYPE_VALUES: &[&str] = &["capacity-block", "odcr"];


/// The values `ReservedCapacityAllocationStrategy` accepts on the wire.
pub const RESERVED_CAPACITY_ALLOCATION_STRATEGY_VALUES: &[&str] = &["prioritized"];


/// The values `ReservedCapacityFallbackMarketType` accepts on the wire.
pub const RESERVED_CAPACITY_FALLBACK_MARKET_TYPE_VALUES: &[&str] = &["on-demand"];


/// The values `ReservedInstanceState` accepts on the wire.
pub const RESERVED_INSTANCE_STATE_VALUES: &[&str] = &["active", "payment-failed", "payment-pending", "queued", "queued-deleted", "retired"];


/// The values `ResetFpgaImageAttributeName` accepts on the wire.
pub const RESET_FPGA_IMAGE_ATTRIBUTE_NAME_VALUES: &[&str] = &["loadPermission"];


/// The values `ResetImageAttributeName` accepts on the wire.
pub const RESET_IMAGE_ATTRIBUTE_NAME_VALUES: &[&str] = &["launchPermission"];


/// The values `ResourceType` accepts on the wire.
pub const RESOURCE_TYPE_VALUES: &[&str] = &["application-status-check", "capacity-block", "capacity-manager-data-export", "capacity-reservation", "capacity-reservation-cancellation-quote", "capacity-reservation-fleet", "capacity-reservation-modification-quote", "carrier-gateway", "client-vpn-endpoint", "coip-pool", "customer-gateway", "declarative-policies-report", "dedicated-host", "dhcp-options", "egress-only-internet-gateway", "elastic-gpu", "elastic-ip", "export-image-task", "export-instance-task", "fleet", "fpga-image", "host-reservation", "image", "image-usage-report", "import-image-task", "import-snapshot-task", "instance", "instance-connect-endpoint", "instance-event-window", "internet-gateway", "ipam", "ipam-external-resource-verification-token", "ipam-internet-registry-association", "ipam-policy", "ipam-pool", "ipam-pool-allocation", "ipam-prefix-list-resolver", "ipam-prefix-list-resolver-target", "ipam-resource-discovery", "ipam-resource-discovery-association", "ipam-scope", "ipv4pool-ec2", "ipv6pool-ec2", "key-pair", "launch-template", "local-gateway", "local-gateway-route-table", "local-gateway-route-table-virtual-interface-group-association", "local-gateway-route-table-vpc-association", "local-gateway-virtual-interface", "local-gateway-virtual-interface-group", "mac-modification-task", "natgateway", "network-acl", "network-insights-access-scope", "network-insights-access-scope-analysis", "network-insights-analysis", "network-insights-path", "network-interface", "outpost-lag", "placement-group", "prefix-list", "replace-root-volume-task", "reserved-instances", "route-server", "route-server-endpoint", "route-server-peer", "route-table", "secondary-interface", "secondary-network", "secondary-subnet", "security-group", "security-group-rule", "service-link-virtual-interface", "snapshot", "spot-fleet-request", "spot-instances-request", "subnet", "subnet-cidr-reservation", "traffic-mirror-filter", "traffic-mirror-filter-rule", "traffic-mirror-session", "traffic-mirror-target", "transit-gateway", "transit-gateway-attachment", "transit-gateway-connect-peer", "transit-gateway-metering-policy", "transit-gateway-multicast-domain", "transit-gateway-policy-table", "transit-gateway-route-table", "transit-gateway-route-table-announcement", "verified-access-endpoint", "verified-access-endpoint-target", "verified-access-group", "verified-access-instance", "verified-access-policy", "verified-access-trust-provider", "volume", "vpc", "vpc-block-public-access-exclusion", "vpc-encryption-control", "vpc-endpoint", "vpc-endpoint-connection", "vpc-endpoint-connection-device-type", "vpc-endpoint-service", "vpc-endpoint-service-permission", "vpc-flow-log", "vpc-peering-connection", "vpn-concentrator", "vpn-connection", "vpn-connection-device-type", "vpn-gateway"];


/// The values `Rir` accepts on the wire.
pub const RIR_VALUES: &[&str] = &["apnic", "arin", "lacnic", "nicbr", "ripe"];


/// The values `RootDeviceType` accepts on the wire.
pub const ROOT_DEVICE_TYPE_VALUES: &[&str] = &["ebs", "instance-store"];


/// The values `RouteOrigin` accepts on the wire.
pub const ROUTE_ORIGIN_VALUES: &[&str] = &["Advertisement", "CreateRoute", "CreateRouteTable", "EnableVgwRoutePropagation"];


/// The values `RouteServerAssociationState` accepts on the wire.
pub const ROUTE_SERVER_ASSOCIATION_STATE_VALUES: &[&str] = &["associated", "associating", "disassociating"];


/// The values `RouteServerBfdState` accepts on the wire.
pub const ROUTE_SERVER_BFD_STATE_VALUES: &[&str] = &["down", "up"];


/// The values `RouteServerBgpState` accepts on the wire.
pub const ROUTE_SERVER_BGP_STATE_VALUES: &[&str] = &["down", "up"];


/// The values `RouteServerEndpointState` accepts on the wire.
pub const ROUTE_SERVER_ENDPOINT_STATE_VALUES: &[&str] = &["available", "delete-failed", "deleted", "deleting", "failed", "failing", "pending"];


/// The values `RouteServerPeerLivenessMode` accepts on the wire.
pub const ROUTE_SERVER_PEER_LIVENESS_MODE_VALUES: &[&str] = &["bfd", "bgp-keepalive"];


/// The values `RouteServerPeerState` accepts on the wire.
pub const ROUTE_SERVER_PEER_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "failed", "failing", "pending"];


/// The values `RouteServerPersistRoutesAction` accepts on the wire.
pub const ROUTE_SERVER_PERSIST_ROUTES_ACTION_VALUES: &[&str] = &["disable", "enable", "reset"];


/// The values `RouteServerPersistRoutesState` accepts on the wire.
pub const ROUTE_SERVER_PERSIST_ROUTES_STATE_VALUES: &[&str] = &["disabled", "disabling", "enabled", "enabling", "modifying", "resetting"];


/// The values `RouteServerPropagationState` accepts on the wire.
pub const ROUTE_SERVER_PROPAGATION_STATE_VALUES: &[&str] = &["available", "deleting", "pending"];


/// The values `RouteServerRouteInstallationStatus` accepts on the wire.
pub const ROUTE_SERVER_ROUTE_INSTALLATION_STATUS_VALUES: &[&str] = &["installed", "rejected"];


/// The values `RouteServerRouteStatus` accepts on the wire.
pub const ROUTE_SERVER_ROUTE_STATUS_VALUES: &[&str] = &["in-fib", "in-rib"];


/// The values `RouteServerState` accepts on the wire.
pub const ROUTE_SERVER_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "modifying", "pending"];


/// The values `RouteState` accepts on the wire.
pub const ROUTE_STATE_VALUES: &[&str] = &["active", "blackhole", "filtered"];


/// The values `RouteTableAssociationStateCode` accepts on the wire.
pub const ROUTE_TABLE_ASSOCIATION_STATE_CODE_VALUES: &[&str] = &["associated", "associating", "disassociated", "disassociating", "failed"];


/// The values `RuleAction` accepts on the wire.
pub const RULE_ACTION_VALUES: &[&str] = &["allow", "deny"];


/// The values `SSEType` accepts on the wire.
pub const S_S_E_TYPE_VALUES: &[&str] = &["none", "sse-ebs", "sse-kms"];


/// The values `Schedule` accepts on the wire.
pub const SCHEDULE_VALUES: &[&str] = &["hourly"];


/// The values `SecondaryInterfaceStatus` accepts on the wire.
pub const SECONDARY_INTERFACE_STATUS_VALUES: &[&str] = &["available", "in-use"];


/// The values `SecondaryInterfaceType` accepts on the wire.
pub const SECONDARY_INTERFACE_TYPE_VALUES: &[&str] = &["secondary"];


/// The values `SecondaryNetworkCidrBlockAssociationState` accepts on the wire.
pub const SECONDARY_NETWORK_CIDR_BLOCK_ASSOCIATION_STATE_VALUES: &[&str] = &["associated", "associating", "association-failed", "disassociated", "disassociating", "disassociation-failed"];


/// The values `SecondaryNetworkState` accepts on the wire.
pub const SECONDARY_NETWORK_STATE_VALUES: &[&str] = &["create-complete", "create-failed", "create-in-progress", "delete-complete", "delete-failed", "delete-in-progress"];


/// The values `SecondaryNetworkType` accepts on the wire.
pub const SECONDARY_NETWORK_TYPE_VALUES: &[&str] = &["rdma"];


/// The values `SecondarySubnetCidrBlockAssociationState` accepts on the wire.
pub const SECONDARY_SUBNET_CIDR_BLOCK_ASSOCIATION_STATE_VALUES: &[&str] = &["associated", "associating", "association-failed", "disassociated", "disassociating", "disassociation-failed"];


/// The values `SecondarySubnetState` accepts on the wire.
pub const SECONDARY_SUBNET_STATE_VALUES: &[&str] = &["create-complete", "create-failed", "create-in-progress", "delete-complete", "delete-failed", "delete-in-progress"];


/// The values `SecurityGroupReferencingSupportValue` accepts on the wire.
pub const SECURITY_GROUP_REFERENCING_SUPPORT_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `SecurityGroupVpcAssociationState` accepts on the wire.
pub const SECURITY_GROUP_VPC_ASSOCIATION_STATE_VALUES: &[&str] = &["associated", "associating", "association-failed", "disassociated", "disassociating", "disassociation-failed"];


/// The values `SelfServicePortal` accepts on the wire.
pub const SELF_SERVICE_PORTAL_VALUES: &[&str] = &["disabled", "enabled"];


/// The values `ServiceConnectivityType` accepts on the wire.
pub const SERVICE_CONNECTIVITY_TYPE_VALUES: &[&str] = &["ipv4", "ipv6"];


/// The values `ServiceLinkVirtualInterfaceConfigurationState` accepts on the wire.
pub const SERVICE_LINK_VIRTUAL_INTERFACE_CONFIGURATION_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "pending"];


/// The values `ServiceManaged` accepts on the wire.
pub const SERVICE_MANAGED_VALUES: &[&str] = &["alb", "nlb", "rds", "rnat"];


/// The values `ServiceState` accepts on the wire.
pub const SERVICE_STATE_VALUES: &[&str] = &["Available", "Deleted", "Deleting", "Failed", "Pending"];


/// The values `ServiceType` accepts on the wire.
pub const SERVICE_TYPE_VALUES: &[&str] = &["Gateway", "GatewayLoadBalancer", "Interface"];


/// The values `ShutdownBehavior` accepts on the wire.
pub const SHUTDOWN_BEHAVIOR_VALUES: &[&str] = &["stop", "terminate"];


/// The values `SnapshotAttributeName` accepts on the wire.
pub const SNAPSHOT_ATTRIBUTE_NAME_VALUES: &[&str] = &["createVolumePermission", "productCodes"];


/// The values `SnapshotBlockPublicAccessState` accepts on the wire.
pub const SNAPSHOT_BLOCK_PUBLIC_ACCESS_STATE_VALUES: &[&str] = &["block-all-sharing", "block-new-sharing", "unblocked"];


/// The values `SnapshotLocationEnum` accepts on the wire.
pub const SNAPSHOT_LOCATION_ENUM_VALUES: &[&str] = &["local", "regional"];


/// The values `SnapshotReturnCodes` accepts on the wire.
pub const SNAPSHOT_RETURN_CODES_VALUES: &[&str] = &["client-error", "internal-error", "missing-permissions", "skipped", "success"];


/// The values `SnapshotState` accepts on the wire.
pub const SNAPSHOT_STATE_VALUES: &[&str] = &["completed", "error", "pending", "recoverable", "recovering"];


/// The values `SpotAllocationStrategy` accepts on the wire.
pub const SPOT_ALLOCATION_STRATEGY_VALUES: &[&str] = &["capacity-optimized", "capacity-optimized-prioritized", "diversified", "lowest-price", "price-capacity-optimized"];


/// The values `SpotInstanceInterruptionBehavior` accepts on the wire.
pub const SPOT_INSTANCE_INTERRUPTION_BEHAVIOR_VALUES: &[&str] = &["hibernate", "stop", "terminate"];


/// The values `SpotInstanceState` accepts on the wire.
pub const SPOT_INSTANCE_STATE_VALUES: &[&str] = &["active", "cancelled", "closed", "disabled", "failed", "open"];


/// The values `SpotInstanceType` accepts on the wire.
pub const SPOT_INSTANCE_TYPE_VALUES: &[&str] = &["one-time", "persistent"];


/// The values `SpreadLevel` accepts on the wire.
pub const SPREAD_LEVEL_VALUES: &[&str] = &["host", "rack"];


/// The values `SqlServerLicenseUsage` accepts on the wire.
pub const SQL_SERVER_LICENSE_USAGE_VALUES: &[&str] = &["full", "waived"];


/// The values `State` accepts on the wire.
pub const STATE_VALUES: &[&str] = &["Available", "Deleted", "Deleting", "Expired", "Failed", "Partial", "Pending", "PendingAcceptance", "Rejected"];


/// The values `StaticSourcesSupportValue` accepts on the wire.
pub const STATIC_SOURCES_SUPPORT_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `StatisticType` accepts on the wire.
pub const STATISTIC_TYPE_VALUES: &[&str] = &["p50"];


/// The values `Status` accepts on the wire.
pub const STATUS_VALUES: &[&str] = &["InClassic", "InVpc", "MoveInProgress"];


/// The values `StatusName` accepts on the wire.
pub const STATUS_NAME_VALUES: &[&str] = &["reachability"];


/// The values `StatusType` accepts on the wire.
pub const STATUS_TYPE_VALUES: &[&str] = &["failed", "initializing", "insufficient-data", "passed"];


/// The values `StorageTier` accepts on the wire.
pub const STORAGE_TIER_VALUES: &[&str] = &["archive", "standard"];


/// The values `SubnetCidrBlockStateCode` accepts on the wire.
pub const SUBNET_CIDR_BLOCK_STATE_CODE_VALUES: &[&str] = &["associated", "associating", "disassociated", "disassociating", "failed", "failing"];


/// The values `SubnetCidrReservationType` accepts on the wire.
pub const SUBNET_CIDR_RESERVATION_TYPE_VALUES: &[&str] = &["explicit", "prefix"];


/// The values `SubnetState` accepts on the wire.
pub const SUBNET_STATE_VALUES: &[&str] = &["available", "failed", "failed-insufficient-capacity", "pending", "unavailable"];


/// The values `SummaryStatus` accepts on the wire.
pub const SUMMARY_STATUS_VALUES: &[&str] = &["impaired", "initializing", "insufficient-data", "not-applicable", "ok"];


/// The values `SupportedAdditionalProcessorFeature` accepts on the wire.
pub const SUPPORTED_ADDITIONAL_PROCESSOR_FEATURE_VALUES: &[&str] = &["amd-sev-snp", "nested-virtualization"];


/// The values `TaggableResourceType` accepts on the wire.
pub const TAGGABLE_RESOURCE_TYPE_VALUES: &[&str] = &["auto-scaling-group", "instance", "network-interface"];


/// The values `TargetCapacityUnitType` accepts on the wire.
pub const TARGET_CAPACITY_UNIT_TYPE_VALUES: &[&str] = &["memory-mib", "units", "vcpu"];


/// The values `TargetStorageTier` accepts on the wire.
pub const TARGET_STORAGE_TIER_VALUES: &[&str] = &["archive"];


/// The values `TelemetryStatus` accepts on the wire.
pub const TELEMETRY_STATUS_VALUES: &[&str] = &["DOWN", "UP"];


/// The values `Tenancy` accepts on the wire.
pub const TENANCY_VALUES: &[&str] = &["dedicated", "default", "host"];


/// The values `TieringOperationStatus` accepts on the wire.
pub const TIERING_OPERATION_STATUS_VALUES: &[&str] = &["archival-completed", "archival-failed", "archival-in-progress", "permanent-restore-completed", "permanent-restore-failed", "permanent-restore-in-progress", "temporary-restore-completed", "temporary-restore-failed", "temporary-restore-in-progress"];


/// The values `TokenState` accepts on the wire.
pub const TOKEN_STATE_VALUES: &[&str] = &["expired", "valid"];


/// The values `TpmSupportValues` accepts on the wire.
pub const TPM_SUPPORT_VALUES_VALUES: &[&str] = &["v2.0"];


/// The values `TrafficDirection` accepts on the wire.
pub const TRAFFIC_DIRECTION_VALUES: &[&str] = &["egress", "ingress"];


/// The values `TrafficIpAddressType` accepts on the wire.
pub const TRAFFIC_IP_ADDRESS_TYPE_VALUES: &[&str] = &["dual-stack", "ipv4", "ipv6"];


/// The values `TrafficMirrorFilterRuleField` accepts on the wire.
pub const TRAFFIC_MIRROR_FILTER_RULE_FIELD_VALUES: &[&str] = &["description", "destination-port-range", "protocol", "source-port-range"];


/// The values `TrafficMirrorNetworkService` accepts on the wire.
pub const TRAFFIC_MIRROR_NETWORK_SERVICE_VALUES: &[&str] = &["amazon-dns"];


/// The values `TrafficMirrorRuleAction` accepts on the wire.
pub const TRAFFIC_MIRROR_RULE_ACTION_VALUES: &[&str] = &["accept", "reject"];


/// The values `TrafficMirrorSessionField` accepts on the wire.
pub const TRAFFIC_MIRROR_SESSION_FIELD_VALUES: &[&str] = &["description", "packet-length", "virtual-network-id"];


/// The values `TrafficMirrorTargetType` accepts on the wire.
pub const TRAFFIC_MIRROR_TARGET_TYPE_VALUES: &[&str] = &["gateway-load-balancer-endpoint", "network-interface", "network-load-balancer"];


/// The values `TrafficType` accepts on the wire.
pub const TRAFFIC_TYPE_VALUES: &[&str] = &["ACCEPT", "ALL", "REJECT"];


/// The values `TransferType` accepts on the wire.
pub const TRANSFER_TYPE_VALUES: &[&str] = &["standard", "time-based"];


/// The values `TransitGatewayAssociationState` accepts on the wire.
pub const TRANSIT_GATEWAY_ASSOCIATION_STATE_VALUES: &[&str] = &["associated", "associating", "disassociated", "disassociating"];


/// The values `TransitGatewayAttachmentResourceType` accepts on the wire.
pub const TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_VALUES: &[&str] = &["client-vpn", "connect", "direct-connect-gateway", "network-function", "peering", "tgw-peering", "vpc", "vpn", "vpn-concentrator"];


/// The values `TransitGatewayAttachmentState` accepts on the wire.
pub const TRANSIT_GATEWAY_ATTACHMENT_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "failed", "failing", "initiating", "initiatingRequest", "modifying", "pending", "pendingAcceptance", "rejected", "rejecting", "rollingBack"];


/// The values `TransitGatewayAttachmentStatusType` accepts on the wire.
pub const TRANSIT_GATEWAY_ATTACHMENT_STATUS_TYPE_VALUES: &[&str] = &["available", "deleted", "deleting", "pending", "pending-acceptance", "rejected"];


/// The values `TransitGatewayConnectPeerState` accepts on the wire.
pub const TRANSIT_GATEWAY_CONNECT_PEER_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "pending"];


/// The values `TransitGatewayMeteringPayerType` accepts on the wire.
pub const TRANSIT_GATEWAY_METERING_PAYER_TYPE_VALUES: &[&str] = &["destination-attachment-owner", "source-attachment-owner", "transit-gateway-owner"];


/// The values `TransitGatewayMeteringPolicyEntryState` accepts on the wire.
pub const TRANSIT_GATEWAY_METERING_POLICY_ENTRY_STATE_VALUES: &[&str] = &["available", "deleted"];


/// The values `TransitGatewayMeteringPolicyState` accepts on the wire.
pub const TRANSIT_GATEWAY_METERING_POLICY_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "modifying", "pending"];


/// The values `TransitGatewayMulitcastDomainAssociationState` accepts on the wire.
pub const TRANSIT_GATEWAY_MULITCAST_DOMAIN_ASSOCIATION_STATE_VALUES: &[&str] = &["associated", "associating", "disassociated", "disassociating", "failed", "pendingAcceptance", "rejected"];


/// The values `TransitGatewayMulticastDomainState` accepts on the wire.
pub const TRANSIT_GATEWAY_MULTICAST_DOMAIN_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "pending"];


/// The values `TransitGatewayPolicyTableEntryState` accepts on the wire.
pub const TRANSIT_GATEWAY_POLICY_TABLE_ENTRY_STATE_VALUES: &[&str] = &["active", "deleted"];


/// The values `TransitGatewayPolicyTableState` accepts on the wire.
pub const TRANSIT_GATEWAY_POLICY_TABLE_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "pending"];


/// The values `TransitGatewayPrefixListReferenceState` accepts on the wire.
pub const TRANSIT_GATEWAY_PREFIX_LIST_REFERENCE_STATE_VALUES: &[&str] = &["available", "deleting", "modifying", "pending"];


/// The values `TransitGatewayPropagationState` accepts on the wire.
pub const TRANSIT_GATEWAY_PROPAGATION_STATE_VALUES: &[&str] = &["disabled", "disabling", "enabled", "enabling"];


/// The values `TransitGatewayRouteState` accepts on the wire.
pub const TRANSIT_GATEWAY_ROUTE_STATE_VALUES: &[&str] = &["active", "blackhole", "deleted", "deleting", "pending"];


/// The values `TransitGatewayRouteTableAnnouncementDirection` accepts on the wire.
pub const TRANSIT_GATEWAY_ROUTE_TABLE_ANNOUNCEMENT_DIRECTION_VALUES: &[&str] = &["incoming", "outgoing"];


/// The values `TransitGatewayRouteTableAnnouncementState` accepts on the wire.
pub const TRANSIT_GATEWAY_ROUTE_TABLE_ANNOUNCEMENT_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "failed", "failing", "pending"];


/// The values `TransitGatewayRouteTableState` accepts on the wire.
pub const TRANSIT_GATEWAY_ROUTE_TABLE_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "pending"];


/// The values `TransitGatewayRouteType` accepts on the wire.
pub const TRANSIT_GATEWAY_ROUTE_TYPE_VALUES: &[&str] = &["propagated", "static"];


/// The values `TransitGatewayState` accepts on the wire.
pub const TRANSIT_GATEWAY_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "modifying", "pending"];


/// The values `TransportProtocol` accepts on the wire.
pub const TRANSPORT_PROTOCOL_VALUES: &[&str] = &["tcp", "udp"];


/// The values `TrustProviderType` accepts on the wire.
pub const TRUST_PROVIDER_TYPE_VALUES: &[&str] = &["device", "user"];


/// The values `TunnelInsideIpVersion` accepts on the wire.
pub const TUNNEL_INSIDE_IP_VERSION_VALUES: &[&str] = &["ipv4", "ipv6"];


/// The values `UnlimitedSupportedInstanceFamily` accepts on the wire.
pub const UNLIMITED_SUPPORTED_INSTANCE_FAMILY_VALUES: &[&str] = &["t2", "t3", "t3a", "t4g", "t8i"];


/// The values `UnsuccessfulInstanceCreditSpecificationErrorCode` accepts on the wire.
pub const UNSUCCESSFUL_INSTANCE_CREDIT_SPECIFICATION_ERROR_CODE_VALUES: &[&str] = &["IncorrectInstanceState", "InstanceCreditSpecification.NotSupported", "InvalidInstanceID.Malformed", "InvalidInstanceID.NotFound"];


/// The values `UsageClassType` accepts on the wire.
pub const USAGE_CLASS_TYPE_VALUES: &[&str] = &["capacity-block", "on-demand", "spot"];


/// The values `UserTrustProviderType` accepts on the wire.
pub const USER_TRUST_PROVIDER_TYPE_VALUES: &[&str] = &["iam-identity-center", "oidc"];


/// The values `VerificationMethod` accepts on the wire.
pub const VERIFICATION_METHOD_VALUES: &[&str] = &["dns-token", "remarks-x509"];


/// The values `VerifiedAccessEndpointAttachmentType` accepts on the wire.
pub const VERIFIED_ACCESS_ENDPOINT_ATTACHMENT_TYPE_VALUES: &[&str] = &["vpc"];


/// The values `VerifiedAccessEndpointProtocol` accepts on the wire.
pub const VERIFIED_ACCESS_ENDPOINT_PROTOCOL_VALUES: &[&str] = &["http", "https", "tcp"];


/// The values `VerifiedAccessEndpointStatusCode` accepts on the wire.
pub const VERIFIED_ACCESS_ENDPOINT_STATUS_CODE_VALUES: &[&str] = &["active", "deleted", "deleting", "pending", "updating"];


/// The values `VerifiedAccessEndpointType` accepts on the wire.
pub const VERIFIED_ACCESS_ENDPOINT_TYPE_VALUES: &[&str] = &["cidr", "load-balancer", "network-interface", "rds"];


/// The values `VerifiedAccessLogDeliveryStatusCode` accepts on the wire.
pub const VERIFIED_ACCESS_LOG_DELIVERY_STATUS_CODE_VALUES: &[&str] = &["failed", "success"];


/// The values `VirtualizationType` accepts on the wire.
pub const VIRTUALIZATION_TYPE_VALUES: &[&str] = &["hvm", "paravirtual"];


/// The values `VolumeAttachmentState` accepts on the wire.
pub const VOLUME_ATTACHMENT_STATE_VALUES: &[&str] = &["attached", "attaching", "busy", "detached", "detaching"];


/// The values `VolumeAttributeName` accepts on the wire.
pub const VOLUME_ATTRIBUTE_NAME_VALUES: &[&str] = &["autoEnableIO", "productCodes"];


/// The values `VolumeModificationState` accepts on the wire.
pub const VOLUME_MODIFICATION_STATE_VALUES: &[&str] = &["completed", "failed", "modifying", "optimizing"];


/// The values `VolumeState` accepts on the wire.
pub const VOLUME_STATE_VALUES: &[&str] = &["available", "creating", "deleted", "deleting", "error", "in-use"];


/// The values `VolumeStatusInfoStatus` accepts on the wire.
pub const VOLUME_STATUS_INFO_STATUS_VALUES: &[&str] = &["impaired", "insufficient-data", "ok", "warning"];


/// The values `VolumeStatusName` accepts on the wire.
pub const VOLUME_STATUS_NAME_VALUES: &[&str] = &["initialization-state", "io-enabled", "io-performance"];


/// The values `VolumeType` accepts on the wire.
pub const VOLUME_TYPE_VALUES: &[&str] = &["gp2", "gp3", "io1", "io2", "sc1", "st1", "standard"];


/// The values `VpcAttributeName` accepts on the wire.
pub const VPC_ATTRIBUTE_NAME_VALUES: &[&str] = &["enableDnsHostnames", "enableDnsSupport", "enableNetworkAddressUsageMetrics"];


/// The values `VpcBlockPublicAccessExclusionState` accepts on the wire.
pub const VPC_BLOCK_PUBLIC_ACCESS_EXCLUSION_STATE_VALUES: &[&str] = &["create-complete", "create-failed", "create-in-progress", "delete-complete", "delete-in-progress", "disable-complete", "disable-in-progress", "update-complete", "update-failed", "update-in-progress"];


/// The values `VpcBlockPublicAccessExclusionsAllowed` accepts on the wire.
pub const VPC_BLOCK_PUBLIC_ACCESS_EXCLUSIONS_ALLOWED_VALUES: &[&str] = &["allowed", "not-allowed"];


/// The values `VpcBlockPublicAccessState` accepts on the wire.
pub const VPC_BLOCK_PUBLIC_ACCESS_STATE_VALUES: &[&str] = &["default-state", "update-complete", "update-in-progress"];


/// The values `VpcCidrBlockStateCode` accepts on the wire.
pub const VPC_CIDR_BLOCK_STATE_CODE_VALUES: &[&str] = &["associated", "associating", "disassociated", "disassociating", "failed", "failing"];


/// The values `VpcEncryptionControlExclusionState` accepts on the wire.
pub const VPC_ENCRYPTION_CONTROL_EXCLUSION_STATE_VALUES: &[&str] = &["disabled", "disabling", "enabled", "enabling"];


/// The values `VpcEncryptionControlExclusionStateInput` accepts on the wire.
pub const VPC_ENCRYPTION_CONTROL_EXCLUSION_STATE_INPUT_VALUES: &[&str] = &["disable", "enable"];


/// The values `VpcEncryptionControlMode` accepts on the wire.
pub const VPC_ENCRYPTION_CONTROL_MODE_VALUES: &[&str] = &["enforce", "monitor"];


/// The values `VpcEncryptionControlState` accepts on the wire.
pub const VPC_ENCRYPTION_CONTROL_STATE_VALUES: &[&str] = &["available", "creating", "delete-failed", "deleted", "deleting", "enforce-failed", "enforce-in-progress", "monitor-failed", "monitor-in-progress"];


/// The values `VpcEndpointType` accepts on the wire.
pub const VPC_ENDPOINT_TYPE_VALUES: &[&str] = &["Gateway", "GatewayLoadBalancer", "Interface", "Resource", "ServiceNetwork", "Tunnel"];


/// The values `VpcPeeringConnectionStateReasonCode` accepts on the wire.
pub const VPC_PEERING_CONNECTION_STATE_REASON_CODE_VALUES: &[&str] = &["active", "deleted", "deleting", "expired", "failed", "initiating-request", "pending-acceptance", "provisioning", "rejected"];


/// The values `VpcState` accepts on the wire.
pub const VPC_STATE_VALUES: &[&str] = &["available", "deleting", "pending"];


/// The values `VpcTenancy` accepts on the wire.
pub const VPC_TENANCY_VALUES: &[&str] = &["default"];


/// The values `VpnConcentratorType` accepts on the wire.
pub const VPN_CONCENTRATOR_TYPE_VALUES: &[&str] = &["ipsec.1"];


/// The values `VpnEcmpSupportValue` accepts on the wire.
pub const VPN_ECMP_SUPPORT_VALUE_VALUES: &[&str] = &["disable", "enable"];


/// The values `VpnProtocol` accepts on the wire.
pub const VPN_PROTOCOL_VALUES: &[&str] = &["openvpn"];


/// The values `VpnState` accepts on the wire.
pub const VPN_STATE_VALUES: &[&str] = &["available", "deleted", "deleting", "pending"];


/// The values `VpnStaticRouteSource` accepts on the wire.
pub const VPN_STATIC_ROUTE_SOURCE_VALUES: &[&str] = &["Static"];


/// The values `VpnTunnelBandwidth` accepts on the wire.
pub const VPN_TUNNEL_BANDWIDTH_VALUES: &[&str] = &["large", "standard"];


/// The values `VpnTunnelProvisioningStatus` accepts on the wire.
pub const VPN_TUNNEL_PROVISIONING_STATUS_VALUES: &[&str] = &["available", "failed", "pending"];


/// The values `WeekDay` accepts on the wire.
pub const WEEK_DAY_VALUES: &[&str] = &["friday", "monday", "saturday", "sunday", "thursday", "tuesday", "wednesday"];


/// The values `ZeroSizePreference` accepts on the wire.
pub const ZERO_SIZE_PREFERENCE_VALUES: &[&str] = &["default", "retain"];


/// The values `scope` accepts on the wire.
pub const SCOPE_VALUES: &[&str] = &["Availability Zone", "Region"];
