// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Put a new suite at `tests/all/<name>.rs` and declare it below, keeping the list
//! sorted.
//! A suite that is not declared here compiles nowhere and its tests never run. A
//! suite left at the top level of `tests/` fails the build.
// `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` resolves to a SIBLING
// (`tests/state_roundtrip.rs`) — which the layout check refuses. The `#[path]` is what
// points it into `tests/all/`.
#[path = "all/audit_records.rs"]
mod audit_records;
#[path = "all/compaction_sort_order.rs"]
mod compaction_sort_order;
#[path = "all/constraint_refusals.rs"]
mod constraint_refusals;
#[path = "all/encryption_configuration_refusals.rs"]
mod encryption_configuration_refusals;
#[path = "all/fixed_error_sentences.rs"]
mod fixed_error_sentences;
#[path = "all/get_table_addressing.rs"]
mod get_table_addressing;
#[path = "all/harness.rs"]
mod harness;
#[path = "all/iceberg_rest_catalog.rs"]
mod iceberg_rest_catalog;
#[path = "all/iceberg_rest_discovery.rs"]
mod iceberg_rest_discovery;
#[path = "all/invalid_input_refusals.rs"]
mod invalid_input_refusals;
#[path = "all/list_namespaces.rs"]
mod list_namespaces;
#[path = "all/list_tags_for_resource.rs"]
mod list_tags_for_resource;
#[path = "all/list_tables_row_wire_forms.rs"]
mod list_tables_row_wire_forms;
#[path = "all/list_tables_summary_members.rs"]
mod list_tables_summary_members;
#[path = "all/maintenance_request_refusals.rs"]
mod maintenance_request_refusals;
#[path = "all/metrics_configuration_not_found_message.rs"]
mod metrics_configuration_not_found_message;
#[path = "all/minted_value_forms.rs"]
mod minted_value_forms;
#[path = "all/namespace_and_table_lifecycle.rs"]
mod namespace_and_table_lifecycle;
#[path = "all/name_lookups.rs"]
mod name_lookups;
#[path = "all/non_empty_container_delete_class.rs"]
mod non_empty_container_delete_class;
#[path = "all/parameter_validation_refusals.rs"]
mod parameter_validation_refusals;
#[path = "all/record_expiration_configuration_reads.rs"]
mod record_expiration_configuration_reads;
#[path = "all/rename_table.rs"]
mod rename_table;
#[path = "all/replication_put_outcome.rs"]
mod replication_put_outcome;
#[path = "all/replication_version_token_preconditions.rs"]
mod replication_version_token_preconditions;
#[path = "all/resource_tagging.rs"]
mod resource_tagging;
#[path = "all/service_managed_table_deletes.rs"]
mod service_managed_table_deletes;
#[path = "all/state_roundtrip.rs"]
mod state_roundtrip;
#[path = "all/table_bucket_encryption.rs"]
mod table_bucket_encryption;
#[path = "all/table_bucket_encryption_put.rs"]
mod table_bucket_encryption_put;
#[path = "all/table_bucket_lifecycle.rs"]
mod table_bucket_lifecycle;
#[path = "all/table_bucket_maintenance_configuration.rs"]
mod table_bucket_maintenance_configuration;
#[path = "all/table_bucket_metrics_configuration.rs"]
mod table_bucket_metrics_configuration;
#[path = "all/table_bucket_policy.rs"]
mod table_bucket_policy;
#[path = "all/table_bucket_replication.rs"]
mod table_bucket_replication;
#[path = "all/table_bucket_storage_class.rs"]
mod table_bucket_storage_class;
#[path = "all/table_bucket_storage_class_reads.rs"]
mod table_bucket_storage_class_reads;
#[path = "all/table_encryption.rs"]
mod table_encryption;
#[path = "all/table_encryption_authorization.rs"]
mod table_encryption_authorization;
#[path = "all/table_maintenance_configuration.rs"]
mod table_maintenance_configuration;
#[path = "all/table_maintenance_put_round_trip.rs"]
mod table_maintenance_put_round_trip;
#[path = "all/table_metadata_location.rs"]
mod table_metadata_location;
#[path = "all/table_modification_record.rs"]
mod table_modification_record;
#[path = "all/table_record_expiration.rs"]
mod table_record_expiration;
#[path = "all/untag_resource.rs"]
mod untag_resource;
#[path = "all/wire_forms.rs"]
mod wire_forms;
#[path = "all/metrics_configuration_delete_scope.rs"]
mod metrics_configuration_delete_scope;
#[path = "all/table_maintenance_job_status.rs"]
mod table_maintenance_job_status;
