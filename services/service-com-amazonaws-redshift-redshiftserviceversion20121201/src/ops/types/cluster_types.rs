// SPDX-License-Identifier: CC0-1.0

//! `Cluster`: the shape every provisioned-cluster write and read answers with.
//!
//! This is the ONE `Cluster` type of this crate, so what it declares is what
//! `CreateCluster`, every `Modify*`, `RestoreFromClusterSnapshot` and
//! `DescribeClusters` report.
//!
//! It is declared here rather than beside the other shared shapes because Amazon
//! Redshift answers with three members the published model snapshot does not
//! declare, and that snapshot is kept as shipped (see
//! `smithy-model/LOCAL-ADDITIONS.md`):
//!
//! * `AvailabilityZoneRelocation` — the relocation option's own enablement,
//!   reported beside the `AvailabilityZoneRelocationStatus` the snapshot does
//!   declare;
//! * `AzDisasterRecoveryStatus` — the enablement of the cluster's
//!   Availability-Zone-level recovery, which for a provisioned cluster is that
//!   same relocation;
//! * `IsMaintenanceWindowActivated` — whether the cluster's preferred
//!   maintenance window is in force rather than suspended by a deferral.
//!
//! So the type is generated, like every other modelled shape, from a model file —
//! the small overlay `smithy-model/local-additions.json`, which restates the
//! members the snapshot declares, with their own targets so every list's item
//! element name and every scalar's type are captured exactly as the snapshot
//! spells them, and adds the three. Nothing about the wire is hand-written: the
//! member names, the shape id and the traits all come from that file.
//!
//! The values of the three are derived from the cluster's own stored
//! configuration by [`crate::state::populate_derived_members`], the one rule
//! every path that writes a cluster record runs.
#![allow(unused_imports)]

use super::*;

smithy_macros::smithy_model!("smithy-model/local-additions.json");

#[smithy_macros::smithy(shape = "com.amazonaws.redshift#Cluster")]
#[derive(Debug, Clone, Default)]
pub struct Cluster {
    pub cluster_identifier: Option<String>,
    pub node_type: Option<String>,
    pub cluster_status: Option<String>,
    pub cluster_availability_status: Option<String>,
    pub modify_status: Option<String>,
    pub master_username: Option<String>,
    pub db_name: Option<String>,
    pub endpoint: Option<Endpoint>,
    pub cluster_create_time: Option<::aws_smithy_types::DateTime>,
    pub automated_snapshot_retention_period: Option<i32>,
    pub manual_snapshot_retention_period: Option<i32>,
    pub cluster_security_groups: Option<Vec<ClusterSecurityGroupMembership>>,
    pub vpc_security_groups: Option<Vec<VpcSecurityGroupMembership>>,
    pub cluster_parameter_groups: Option<Vec<ClusterParameterGroupStatus>>,
    pub cluster_subnet_group_name: Option<String>,
    pub vpc_id: Option<String>,
    pub availability_zone: Option<String>,
    pub preferred_maintenance_window: Option<String>,
    pub pending_modified_values: Option<PendingModifiedValues>,
    pub cluster_version: Option<String>,
    pub allow_version_upgrade: Option<bool>,
    pub number_of_nodes: Option<i32>,
    pub publicly_accessible: Option<bool>,
    pub encrypted: Option<bool>,
    pub restore_status: Option<RestoreStatus>,
    pub data_transfer_progress: Option<DataTransferProgress>,
    pub hsm_status: Option<HsmStatus>,
    pub cluster_snapshot_copy_status: Option<ClusterSnapshotCopyStatus>,
    pub cluster_public_key: Option<String>,
    pub cluster_nodes: Option<Vec<ClusterNode>>,
    pub elastic_ip_status: Option<ElasticIpStatus>,
    pub cluster_revision_number: Option<String>,
    pub tags: Option<Vec<Tag>>,
    pub kms_key_id: Option<String>,
    pub enhanced_vpc_routing: Option<bool>,
    pub iam_roles: Option<Vec<ClusterIamRole>>,
    pub pending_actions: Option<Vec<String>>,
    pub maintenance_track_name: Option<String>,
    pub elastic_resize_number_of_node_options: Option<String>,
    pub deferred_maintenance_windows: Option<Vec<DeferredMaintenanceWindow>>,
    pub snapshot_schedule_identifier: Option<String>,
    pub snapshot_schedule_state: Option<String>,
    pub expected_next_snapshot_schedule_time: Option<::aws_smithy_types::DateTime>,
    pub expected_next_snapshot_schedule_time_status: Option<String>,
    pub next_maintenance_window_start_time: Option<::aws_smithy_types::DateTime>,
    pub resize_info: Option<ResizeInfo>,
    pub availability_zone_relocation_status: Option<String>,
    /// The first member the snapshot does not declare: the Availability Zone
    /// relocation option's own enablement, which carries the same value as
    /// `availability_zone_relocation_status`.
    pub availability_zone_relocation: Option<String>,
    /// The second: the enablement of the cluster's Availability-Zone-level
    /// disaster recovery, which is that same relocation.
    pub az_disaster_recovery_status: Option<String>,
    /// The third: whether the cluster's preferred maintenance window is in force.
    pub is_maintenance_window_activated: Option<bool>,
    pub cluster_namespace_arn: Option<String>,
    pub total_storage_capacity_in_mega_bytes: Option<i64>,
    pub aqua_configuration: Option<AquaConfiguration>,
    pub default_iam_role_arn: Option<String>,
    pub reserved_node_exchange_status: Option<ReservedNodeExchangeStatus>,
    pub custom_domain_name: Option<String>,
    pub custom_domain_certificate_arn: Option<String>,
    pub custom_domain_certificate_expiry_date: Option<::aws_smithy_types::DateTime>,
    pub master_password_secret_arn: Option<String>,
    pub master_password_secret_kms_key_id: Option<String>,
    pub ip_address_type: Option<String>,
    pub multi_az: Option<String>,
    pub multi_az_secondary: Option<SecondaryClusterInfo>,
    pub lakehouse_registration_status: Option<String>,
    pub catalog_arn: Option<String>,
    pub extra_compute_for_automatic_optimization: Option<String>,
    pub logging_publish_status: Option<LoggingPublishStatus>,
}
