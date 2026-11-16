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
#[path = "all/arn_parameter_refusals.rs"]
mod arn_parameter_refusals;
#[path = "all/component_build_version_listing.rs"]
mod component_build_version_listing;
#[path = "all/component_document_sources.rs"]
mod component_document_sources;
#[path = "all/component_image_recipe.rs"]
mod component_image_recipe;
#[path = "all/component_import.rs"]
mod component_import;
#[path = "all/component_listing.rs"]
mod component_listing;
#[path = "all/component_listing_wire_members.rs"]
mod component_listing_wire_members;
#[path = "all/component_policy.rs"]
mod component_policy;
#[path = "all/component_policy_lifecycle.rs"]
mod component_policy_lifecycle;
#[path = "all/component_read_wire_members.rs"]
mod component_read_wire_members;
#[path = "all/container_recipe.rs"]
mod container_recipe;
#[path = "all/dependency_refusal_wording.rs"]
mod dependency_refusal_wording;
#[path = "all/distribution_ami_names.rs"]
mod distribution_ami_names;
#[path = "all/distribution_configuration.rs"]
mod distribution_configuration;
#[path = "all/distribution_target_regions.rs"]
mod distribution_target_regions;
#[path = "all/error_response_envelope.rs"]
mod error_response_envelope;
#[path = "all/image_creation.rs"]
mod image_creation;
#[path = "all/image_output_resources.rs"]
mod image_output_resources;
#[path = "all/image_packages.rs"]
mod image_packages;
#[path = "all/image_pipeline_execution.rs"]
mod image_pipeline_execution;
#[path = "all/image_pipeline_listing.rs"]
mod image_pipeline_listing;
#[path = "all/image_pipeline_schedule.rs"]
mod image_pipeline_schedule;
#[path = "all/image_pipeline_schedule_listing.rs"]
mod image_pipeline_schedule_listing;
#[path = "all/image_pipeline_update.rs"]
mod image_pipeline_update;
#[path = "all/image_read.rs"]
mod image_read;
#[path = "all/infrastructure_configuration.rs"]
mod infrastructure_configuration;
#[path = "all/infrastructure_configuration_listing.rs"]
mod infrastructure_configuration_listing;
#[path = "all/infrastructure_configuration_update.rs"]
mod infrastructure_configuration_update;
#[path = "all/modelled_range_bounds.rs"]
mod modelled_range_bounds;
#[path = "all/parameter_value_refusals.rs"]
mod parameter_value_refusals;
#[path = "all/parent_image_and_dependencies.rs"]
mod parent_image_and_dependencies;
#[path = "all/recipe_group_and_pipeline_listing.rs"]
mod recipe_group_and_pipeline_listing;
#[path = "all/resource_delete_lifecycle.rs"]
mod resource_delete_lifecycle;
#[path = "all/resource_not_found.rs"]
mod resource_not_found;
#[path = "all/resource_policy_kinds.rs"]
mod resource_policy_kinds;
#[path = "all/resource_tags.rs"]
mod resource_tags;
#[path = "all/semantic_version_bounds.rs"]
mod semantic_version_bounds;
#[path = "all/state_roundtrip.rs"]
mod state_roundtrip;
#[path = "all/tag_resource_resource_kinds.rs"]
mod tag_resource_resource_kinds;
#[path = "all/tagging_not_found_class.rs"]
mod tagging_not_found_class;
#[path = "all/untag_resource.rs"]
mod untag_resource;
#[path = "all/versioned_resource_responses.rs"]
mod versioned_resource_responses;
#[path = "all/waiting_workflow_step_pages.rs"]
mod waiting_workflow_step_pages;
#[path = "all/waiting_workflow_steps.rs"]
mod waiting_workflow_steps;
#[path = "all/workflow_resources.rs"]
mod workflow_resources;
#[path = "all/workflow_versions.rs"]
mod workflow_versions;
#[path = "all/parameter_value_refusal_wording.rs"]
mod parameter_value_refusal_wording;
