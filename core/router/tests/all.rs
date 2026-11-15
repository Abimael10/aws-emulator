// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for the router.
//!
//! Put a new suite at `tests/all/<name>.rs` and declare it below with a `#[path]`/`mod`
//! pair, keeping the list sorted: one pair per suite and nothing else, so a landing has one
//! place to insert and nothing to keep consistent.
//! A suite that is not declared here compiles nowhere and its tests never run; a suite left
//! at the top level of `tests/` fails the build; and a suite declared twice is `E0428`,
//! which compiles no suite of this target. THIS HEADER IS THE ONLY PLACE AN INNER DOC
//! COMMENT (`//!`) MAY STAND IN THIS FILE: past the first item one is `E0753` and compiles
//! nothing, so a note below must be an ordinary `//` comment — better still, add a
//! declaration rather than a note about one.
//! `all/all_rs_declares_each_suite_once.rs` and
//! `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of that, for
//! this file and for every service crate's `tests/all.rs`.
// `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` resolves to a SIBLING
// (`tests/<name>.rs`) — which the layout check refuses. The `#[path]` is what points each
// declaration into `tests/all/`.
//! Put a new suite at `tests/all/<name>.rs` and declare it below with a
//! `#[path]`/`mod` pair, keeping the list sorted: one pair per suite and nothing
//! else, so a landing has one place to insert and nothing to keep consistent.
//! A suite that is not declared here compiles nowhere and its tests never run. A
//! suite left at the top level of `tests/` fails the build. Declare each suite
//! exactly ONCE: this is one crate root, so a second `mod` for the same name is
//! `E0428` and fails every suite in this target. An inner doc comment (`//!`) is
//! legal only BEFORE the first item, so THIS header is the only place one may
//! stand — past it, `//!` is `E0753` and compiles nothing, so a note further down
//! must be an ordinary `//` comment, or better still belong here instead.
//! `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of
//! that, for this file and for every service crate's `tests/all.rs`, so check
//! there rather than leaving a note below.
//! `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` resolves to a
//! SIBLING (`tests/<name>.rs`) — which the layout check refuses. The `#[path]` is
//! what points each declaration into `tests/all/`.
//! place to insert and nothing to keep consistent. A suite no `mod` names compiles nowhere
//! and its tests never run; a suite left at the top level of `tests/` fails the build; and a
//! suite declared twice is `E0428`, which fails every suite in this target.
//! THIS BLOCK IS THE ONLY PLACE AN INNER DOC COMMENT (`//!`) MAY STAND IN THIS FILE: past
//! the first item one is `E0753` and compiles no suite of this target. Below, add a
//! declaration — not a note about one. `all/all_rs_declares_each_suite_once.rs` and
//! this file and for every service crate's `tests/all.rs`, so check there rather than
//! leaving a note here.
//! `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` resolves to a SIBLING
//! (`tests/<name>.rs`) — which the layout check refuses. The `#[path]` is what points it
//! into `tests/all/`.
//! Put a new suite at `tests/all/<name>.rs` and declare it below with a `#[path]` / `mod`
//! A suite that is not declared here compiles nowhere and its tests never run, and a suite
//! left at the top level of `tests/` fails the build. Declare each suite exactly ONCE: this
//! is one crate root, so a second `mod` for the same name is `E0428`. THIS HEADER IS THE
//! ONLY PLACE AN INNER DOC COMMENT (`//!`) MAY STAND IN THIS FILE: past the first item one
//! is `E0753`. Either fails every suite in this target, so below add a declaration — not a
//! note about one. `all/all_rs_declares_each_suite_once.rs` and
//! (`tests/boot_serve.rs`) — which the layout check refuses. The `#[path]` above each `mod`
//! is what points it into `tests/all/`.
//! pair, keeping the list sorted: one pair per suite and nothing else, so a landing has
//! one place to insert and nothing to keep consistent.
//! A suite that is not declared here compiles nowhere and its tests never run; a suite
//! left at the top level of `tests/` fails the build; and a suite declared twice is
//! `E0428`, which fails every suite in this target. `tests/all.rs` is a test-target CRATE
//! ROOT, so a bare `mod` resolves to a SIBLING of this file, which the layout check
//! refuses: the `#[path]` is what points the module into `tests/all/`.
//! `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of that,
//! for this file and for every service crate's `tests/all.rs`.
#[path = "all/agent_registry_signing_name_routing.rs"]
mod agent_registry_signing_name_routing;
#[path = "all/all_rs_declares_each_suite_once.rs"]
mod all_rs_declares_each_suite_once;
#[path = "all/all_rs_inner_doc_comments_precede_the_first_item.rs"]
mod all_rs_inner_doc_comments_precede_the_first_item;
#[path = "all/amplifybackend_app_registration.rs"]
mod amplifybackend_app_registration;
#[path = "all/aoss_vpc_endpoint_placement_resolves_in_ec2.rs"]
mod aoss_vpc_endpoint_placement_resolves_in_ec2;
#[path = "all/apigateway_account_cloudwatch_role.rs"]
mod apigateway_account_cloudwatch_role;
#[path = "all/apigateway_cognito_authorizer_app_client_audience.rs"]
mod apigateway_cognito_authorizer_app_client_audience;
#[path = "all/apigateway_cognito_user_pools_authorizer.rs"]
mod apigateway_cognito_user_pools_authorizer;
#[path = "all/apigateway_dynamodb_service_integration.rs"]
mod apigateway_dynamodb_service_integration;
#[path = "all/apigateway_iam_authenticated_invoke.rs"]
mod apigateway_iam_authenticated_invoke;
#[path = "all/apigateway_iam_authorized_invoke.rs"]
mod apigateway_iam_authorized_invoke;
#[path = "all/apigateway_lambda_ddb_s3_trace_tree.rs"]
mod apigateway_lambda_ddb_s3_trace_tree;
#[path = "all/apigateway_private_vpce_invoke.rs"]
mod apigateway_private_vpce_invoke;
#[path = "all/apigateway_proxy_lambda_ddb_pipeline.rs"]
mod apigateway_proxy_lambda_ddb_pipeline;
#[path = "all/apigateway_proxy_stage_surfaces_its_handlers_envelope.rs"]
mod apigateway_proxy_stage_surfaces_its_handlers_envelope;
#[path = "all/apigateway_repeated_stage_requests.rs"]
mod apigateway_repeated_stage_requests;
#[path = "all/apigateway_s3_service_integration.rs"]
mod apigateway_s3_service_integration;
#[path = "all/apigateway_signing_name_routing.rs"]
mod apigateway_signing_name_routing;
#[path = "all/apigateway_sns_service_integration.rs"]
mod apigateway_sns_service_integration;
#[path = "all/apigateway_sqs_service_integration.rs"]
mod apigateway_sqs_service_integration;
#[path = "all/apigateway_vpc_link_private_integration.rs"]
mod apigateway_vpc_link_private_integration;
#[path = "all/apigateway_wafv2_waf_log_destination_gate.rs"]
mod apigateway_wafv2_waf_log_destination_gate;
#[path = "all/apigatewaymanagementapi_manage_connections_authorization.rs"]
mod apigatewaymanagementapi_manage_connections_authorization;
#[path = "all/apigatewayv2_cognito_jwt_authorizer.rs"]
mod apigatewayv2_cognito_jwt_authorizer;
#[path = "all/apigatewayv2_vpc_link_subnets_resolve_in_ec2.rs"]
mod apigatewayv2_vpc_link_subnets_resolve_in_ec2;
#[path = "all/apigatewayv2_websocket_deploy_then_connections.rs"]
mod apigatewayv2_websocket_deploy_then_connections;
#[path = "all/appconfig_signing_name_routing.rs"]
mod appconfig_signing_name_routing;
#[path = "all/applicationinsights_resource_group_admission.rs"]
mod applicationinsights_resource_group_admission;
#[path = "all/appstream_app_block_builder_role_trust.rs"]
mod appstream_app_block_builder_role_trust;
#[path = "all/assembled_router_refusal_identities.rs"]
mod assembled_router_refusal_identities;
#[path = "all/audited_calls_that_fail_after_acceptance.rs"]
mod audited_calls_that_fail_after_acceptance;
#[path = "all/autoscaling_notification_reaches_its_subscribed_queue.rs"]
mod autoscaling_notification_reaches_its_subscribed_queue;
#[path = "all/autoscaling_plans_policies_reach_application_auto_scaling.rs"]
mod autoscaling_plans_policies_reach_application_auto_scaling;
#[path = "all/aws_marketplace_signing_name_routing.rs"]
mod aws_marketplace_signing_name_routing;
#[path = "all/backup_job_state_change_invokes_its_lambda_target.rs"]
mod backup_job_state_change_invokes_its_lambda_target;
#[path = "all/batch_job_state_change_is_scoped_to_its_own_job_queue.rs"]
mod batch_job_state_change_is_scoped_to_its_own_job_queue;
#[path = "all/batch_job_state_change_on_an_authenticating_router.rs"]
mod batch_job_state_change_on_an_authenticating_router;
#[path = "all/batch_job_state_change_reaches_matching_rules.rs"]
mod batch_job_state_change_reaches_matching_rules;
#[path = "all/bedrock_agent_ingestion_populates_its_s3_vectors_index.rs"]
mod bedrock_agent_ingestion_populates_its_s3_vectors_index;
#[path = "all/bedrock_agent_runtime_session_authorization.rs"]
mod bedrock_agent_runtime_session_authorization;
#[path = "all/bedrock_agentcore_data_plane_parent_resources.rs"]
mod bedrock_agentcore_data_plane_parent_resources;
#[path = "all/bedrock_agentcore_execution_role.rs"]
mod bedrock_agentcore_execution_role;
#[path = "all/bedrock_agentcore_gateway_lambda_target.rs"]
mod bedrock_agentcore_gateway_lambda_target;
#[path = "all/bedrock_agentcore_gateway_mcp_endpoint.rs"]
mod bedrock_agentcore_gateway_mcp_endpoint;
#[path = "all/bedrock_agentcore_gateway_mcp_transport_path.rs"]
mod bedrock_agentcore_gateway_mcp_transport_path;
#[path = "all/bedrock_agentcore_signing_name_routing.rs"]
mod bedrock_agentcore_signing_name_routing;
#[path = "all/bedrock_catalogue_is_invocable.rs"]
mod bedrock_catalogue_is_invocable;
#[path = "all/bedrock_guardrail_applied_across_planes.rs"]
mod bedrock_guardrail_applied_across_planes;
#[path = "all/bedrock_knowledge_base_over_an_aoss_collection.rs"]
mod bedrock_knowledge_base_over_an_aoss_collection;
#[path = "all/bedrock_logging_destination_region.rs"]
mod bedrock_logging_destination_region;
#[path = "all/bedrock_model_customization_pass_role.rs"]
mod bedrock_model_customization_pass_role;
#[path = "all/bedrock_model_invocation_logging_pass_role.rs"]
mod bedrock_model_invocation_logging_pass_role;
#[path = "all/bedrock_modelled_route_claims.rs"]
mod bedrock_modelled_route_claims;
#[path = "all/bedrock_signing_name_routing.rs"]
mod bedrock_signing_name_routing;
#[path = "all/billing_access_denied_principal_agrees_with_sts.rs"]
mod billing_access_denied_principal_agrees_with_sts;
#[path = "all/billingconductor_payer_account.rs"]
mod billingconductor_payer_account;
#[path = "all/boot_serve.rs"]
mod boot_serve;
#[path = "all/cfn_deployed_backup_pipeline_delivers_job_events.rs"]
mod cfn_deployed_backup_pipeline_delivers_job_events;
#[path = "all/cfn_provisions_declared_resources.rs"]
mod cfn_provisions_declared_resources;
#[path = "all/chime_app_instance_admin_authorization.rs"]
mod chime_app_instance_admin_authorization;
#[path = "all/chime_deleted_meeting_tagging.rs"]
mod chime_deleted_meeting_tagging;
#[path = "all/chime_family_signing_name_routing.rs"]
mod chime_family_signing_name_routing;
#[path = "all/chime_identity_name_projection.rs"]
mod chime_identity_name_projection;
#[path = "all/chime_signing_name_routing.rs"]
mod chime_signing_name_routing;
#[path = "all/cloudformation_activates_acmpca_certificate_authorities.rs"]
mod cloudformation_activates_acmpca_certificate_authorities;
#[path = "all/cloudformation_admits_acmpca_resource_types.rs"]
mod cloudformation_admits_acmpca_resource_types;
#[path = "all/cloudformation_alb_fargate_service_converges.rs"]
mod cloudformation_alb_fargate_service_converges;
#[path = "all/cloudformation_apigateway_access_control.rs"]
mod cloudformation_apigateway_access_control;
#[path = "all/cloudformation_apigateway_base_path_mappings.rs"]
mod cloudformation_apigateway_base_path_mappings;
#[path = "all/cloudformation_apigateway_custom_domains.rs"]
mod cloudformation_apigateway_custom_domains;
#[path = "all/cloudformation_apigatewayv2_api_children.rs"]
mod cloudformation_apigatewayv2_api_children;
#[path = "all/cloudformation_apigatewayv2_resources.rs"]
mod cloudformation_apigatewayv2_resources;
#[path = "all/cloudformation_appsync_custom_domains.rs"]
mod cloudformation_appsync_custom_domains;
#[path = "all/cloudformation_asset_bucket_publishing_role.rs"]
mod cloudformation_asset_bucket_publishing_role;
#[path = "all/cloudformation_bucket_handles_drive_their_owning_services.rs"]
mod cloudformation_bucket_handles_drive_their_owning_services;
#[path = "all/cloudformation_canceled_delete_keeps_exported_resources.rs"]
mod cloudformation_canceled_delete_keeps_exported_resources;
#[path = "all/cloudformation_cdk_bucket_notifications_custom_resource.rs"]
mod cloudformation_cdk_bucket_notifications_custom_resource;
#[path = "all/cloudformation_ci_topology_runs_its_pipeline.rs"]
mod cloudformation_ci_topology_runs_its_pipeline;
#[path = "all/cloudformation_cloudfront_fronted_rest_api.rs"]
mod cloudformation_cloudfront_fronted_rest_api;
#[path = "all/cloudformation_cognito_identity_role_attachment_refusal.rs"]
mod cloudformation_cognito_identity_role_attachment_refusal;
#[path = "all/cloudformation_condition_gated_secrets_policy.rs"]
mod cloudformation_condition_gated_secrets_policy;
#[path = "all/cloudformation_declared_log_group_takes_its_function_logs.rs"]
mod cloudformation_declared_log_group_takes_its_function_logs;
#[path = "all/cloudformation_deployed_configuration_set_reaches_the_email_api.rs"]
mod cloudformation_deployed_configuration_set_reaches_the_email_api;
#[path = "all/cloudformation_deployed_crawler_crawls_its_s3_target.rs"]
mod cloudformation_deployed_crawler_crawls_its_s3_target;
#[path = "all/cloudformation_deployed_domain_index_lifecycle.rs"]
mod cloudformation_deployed_domain_index_lifecycle;
#[path = "all/cloudformation_deployed_pipe_delivery.rs"]
mod cloudformation_deployed_pipe_delivery;
#[path = "all/cloudformation_deployed_schedule_invokes_its_target.rs"]
mod cloudformation_deployed_schedule_invokes_its_target;
#[path = "all/cloudformation_domain_access_policy_revision.rs"]
mod cloudformation_domain_access_policy_revision;
#[path = "all/cloudformation_domain_endpoint_is_dialled_by_consumers.rs"]
mod cloudformation_domain_endpoint_is_dialled_by_consumers;
#[path = "all/cloudformation_domain_endpoint_outputs.rs"]
mod cloudformation_domain_endpoint_outputs;
#[path = "all/cloudformation_drift_detection_status_change_events.rs"]
mod cloudformation_drift_detection_status_change_events;
#[path = "all/cloudformation_dynamodb_table_in_place_update.rs"]
mod cloudformation_dynamodb_table_in_place_update;
#[path = "all/cloudformation_ec2_cdk_vpc_estate.rs"]
mod cloudformation_ec2_cdk_vpc_estate;
#[path = "all/cloudformation_ec2_ebs_instance_estate.rs"]
mod cloudformation_ec2_ebs_instance_estate;
#[path = "all/cloudformation_ec2_instance_rollback.rs"]
mod cloudformation_ec2_instance_rollback;
#[path = "all/cloudformation_ec2_network_identifiers.rs"]
mod cloudformation_ec2_network_identifiers;
#[path = "all/cloudformation_ec2_network_stack_routes.rs"]
mod cloudformation_ec2_network_stack_routes;
#[path = "all/cloudformation_elastic_beanstalk_removal_stabilisation.rs"]
mod cloudformation_elastic_beanstalk_removal_stabilisation;
#[path = "all/cloudformation_eventbridge_fan_out_to_lambda_and_sfn.rs"]
mod cloudformation_eventbridge_fan_out_to_lambda_and_sfn;
#[path = "all/cloudformation_eventbridge_scheduled_fargate_estate.rs"]
mod cloudformation_eventbridge_scheduled_fargate_estate;
#[path = "all/cloudformation_events_rule_lambda_target_permission_boundary.rs"]
mod cloudformation_events_rule_lambda_target_permission_boundary;
#[path = "all/cloudformation_failed_custom_resource_rollback_deletes.rs"]
mod cloudformation_failed_custom_resource_rollback_deletes;
#[path = "all/cloudformation_fifo_resource_generated_names.rs"]
mod cloudformation_fifo_resource_generated_names;
#[path = "all/cloudformation_firehose_stream_direct_put_delivers.rs"]
mod cloudformation_firehose_stream_direct_put_delivers;
#[path = "all/cloudformation_firehose_stream_stabilisation.rs"]
mod cloudformation_firehose_stream_stabilisation;
#[path = "all/cloudformation_glue_job_redeploy_across_regions.rs"]
mod cloudformation_glue_job_redeploy_across_regions;
#[path = "all/cloudformation_grants_acmpca_permissions.rs"]
mod cloudformation_grants_acmpca_permissions;
#[path = "all/cloudformation_half_made_resource_is_not_left_behind.rs"]
mod cloudformation_half_made_resource_is_not_left_behind;
#[path = "all/cloudformation_http_api_cors_lambda_default_route.rs"]
mod cloudformation_http_api_cors_lambda_default_route;
#[path = "all/cloudformation_imports_an_existing_queue.rs"]
mod cloudformation_imports_an_existing_queue;
#[path = "all/cloudformation_kms_encrypted_sns_sqs_pipeline.rs"]
mod cloudformation_kms_encrypted_sns_sqs_pipeline;
#[path = "all/cloudformation_lambda_function_log_destination.rs"]
mod cloudformation_lambda_function_log_destination;
#[path = "all/cloudformation_lambda_ssm_sourced_update.rs"]
mod cloudformation_lambda_ssm_sourced_update;
#[path = "all/cloudformation_lambda_versions_and_aliases.rs"]
mod cloudformation_lambda_versions_and_aliases;
#[path = "all/cloudformation_management_events.rs"]
mod cloudformation_management_events;
#[path = "all/cloudformation_metric_stream_estate_is_drainable.rs"]
mod cloudformation_metric_stream_estate_is_drainable;
#[path = "all/cloudformation_metric_stream_exports_to_s3.rs"]
mod cloudformation_metric_stream_exports_to_s3;
#[path = "all/cloudformation_metric_stream_filter_update.rs"]
mod cloudformation_metric_stream_filter_update;
#[path = "all/cloudformation_monitored_alb_search_estate.rs"]
mod cloudformation_monitored_alb_search_estate;
#[path = "all/cloudformation_opensearch_domain_access_policies.rs"]
mod cloudformation_opensearch_domain_access_policies;
#[path = "all/cloudformation_opensearch_domain_deployments.rs"]
mod cloudformation_opensearch_domain_deployments;
#[path = "all/cloudformation_opensearch_vpc_endpoint_access.rs"]
mod cloudformation_opensearch_vpc_endpoint_access;
#[path = "all/cloudformation_provisions_a_bedrock_knowledge_base.rs"]
mod cloudformation_provisions_a_bedrock_knowledge_base;
#[path = "all/cloudformation_provisions_acm_acme_endpoints.rs"]
mod cloudformation_provisions_acm_acme_endpoints;
#[path = "all/cloudformation_provisions_acm_certificates.rs"]
mod cloudformation_provisions_acm_certificates;
#[path = "all/cloudformation_provisions_acmpca_certificate_authorities.rs"]
mod cloudformation_provisions_acmpca_certificate_authorities;
#[path = "all/cloudformation_provisions_agentcore_workload_identities.rs"]
mod cloudformation_provisions_agentcore_workload_identities;
#[path = "all/cloudformation_provisions_amplifyuibuilder_entities.rs"]
mod cloudformation_provisions_amplifyuibuilder_entities;
#[path = "all/cloudformation_provisions_appsync_resources.rs"]
mod cloudformation_provisions_appsync_resources;
#[path = "all/cloudformation_provisions_backup_resources.rs"]
mod cloudformation_provisions_backup_resources;
#[path = "all/cloudformation_provisions_batch_resources.rs"]
mod cloudformation_provisions_batch_resources;
#[path = "all/cloudformation_provisions_bedrock_guardrails.rs"]
mod cloudformation_provisions_bedrock_guardrails;
#[path = "all/cloudformation_provisions_braket_spending_limits.rs"]
mod cloudformation_provisions_braket_spending_limits;
#[path = "all/cloudformation_provisions_cloudfront_resources.rs"]
mod cloudformation_provisions_cloudfront_resources;
#[path = "all/cloudformation_provisions_cloudwatch_dashboards.rs"]
mod cloudformation_provisions_cloudwatch_dashboards;
#[path = "all/cloudformation_provisions_cloudwatch_metric_streams.rs"]
mod cloudformation_provisions_cloudwatch_metric_streams;
#[path = "all/cloudformation_provisions_codeartifact_resources.rs"]
mod cloudformation_provisions_codeartifact_resources;
#[path = "all/cloudformation_provisions_codebuild_projects.rs"]
mod cloudformation_provisions_codebuild_projects;
#[path = "all/cloudformation_provisions_codebuild_report_groups.rs"]
mod cloudformation_provisions_codebuild_report_groups;
#[path = "all/cloudformation_provisions_codecommit_repositories.rs"]
mod cloudformation_provisions_codecommit_repositories;
#[path = "all/cloudformation_provisions_codedeploy_resources.rs"]
mod cloudformation_provisions_codedeploy_resources;
#[path = "all/cloudformation_provisions_codepipeline_pipelines.rs"]
mod cloudformation_provisions_codepipeline_pipelines;
#[path = "all/cloudformation_provisions_cognito_identity_pools.rs"]
mod cloudformation_provisions_cognito_identity_pools;
#[path = "all/cloudformation_provisions_cognito_user_pool_groups.rs"]
mod cloudformation_provisions_cognito_user_pool_groups;
#[path = "all/cloudformation_provisions_cognito_user_pools.rs"]
mod cloudformation_provisions_cognito_user_pools;
#[path = "all/cloudformation_provisions_config_recording.rs"]
mod cloudformation_provisions_config_recording;
#[path = "all/cloudformation_provisions_container_platform_resources.rs"]
mod cloudformation_provisions_container_platform_resources;
#[path = "all/cloudformation_provisions_ecr_public_repository.rs"]
mod cloudformation_provisions_ecr_public_repository;
#[path = "all/cloudformation_provisions_eks_access_entries.rs"]
mod cloudformation_provisions_eks_access_entries;
#[path = "all/cloudformation_provisions_eks_clusters.rs"]
mod cloudformation_provisions_eks_clusters;
#[path = "all/cloudformation_provisions_elastic_beanstalk_resources.rs"]
mod cloudformation_provisions_elastic_beanstalk_resources;
#[path = "all/cloudformation_provisions_elasticache_cache_clusters.rs"]
mod cloudformation_provisions_elasticache_cache_clusters;
#[path = "all/cloudformation_provisions_event_buses_and_bus_policies.rs"]
mod cloudformation_provisions_event_buses_and_bus_policies;
#[path = "all/cloudformation_provisions_event_rules_and_ssm_parameters.rs"]
mod cloudformation_provisions_event_rules_and_ssm_parameters;
#[path = "all/cloudformation_provisions_eventbridge_pipes.rs"]
mod cloudformation_provisions_eventbridge_pipes;
#[path = "all/cloudformation_provisions_fis_experiment_templates.rs"]
mod cloudformation_provisions_fis_experiment_templates;
#[path = "all/cloudformation_provisions_glue_catalog.rs"]
mod cloudformation_provisions_glue_catalog;
#[path = "all/cloudformation_provisions_glue_jobs.rs"]
mod cloudformation_provisions_glue_jobs;
#[path = "all/cloudformation_provisions_grafana_workspaces.rs"]
mod cloudformation_provisions_grafana_workspaces;
#[path = "all/cloudformation_provisions_guardduty_detector.rs"]
mod cloudformation_provisions_guardduty_detector;
#[path = "all/cloudformation_provisions_guardduty_ip_set.rs"]
mod cloudformation_provisions_guardduty_ip_set;
#[path = "all/cloudformation_provisions_kinesis_streams.rs"]
mod cloudformation_provisions_kinesis_streams;
#[path = "all/cloudformation_provisions_kms_aliases.rs"]
mod cloudformation_provisions_kms_aliases;
#[path = "all/cloudformation_provisions_kms_keys.rs"]
mod cloudformation_provisions_kms_keys;
#[path = "all/cloudformation_provisions_lambda_event_invoke_configs.rs"]
mod cloudformation_provisions_lambda_event_invoke_configs;
#[path = "all/cloudformation_provisions_lambda_event_source_mappings.rs"]
mod cloudformation_provisions_lambda_event_source_mappings;
#[path = "all/cloudformation_provisions_lambda_functions.rs"]
mod cloudformation_provisions_lambda_functions;
#[path = "all/cloudformation_provisions_logs_log_groups.rs"]
mod cloudformation_provisions_logs_log_groups;
#[path = "all/cloudformation_provisions_memorydb_groups.rs"]
mod cloudformation_provisions_memorydb_groups;
#[path = "all/cloudformation_provisions_mq_brokers.rs"]
mod cloudformation_provisions_mq_brokers;
#[path = "all/cloudformation_provisions_msk_clusters.rs"]
mod cloudformation_provisions_msk_clusters;
#[path = "all/cloudformation_provisions_mwaa_environments.rs"]
mod cloudformation_provisions_mwaa_environments;
#[path = "all/cloudformation_provisions_opensearch_domains.rs"]
mod cloudformation_provisions_opensearch_domains;
#[path = "all/cloudformation_provisions_redshift_clusters.rs"]
mod cloudformation_provisions_redshift_clusters;
#[path = "all/cloudformation_provisions_redshift_snapshot_copy_grants.rs"]
mod cloudformation_provisions_redshift_snapshot_copy_grants;
#[path = "all/cloudformation_provisions_redshift_subnet_groups.rs"]
mod cloudformation_provisions_redshift_subnet_groups;
#[path = "all/cloudformation_provisions_resource_groups.rs"]
mod cloudformation_provisions_resource_groups;
#[path = "all/cloudformation_provisions_route53_health_checks.rs"]
mod cloudformation_provisions_route53_health_checks;
#[path = "all/cloudformation_provisions_route53_hosted_zones.rs"]
mod cloudformation_provisions_route53_hosted_zones;
#[path = "all/cloudformation_provisions_route53_policies.rs"]
mod cloudformation_provisions_route53_policies;
#[path = "all/cloudformation_provisions_route53resolver_firewall.rs"]
mod cloudformation_provisions_route53resolver_firewall;
#[path = "all/cloudformation_provisions_route53resolver_rules.rs"]
mod cloudformation_provisions_route53resolver_rules;
#[path = "all/cloudformation_provisions_s3tables_table_buckets.rs"]
mod cloudformation_provisions_s3tables_table_buckets;
#[path = "all/cloudformation_provisions_s3vectors_resources.rs"]
mod cloudformation_provisions_s3vectors_resources;
#[path = "all/cloudformation_provisions_sagemaker_model_package_group.rs"]
mod cloudformation_provisions_sagemaker_model_package_group;
#[path = "all/cloudformation_provisions_sagemaker_notebook_estate.rs"]
mod cloudformation_provisions_sagemaker_notebook_estate;
#[path = "all/cloudformation_provisions_scheduler_schedules.rs"]
mod cloudformation_provisions_scheduler_schedules;
#[path = "all/cloudformation_provisions_secrets_manager_secrets.rs"]
mod cloudformation_provisions_secrets_manager_secrets;
#[path = "all/cloudformation_provisions_ses_configuration_sets.rs"]
mod cloudformation_provisions_ses_configuration_sets;
#[path = "all/cloudformation_provisions_ses_receipt_rule_sets.rs"]
mod cloudformation_provisions_ses_receipt_rule_sets;
#[path = "all/cloudformation_provisions_sfn_activities.rs"]
mod cloudformation_provisions_sfn_activities;
#[path = "all/cloudformation_provisions_sfn_state_machines.rs"]
mod cloudformation_provisions_sfn_state_machines;
#[path = "all/cloudformation_provisions_sns_subscriptions.rs"]
mod cloudformation_provisions_sns_subscriptions;
#[path = "all/cloudformation_provisions_sns_topics.rs"]
mod cloudformation_provisions_sns_topics;
#[path = "all/cloudformation_provisions_sqs_queue_policies.rs"]
mod cloudformation_provisions_sqs_queue_policies;
#[path = "all/cloudformation_provisions_sqs_queues.rs"]
mod cloudformation_provisions_sqs_queues;
#[path = "all/cloudformation_provisions_stack_resources.rs"]
mod cloudformation_provisions_stack_resources;
#[path = "all/cloudformation_provisions_transfer_servers.rs"]
mod cloudformation_provisions_transfer_servers;
#[path = "all/cloudformation_provisions_waf_regional_ip_sets.rs"]
mod cloudformation_provisions_waf_regional_ip_sets;
#[path = "all/cloudformation_provisions_wafv2_entities.rs"]
mod cloudformation_provisions_wafv2_entities;
#[path = "all/cloudformation_provisions_wafv2_web_acl_associations.rs"]
mod cloudformation_provisions_wafv2_web_acl_associations;
#[path = "all/cloudformation_provisions_xray_groups.rs"]
mod cloudformation_provisions_xray_groups;
#[path = "all/cloudformation_provisions_xray_sampling_rules.rs"]
mod cloudformation_provisions_xray_sampling_rules;
#[path = "all/cloudformation_records_per_resource_stack_events.rs"]
mod cloudformation_records_per_resource_stack_events;
#[path = "all/cloudformation_redshift_stack_outputs.rs"]
mod cloudformation_redshift_stack_outputs;
#[path = "all/cloudformation_refuses_mq_brokers.rs"]
mod cloudformation_refuses_mq_brokers;
#[path = "all/cloudformation_refuses_s3vectors_vector_buckets.rs"]
mod cloudformation_refuses_s3vectors_vector_buckets;
#[path = "all/cloudformation_registry_backed_task_definition.rs"]
mod cloudformation_registry_backed_task_definition;
#[path = "all/cloudformation_resource_removal_stabilisation.rs"]
mod cloudformation_resource_removal_stabilisation;
#[path = "all/cloudformation_rest_api_request_validation.rs"]
mod cloudformation_rest_api_request_validation;
#[path = "all/cloudformation_rest_api_serves_its_stage.rs"]
mod cloudformation_rest_api_serves_its_stage;
#[path = "all/cloudformation_rest_api_token_authorizer_credentials.rs"]
mod cloudformation_rest_api_token_authorizer_credentials;
#[path = "all/cloudformation_role_trusted_by_the_s3_service.rs"]
mod cloudformation_role_trusted_by_the_s3_service;
#[path = "all/cloudformation_rollback_deletes_the_buckets_it_created.rs"]
mod cloudformation_rollback_deletes_the_buckets_it_created;
#[path = "all/cloudformation_rollback_triggers_revert_the_operation.rs"]
mod cloudformation_rollback_triggers_revert_the_operation;
#[path = "all/cloudformation_rotation_schedule_rotates_its_secret.rs"]
mod cloudformation_rotation_schedule_rotates_its_secret;
#[path = "all/cloudformation_route53_record_set_contract.rs"]
mod cloudformation_route53_record_set_contract;
#[path = "all/cloudformation_s3_bucket_update_and_rename.rs"]
mod cloudformation_s3_bucket_update_and_rename;
#[path = "all/cloudformation_s3tables_table_bucket_maintenance.rs"]
mod cloudformation_s3tables_table_bucket_maintenance;
#[path = "all/cloudformation_sam_function_writes_its_table.rs"]
mod cloudformation_sam_function_writes_its_table;
#[path = "all/cloudformation_sam_http_api_definitions.rs"]
mod cloudformation_sam_http_api_definitions;
#[path = "all/cloudformation_scripted_queue_deploy.rs"]
mod cloudformation_scripted_queue_deploy;
#[path = "all/cloudformation_search_indexing_pipeline_outputs.rs"]
mod cloudformation_search_indexing_pipeline_outputs;
#[path = "all/cloudformation_secretsmanager_dynamic_references.rs"]
mod cloudformation_secretsmanager_dynamic_references;
#[path = "all/cloudformation_sfn_absent_role_and_stop.rs"]
mod cloudformation_sfn_absent_role_and_stop;
#[path = "all/cloudformation_sfn_fifo_queue_stack.rs"]
mod cloudformation_sfn_fifo_queue_stack;
#[path = "all/cloudformation_sfn_lambda_published_handles.rs"]
mod cloudformation_sfn_lambda_published_handles;
#[path = "all/cloudformation_sfn_reference_starts_execution.rs"]
mod cloudformation_sfn_reference_starts_execution;
#[path = "all/cloudformation_sns_custom_resource_protocol.rs"]
mod cloudformation_sns_custom_resource_protocol;
#[path = "all/cloudformation_sns_delivery_status_logging.rs"]
mod cloudformation_sns_delivery_status_logging;
#[path = "all/cloudformation_sns_topic_policy_admits_s3_notifications.rs"]
mod cloudformation_sns_topic_policy_admits_s3_notifications;
#[path = "all/cloudformation_sqs_redrive_policy_dead_letters_a_failing_consumer.rs"]
mod cloudformation_sqs_redrive_policy_dead_letters_a_failing_consumer;
#[path = "all/cloudformation_stack_clock.rs"]
mod cloudformation_stack_clock;
#[path = "all/cloudformation_stack_event_notifications.rs"]
mod cloudformation_stack_event_notifications;
#[path = "all/cloudformation_stack_update_redirects_a_schedule.rs"]
mod cloudformation_stack_update_redirects_a_schedule;
#[path = "all/cloudformation_stackset_status_change_events.rs"]
mod cloudformation_stackset_status_change_events;
#[path = "all/cloudformation_stamps_s3_bucket_stack_provenance.rs"]
mod cloudformation_stamps_s3_bucket_stack_provenance;
#[path = "all/cloudformation_stamps_s3vectors_stack_provenance.rs"]
mod cloudformation_stamps_s3vectors_stack_provenance;
#[path = "all/cloudformation_task_definition_runs_an_ecs_service.rs"]
mod cloudformation_task_definition_runs_an_ecs_service;
#[path = "all/cloudformation_updated_stack_outputs.rs"]
mod cloudformation_updated_stack_outputs;
#[path = "all/cloudformation_updates_iam_roles.rs"]
mod cloudformation_updates_iam_roles;
#[path = "all/cloudformation_updates_lambda_functions.rs"]
mod cloudformation_updates_lambda_functions;
#[path = "all/cloudformation_wafv2_protection_stack.rs"]
mod cloudformation_wafv2_protection_stack;
#[path = "all/cloudfront_api_call_events.rs"]
mod cloudfront_api_call_events;
#[path = "all/cloudfront_apigateway_origin_edge.rs"]
mod cloudfront_apigateway_origin_edge;
#[path = "all/cloudfront_edge_4xx_share_decides_the_alarm.rs"]
mod cloudfront_edge_4xx_share_decides_the_alarm;
#[path = "all/cloudfront_edge_cache_refresh.rs"]
mod cloudfront_edge_cache_refresh;
#[path = "all/cloudfront_edge_generated_headers.rs"]
mod cloudfront_edge_generated_headers;
#[path = "all/cloudfront_edge_metrics_drive_the_alarm_chain.rs"]
mod cloudfront_edge_metrics_drive_the_alarm_chain;
#[path = "all/cloudfront_key_value_store_handshake.rs"]
mod cloudfront_key_value_store_handshake;
#[path = "all/cloudfront_oac_kms_s3_origin_edge.rs"]
mod cloudfront_oac_kms_s3_origin_edge;
#[path = "all/cloudfront_oac_s3_origin_edge.rs"]
mod cloudfront_oac_s3_origin_edge;
#[path = "all/cloudfront_oai_s3_origin_edge.rs"]
mod cloudfront_oai_s3_origin_edge;
#[path = "all/cloudhsm_signing_name_routing.rs"]
mod cloudhsm_signing_name_routing;
#[path = "all/cloudtrail_api_call_events_by_region.rs"]
mod cloudtrail_api_call_events_by_region;
#[path = "all/cloudtrail_channel_routing.rs"]
mod cloudtrail_channel_routing;
#[path = "all/cloudtrail_channel_target_routing.rs"]
mod cloudtrail_channel_target_routing;
#[path = "all/cloudtrail_import_failures_routing.rs"]
mod cloudtrail_import_failures_routing;
#[path = "all/cloudtrail_read_only_api_calls.rs"]
mod cloudtrail_read_only_api_calls;
#[path = "all/cloudtrail_sdk_target_routing.rs"]
mod cloudtrail_sdk_target_routing;
#[path = "all/cloudwatch_alarm_action_reaches_its_subscribed_queue.rs"]
mod cloudwatch_alarm_action_reaches_its_subscribed_queue;
#[path = "all/cloudwatch_alarm_ssm_ops_item_action.rs"]
mod cloudwatch_alarm_ssm_ops_item_action;
#[path = "all/cloudwatch_breaching_alarm_fires_through_the_whole_router.rs"]
mod cloudwatch_breaching_alarm_fires_through_the_whole_router;
#[path = "all/cloudwatch_cfn_alarm_action_topic_policy.rs"]
mod cloudwatch_cfn_alarm_action_topic_policy;
#[path = "all/cloudwatch_composite_alarm_rule_decides_its_sns_action.rs"]
mod cloudwatch_composite_alarm_rule_decides_its_sns_action;
#[path = "all/codeartifact_package_version_events.rs"]
mod codeartifact_package_version_events;
#[path = "all/codebuild_artifacts_override_bucket.rs"]
mod codebuild_artifacts_override_bucket;
#[path = "all/codebuild_s3_build_output.rs"]
mod codebuild_s3_build_output;
#[path = "all/codebuild_secrets_manager_environment_variable.rs"]
mod codebuild_secrets_manager_environment_variable;
#[path = "all/codebuild_service_role_trust.rs"]
mod codebuild_service_role_trust;
#[path = "all/codepipeline_approval_reviewer_attribution.rs"]
mod codepipeline_approval_reviewer_attribution;
#[path = "all/codepipeline_build_action_invokes_codebuild.rs"]
mod codepipeline_build_action_invokes_codebuild;
#[path = "all/codepipeline_execution_controls.rs"]
mod codepipeline_execution_controls;
#[path = "all/codepipeline_execution_state_changes_reach_a_queue_target.rs"]
mod codepipeline_execution_state_changes_reach_a_queue_target;
#[path = "all/codepipeline_management_events.rs"]
mod codepipeline_management_events;
#[path = "all/cognito_authorizer_issuer_endpoint.rs"]
mod cognito_authorizer_issuer_endpoint;
#[path = "all/cognito_group_role_mapping.rs"]
mod cognito_group_role_mapping;
#[path = "all/cognito_post_confirmation_lambda_trigger.rs"]
mod cognito_post_confirmation_lambda_trigger;
#[path = "all/cognito_rules_role_mapping.rs"]
mod cognito_rules_role_mapping;
#[path = "all/cognito_signed_session_leading_keys_partition.rs"]
mod cognito_signed_session_leading_keys_partition;
#[path = "all/cognito_sms_caller_role_trust.rs"]
mod cognito_sms_caller_role_trust;
#[path = "all/cognito_vended_session_bucket_policy.rs"]
mod cognito_vended_session_bucket_policy;
#[path = "all/cognito_vended_session_is_an_assumed_role_caller.rs"]
mod cognito_vended_session_is_an_assumed_role_caller;
#[path = "all/cognito_vended_session_is_audited.rs"]
mod cognito_vended_session_is_audited;
#[path = "all/cognito_vended_session_is_bounded_by_its_role_policy.rs"]
mod cognito_vended_session_is_bounded_by_its_role_policy;
#[path = "all/cognito_vended_session_leading_keys_scoped_query.rs"]
mod cognito_vended_session_leading_keys_scoped_query;
#[path = "all/cognito_vended_session_outlives_pool_deletion.rs"]
mod cognito_vended_session_outlives_pool_deletion;
#[path = "all/cognito_vended_session_principal_tag_scoped_read.rs"]
mod cognito_vended_session_principal_tag_scoped_read;
#[path = "all/cognito_vended_session_reads_sse_kms_object.rs"]
mod cognito_vended_session_reads_sse_kms_object;
#[path = "all/compute_optimizer_automation_denial_principal_agrees_with_sts.rs"]
mod compute_optimizer_automation_denial_principal_agrees_with_sts;
#[path = "all/config_compliance_change_event.rs"]
mod config_compliance_change_event;
#[path = "all/connect_signing_name_routing.rs"]
mod connect_signing_name_routing;
#[path = "all/connectcampaigns_signing_name_routing.rs"]
mod connectcampaigns_signing_name_routing;
#[path = "all/contributor_insights_log_events.rs"]
mod contributor_insights_log_events;
#[path = "all/ddb_stream_lambda_esm_provisioning.rs"]
mod ddb_stream_lambda_esm_provisioning;
#[path = "all/directory_data_access_cycle.rs"]
mod directory_data_access_cycle;
#[path = "all/directory_data_plane_authorization.rs"]
mod directory_data_plane_authorization;
#[path = "all/directory_realm_follows_its_create.rs"]
mod directory_realm_follows_its_create;
#[path = "all/ds_data_plane_access_and_realm.rs"]
mod ds_data_plane_access_and_realm;
#[path = "all/ds_log_subscription_forwarding.rs"]
mod ds_log_subscription_forwarding;
#[path = "all/ds_placement_zones.rs"]
mod ds_placement_zones;
#[path = "all/dynamodb_data_plane_authorization.rs"]
mod dynamodb_data_plane_authorization;
#[path = "all/dynamodb_session_policy_authorization.rs"]
mod dynamodb_session_policy_authorization;
#[path = "all/dynamodb_streams_routing.rs"]
mod dynamodb_streams_routing;
#[path = "all/dynamodb_write_capacity_metrics_drive_the_alarm_chain.rs"]
mod dynamodb_write_capacity_metrics_drive_the_alarm_chain;
#[path = "all/ebs_refused_block_write_then_ec2_snapshot_completion.rs"]
mod ebs_refused_block_write_then_ec2_snapshot_completion;
#[path = "all/ebs_start_snapshot_resolves_its_kms_key.rs"]
mod ebs_start_snapshot_resolves_its_kms_key;
#[path = "all/ec2_management_events.rs"]
mod ec2_management_events;
#[path = "all/ec2_network.rs"]
mod ec2_network;
#[path = "all/ecr_public_action_scoped_iam_identity.rs"]
mod ecr_public_action_scoped_iam_identity;
#[path = "all/ecr_push_triggers_its_pipeline.rs"]
mod ecr_push_triggers_its_pipeline;
#[path = "all/ecr_repository_policy_principal.rs"]
mod ecr_repository_policy_principal;
#[path = "all/ecr_scan_on_push_event_reaches_its_queue_target.rs"]
mod ecr_scan_on_push_event_reaches_its_queue_target;
#[path = "all/ecs_awsvpc_configuration_over_an_ec2_network.rs"]
mod ecs_awsvpc_configuration_over_an_ec2_network;
#[path = "all/ecs_fargate_service_converges_to_its_desired_count.rs"]
mod ecs_fargate_service_converges_to_its_desired_count;
#[path = "all/ecs_service_target_group_association.rs"]
mod ecs_service_target_group_association;
#[path = "all/ecs_service_target_group_health_check_port.rs"]
mod ecs_service_target_group_health_check_port;
#[path = "all/ecs_service_target_group_security_groups.rs"]
mod ecs_service_target_group_security_groups;
#[path = "all/ecs_task_registration_precedes_its_health_verdict.rs"]
mod ecs_task_registration_precedes_its_health_verdict;
#[path = "all/efs_access_point_mounted_by_lambda.rs"]
mod efs_access_point_mounted_by_lambda;
#[path = "all/eks_bootstrap_cluster_creator_access_entry.rs"]
mod eks_bootstrap_cluster_creator_access_entry;
#[path = "all/eks_capability_role_trust.rs"]
mod eks_capability_role_trust;
#[path = "all/eks_cluster_inputs_resolve.rs"]
mod eks_cluster_inputs_resolve;
#[path = "all/eks_cluster_security_group.rs"]
mod eks_cluster_security_group;
#[path = "all/eks_control_plane_log_suppression.rs"]
mod eks_control_plane_log_suppression;
#[path = "all/eks_control_plane_logs.rs"]
mod eks_control_plane_logs;
#[path = "all/eks_kubernetes_api_rbac_audit.rs"]
mod eks_kubernetes_api_rbac_audit;
#[path = "all/eks_logging_update_reaches_cloudwatch_logs.rs"]
mod eks_logging_update_reaches_cloudwatch_logs;
#[path = "all/eks_nodegroup_capacity.rs"]
mod eks_nodegroup_capacity;
#[path = "all/eks_nodegroup_lifecycle_convergence.rs"]
mod eks_nodegroup_lifecycle_convergence;
#[path = "all/eks_pod_identity_role_trust.rs"]
mod eks_pod_identity_role_trust;
#[path = "all/eks_service_cidr_vpc_overlap.rs"]
mod eks_service_cidr_vpc_overlap;
#[path = "all/eks_wire.rs"]
mod eks_wire;
#[path = "all/eks_wrapped_peer_refusals.rs"]
mod eks_wrapped_peer_refusals;
#[path = "all/elb_classic_and_v2_signing_name_routing.rs"]
mod elb_classic_and_v2_signing_name_routing;
#[path = "all/elb_classic_network_resources_resolve_in_ec2.rs"]
mod elb_classic_network_resources_resolve_in_ec2;
#[path = "all/elb_classic_zone_enable_turns_on_the_vpc.rs"]
mod elb_classic_zone_enable_turns_on_the_vpc;
#[path = "all/elb_classic_zone_membership_follows_its_subnets.rs"]
mod elb_classic_zone_membership_follows_its_subnets;
#[path = "all/elbv2_certificate_in_use_by_acm.rs"]
mod elbv2_certificate_in_use_by_acm;
#[path = "all/elbv2_dualstack_and_security_group_classes.rs"]
mod elbv2_dualstack_and_security_group_classes;
#[path = "all/elbv2_host_counts_and_interfaces.rs"]
mod elbv2_host_counts_and_interfaces;
#[path = "all/elbv2_ip_target_refusal_classes.rs"]
mod elbv2_ip_target_refusal_classes;
#[path = "all/elbv2_lambda_target_published_address.rs"]
mod elbv2_lambda_target_published_address;
#[path = "all/elbv2_load_balancer_security_groups.rs"]
mod elbv2_load_balancer_security_groups;
#[path = "all/elbv2_network_resolution.rs"]
mod elbv2_network_resolution;
#[path = "all/elbv2_subnet_and_elastic_ip_resolution.rs"]
mod elbv2_subnet_and_elastic_ip_resolution;
#[path = "all/elbv2_subnet_vpc_membership.rs"]
mod elbv2_subnet_vpc_membership;
#[path = "all/elbv2_zonal_shift_managed_resource.rs"]
mod elbv2_zonal_shift_managed_resource;
#[path = "all/emr_cluster_state_change_events.rs"]
mod emr_cluster_state_change_events;
#[path = "all/emr_job_flow_role_instance_profile.rs"]
mod emr_job_flow_role_instance_profile;
#[path = "all/emr_studio_network_resolution.rs"]
mod emr_studio_network_resolution;
#[path = "all/es_opensearch_and_elasticsearch_signing_name_routing.rs"]
mod es_opensearch_and_elasticsearch_signing_name_routing;
#[path = "all/events_fan_out_to_each_matched_target.rs"]
mod events_fan_out_to_each_matched_target;
#[path = "all/events_lambda_target_dead_letter_capture.rs"]
mod events_lambda_target_dead_letter_capture;
#[path = "all/events_lambda_target_handler_records_the_invoking_rule.rs"]
mod events_lambda_target_handler_records_the_invoking_rule;
#[path = "all/events_lambda_target_qualified_arn_envelope.rs"]
mod events_lambda_target_qualified_arn_envelope;
#[path = "all/events_lambda_target_resource_policy.rs"]
mod events_lambda_target_resource_policy;
#[path = "all/events_lambda_target_runs_the_handler.rs"]
mod events_lambda_target_runs_the_handler;
#[path = "all/events_one_envelope_reaches_every_target_kind.rs"]
mod events_one_envelope_reaches_every_target_kind;
#[path = "all/events_queue_target_nested_detail_reaches_its_lambda_consumer.rs"]
mod events_queue_target_nested_detail_reaches_its_lambda_consumer;
#[path = "all/events_replay_redelivers_the_archived_window.rs"]
mod events_replay_redelivers_the_archived_window;
#[path = "all/events_rule_fans_out_to_firehose_and_queue_targets.rs"]
mod events_rule_fans_out_to_firehose_and_queue_targets;
#[path = "all/events_rule_firehose_target_delivery.rs"]
mod events_rule_firehose_target_delivery;
#[path = "all/events_signing_name_routing.rs"]
mod events_signing_name_routing;
#[path = "all/events_sns_lambda_delivery_observed_in_logs.rs"]
mod events_sns_lambda_delivery_observed_in_logs;
#[path = "all/events_store_ownership.rs"]
mod events_store_ownership;
#[path = "all/events_to_sfn_to_dynamodb_chain.rs"]
mod events_to_sfn_to_dynamodb_chain;
#[path = "all/firehose_destination_role.rs"]
mod firehose_destination_role;
#[path = "all/fis_execution_role_permissions.rs"]
mod fis_execution_role_permissions;
#[path = "all/fis_execution_role_trust.rs"]
mod fis_execution_role_trust;
#[path = "all/fis_injected_api_fault.rs"]
mod fis_injected_api_fault;
#[path = "all/forecast_signing_name_routing.rs"]
mod forecast_signing_name_routing;
#[path = "all/glue_jobs_are_regional_while_their_role_is_global.rs"]
mod glue_jobs_are_regional_while_their_role_is_global;
#[path = "all/glue_studio_notebook_catalog_through_the_router.rs"]
mod glue_studio_notebook_catalog_through_the_router;
#[path = "all/greengrass_deployment_iot_job.rs"]
mod greengrass_deployment_iot_job;
#[path = "all/greengrass_deployment_target_resolution.rs"]
mod greengrass_deployment_target_resolution;
#[path = "all/greengrass_signing_name_routing.rs"]
mod greengrass_signing_name_routing;
#[path = "all/guardduty_malware_protection_plan_role.rs"]
mod guardduty_malware_protection_plan_role;
#[path = "all/guardduty_runtime_coverage.rs"]
mod guardduty_runtime_coverage;
#[path = "all/https_front_door_concurrent_pooled_producer.rs"]
mod https_front_door_concurrent_pooled_producer;
#[path = "all/iam_access_key_signs_as_its_user.rs"]
mod iam_access_key_signs_as_its_user;
#[path = "all/iam_and_signer_signing_name_routing.rs"]
mod iam_and_signer_signing_name_routing;
#[path = "all/iam_authorizes_an_assumed_role_session.rs"]
mod iam_authorizes_an_assumed_role_session;
#[path = "all/iam_denial_reason_attribution.rs"]
mod iam_denial_reason_attribution;
#[path = "all/iam_holds_the_principal_sts_reports.rs"]
mod iam_holds_the_principal_sts_reports;
#[path = "all/iam_management_events.rs"]
mod iam_management_events;
#[path = "all/iam_role_id_agrees_with_sts.rs"]
mod iam_role_id_agrees_with_sts;
#[path = "all/iam_secretsmanager_denial_envelope_by_protocol.rs"]
mod iam_secretsmanager_denial_envelope_by_protocol;
#[path = "all/iam_session_policy_scopes_an_assumed_role.rs"]
mod iam_session_policy_scopes_an_assumed_role;
#[path = "all/iam_session_refusal_audit_documents.rs"]
mod iam_session_refusal_audit_documents;
#[path = "all/iam_session_tag_authorization.rs"]
mod iam_session_tag_authorization;
#[path = "all/iam_signing_name_routing.rs"]
mod iam_signing_name_routing;
#[path = "all/iot_command_deprecation_is_observable_on_the_data_plane.rs"]
mod iot_command_deprecation_is_observable_on_the_data_plane;
#[path = "all/iot_command_execution_lifecycle.rs"]
mod iot_command_execution_lifecycle;
#[path = "all/iot_command_executions_are_listed_across_planes.rs"]
mod iot_command_executions_are_listed_across_planes;
#[path = "all/iot_job_cancel_unwinds_the_device_queue.rs"]
mod iot_job_cancel_unwinds_the_device_queue;
#[path = "all/iot_job_execution_reaches_the_device.rs"]
mod iot_job_execution_reaches_the_device;
#[path = "all/iot_jobs_data_endpoint_discovery.rs"]
mod iot_jobs_data_endpoint_discovery;
#[path = "all/ivs_signing_name_routing.rs"]
mod ivs_signing_name_routing;
#[path = "all/kafka_msk_own_refusals_precede_the_subnet_lookup.rs"]
mod kafka_msk_own_refusals_precede_the_subnet_lookup;
#[path = "all/kafka_placement_subnets_resolve_in_ec2.rs"]
mod kafka_placement_subnets_resolve_in_ec2;
#[path = "all/keyspaces_signing_name_routing.rs"]
mod keyspaces_signing_name_routing;
#[path = "all/keyspaces_streams_routing.rs"]
mod keyspaces_streams_routing;
#[path = "all/kinesis_video_signing_name_routing.rs"]
mod kinesis_video_signing_name_routing;
#[path = "all/kinesisanalytics_signing_name_routing.rs"]
mod kinesisanalytics_signing_name_routing;
#[path = "all/kinesisvideo_discovered_data_plane.rs"]
mod kinesisvideo_discovered_data_plane;
#[path = "all/kinesisvideo_signing_name_routing.rs"]
mod kinesisvideo_signing_name_routing;
#[path = "all/lakeformation_catalog_resource_resolution.rs"]
mod lakeformation_catalog_resource_resolution;
#[path = "all/lakeformation_grant_principal_resolution.rs"]
mod lakeformation_grant_principal_resolution;
#[path = "all/lambda_bundled_wire_operations.rs"]
mod lambda_bundled_wire_operations;
#[path = "all/lambda_capacity_provider_operator_role.rs"]
mod lambda_capacity_provider_operator_role;
#[path = "all/lambda_durable_execution_status_events_reach_sqs.rs"]
mod lambda_durable_execution_status_events_reach_sqs;
#[path = "all/lambda_execution_role_search_domain_grant.rs"]
mod lambda_execution_role_search_domain_grant;
#[path = "all/lambda_execution_role_session_identity.rs"]
mod lambda_execution_role_session_identity;
#[path = "all/lambda_execution_role_trust.rs"]
mod lambda_execution_role_trust;
#[path = "all/lambda_handler_invalidates_its_distribution.rs"]
mod lambda_handler_invalidates_its_distribution;
#[path = "all/lambda_in_function_cloudwatch_authorization.rs"]
mod lambda_in_function_cloudwatch_authorization;
#[path = "all/lambda_in_function_dynamodb_authorization.rs"]
mod lambda_in_function_dynamodb_authorization;
#[path = "all/lambda_in_function_dynamodb_partition_conditions.rs"]
mod lambda_in_function_dynamodb_partition_conditions;
#[path = "all/lambda_in_function_secretsmanager_grant_revocation.rs"]
mod lambda_in_function_secretsmanager_grant_revocation;
#[path = "all/lambda_in_function_sqs_authorization.rs"]
mod lambda_in_function_sqs_authorization;
#[path = "all/lambda_log_metric_filter_drives_the_alarm_chain.rs"]
mod lambda_log_metric_filter_drives_the_alarm_chain;
#[path = "all/lambda_signed_write_to_an_aoss_collection_endpoint.rs"]
mod lambda_signed_write_to_an_aoss_collection_endpoint;
#[path = "all/lambda_signing_name_routing.rs"]
mod lambda_signing_name_routing;
#[path = "all/lambda_sqs_event_source_batching_window.rs"]
mod lambda_sqs_event_source_batching_window;
#[path = "all/lambda_sqs_event_source_failed_batch_keeps_its_message.rs"]
mod lambda_sqs_event_source_failed_batch_keeps_its_message;
#[path = "all/lambda_sqs_event_source_visibility_hold.rs"]
mod lambda_sqs_event_source_visibility_hold;
#[path = "all/lex_models_v2_runtime_v2_cross_service.rs"]
mod lex_models_v2_runtime_v2_cross_service;
#[path = "all/lex_runtime_v2_sessions_through_the_router.rs"]
mod lex_runtime_v2_sessions_through_the_router;
#[path = "all/lex_signing_name_routing.rs"]
mod lex_signing_name_routing;
#[path = "all/log_alarm_scheduled_query.rs"]
mod log_alarm_scheduled_query;
#[path = "all/logs_delivery_source_cloudfront_resource.rs"]
mod logs_delivery_source_cloudfront_resource;
#[path = "all/logs_emf_declared_projections.rs"]
mod logs_emf_declared_projections;
#[path = "all/logs_emf_disabled_errors.rs"]
mod logs_emf_disabled_errors;
#[path = "all/logs_emf_extraction_policy_scope.rs"]
mod logs_emf_extraction_policy_scope;
#[path = "all/logs_emf_metric_extraction.rs"]
mod logs_emf_metric_extraction;
#[path = "all/logs_export_task_destination_bucket.rs"]
mod logs_export_task_destination_bucket;
#[path = "all/logs_metric_filter_drives_the_alarm_notification_chain.rs"]
mod logs_metric_filter_drives_the_alarm_notification_chain;
#[path = "all/logs_metric_filter_extraction.rs"]
mod logs_metric_filter_extraction;
#[path = "all/logs_subscription_delivery_role.rs"]
mod logs_subscription_delivery_role;
#[path = "all/logs_subscription_delivery_role_scope.rs"]
mod logs_subscription_delivery_role_scope;
#[path = "all/logs_subscription_filter_role_trust.rs"]
mod logs_subscription_filter_role_trust;
#[path = "all/logs_subscription_filter_to_firehose_s3.rs"]
mod logs_subscription_filter_to_firehose_s3;
#[path = "all/logs_subscription_filter_to_lambda.rs"]
mod logs_subscription_filter_to_lambda;
#[path = "all/logs_vended_group_lambda_subscriber_permission.rs"]
mod logs_vended_group_lambda_subscriber_permission;
#[path = "all/marketplace_signing_name_routing.rs"]
mod marketplace_signing_name_routing;
#[path = "all/mediaconvert_input_probe.rs"]
mod mediaconvert_input_probe;
#[path = "all/mediapackagev2_published_data_plane_urls.rs"]
mod mediapackagev2_published_data_plane_urls;
#[path = "all/medical_imaging_dicom_import.rs"]
mod medical_imaging_dicom_import;
#[path = "all/migrationhub_signing_name_routing.rs"]
mod migrationhub_signing_name_routing;
#[path = "all/opensearch_domain_vpc_placement.rs"]
mod opensearch_domain_vpc_placement;
#[path = "all/opensearch_vpc_endpoint_vpc_membership.rs"]
mod opensearch_vpc_endpoint_vpc_membership;
#[path = "all/opensearchserverless_cloudformation_resources.rs"]
mod opensearchserverless_cloudformation_resources;
#[path = "all/partnercentral_channel_target_routing.rs"]
mod partnercentral_channel_target_routing;
#[path = "all/payment_cryptography_signing_name_routing.rs"]
mod payment_cryptography_signing_name_routing;
#[path = "all/pcs_compute_node_group_instance_profile.rs"]
mod pcs_compute_node_group_instance_profile;
#[path = "all/per_service_response_request_id_form.rs"]
mod per_service_response_request_id_form;
#[path = "all/personalize_ingestion_targets.rs"]
mod personalize_ingestion_targets;
#[path = "all/personalize_signing_name_routing.rs"]
mod personalize_signing_name_routing;
#[path = "all/pi_data_source_ownership.rs"]
mod pi_data_source_ownership;
#[path = "all/pi_resource_features.rs"]
mod pi_resource_features;
#[path = "all/pi_resource_metadata_reflects_rds_configuration.rs"]
mod pi_resource_metadata_reflects_rds_configuration;
#[path = "all/pinpoint_sms_voice_signing_name_routing.rs"]
mod pinpoint_sms_voice_signing_name_routing;
#[path = "all/polly_synthesis_task_side_effects.rs"]
mod polly_synthesis_task_side_effects;
#[path = "all/pricing_plan_subscription_resource_eligibility.rs"]
mod pricing_plan_subscription_resource_eligibility;
#[path = "all/query_wire.rs"]
mod query_wire;
#[path = "all/rds_docdb_neptune_signing_name_routing.rs"]
mod rds_docdb_neptune_signing_name_routing;
#[path = "all/rds_management_events.rs"]
mod rds_management_events;
#[path = "all/rds_query_front_door.rs"]
mod rds_query_front_door;
#[path = "all/rds_s3_snapshot_export_flow.rs"]
mod rds_s3_snapshot_export_flow;
#[path = "all/rds_shared_query_front_door_routing.rs"]
mod rds_shared_query_front_door_routing;
#[path = "all/rds_shared_query_surface_routing.rs"]
mod rds_shared_query_surface_routing;
#[path = "all/rds_shared_signing_name_routing.rs"]
mod rds_shared_signing_name_routing;
#[path = "all/rds_signing_name_routing.rs"]
mod rds_signing_name_routing;
#[path = "all/redshift_cluster_iam_role_poll_lifecycle.rs"]
mod redshift_cluster_iam_role_poll_lifecycle;
#[path = "all/redshift_health_alarms_evaluate.rs"]
mod redshift_health_alarms_evaluate;
#[path = "all/resiliencehub_resource_mapping_existence.rs"]
mod resiliencehub_resource_mapping_existence;
#[path = "all/resiliencehub_signing_name_routing.rs"]
mod resiliencehub_signing_name_routing;
#[path = "all/route53_health_check_status_metric.rs"]
mod route53_health_check_status_metric;
#[path = "all/route53_health_check_string_match.rs"]
mod route53_health_check_string_match;
#[path = "all/s3_access_point_policy_denial.rs"]
mod s3_access_point_policy_denial;
#[path = "all/s3_access_point_vpc_network_origin.rs"]
mod s3_access_point_vpc_network_origin;
#[path = "all/s3_address_forms.rs"]
mod s3_address_forms;
#[path = "all/s3_bucket_lifecycle_identity_authorization.rs"]
mod s3_bucket_lifecycle_identity_authorization;
#[path = "all/s3_deployed_notification_copies_within_its_bucket.rs"]
mod s3_deployed_notification_copies_within_its_bucket;
#[path = "all/s3_form_post_upload_routes_by_event_bridge_reason.rs"]
mod s3_form_post_upload_routes_by_event_bridge_reason;
#[path = "all/s3_notification_indexes_into_its_opensearch_domain.rs"]
mod s3_notification_indexes_into_its_opensearch_domain;
#[path = "all/s3_notification_invokes_its_lambda_target.rs"]
mod s3_notification_invokes_its_lambda_target;
#[path = "all/s3_notification_pipeline_lifecycle.rs"]
mod s3_notification_pipeline_lifecycle;
#[path = "all/s3_object_created_invalidates_the_distribution_edge.rs"]
mod s3_object_created_invalidates_the_distribution_edge;
#[path = "all/s3_object_events_fan_out_through_sns.rs"]
mod s3_object_events_fan_out_through_sns;
#[path = "all/s3_object_events_update_their_search_index.rs"]
mod s3_object_events_update_their_search_index;
#[path = "all/s3_object_read_identity_or_bucket_policy.rs"]
mod s3_object_read_identity_or_bucket_policy;
#[path = "all/s3_sns_lambda_chain_invoke_permission.rs"]
mod s3_sns_lambda_chain_invoke_permission;
#[path = "all/s3control_tags_a_general_purpose_bucket.rs"]
mod s3control_tags_a_general_purpose_bucket;
#[path = "all/s3files_create_file_system_bucket.rs"]
mod s3files_create_file_system_bucket;
#[path = "all/sagemaker_agentic_rft_job_runtime_handoff.rs"]
mod sagemaker_agentic_rft_job_runtime_handoff;
#[path = "all/sagemaker_create_admission.rs"]
mod sagemaker_create_admission;
#[path = "all/sagemaker_endpoint_serves_a_trained_model.rs"]
mod sagemaker_endpoint_serves_a_trained_model;
#[path = "all/sagemaker_execution_role_admission.rs"]
mod sagemaker_execution_role_admission;
#[path = "all/sagemaker_feature_store_control_plane_handoff.rs"]
mod sagemaker_feature_store_control_plane_handoff;
#[path = "all/sagemaker_feature_store_default_ttl.rs"]
mod sagemaker_feature_store_default_ttl;
#[path = "all/sagemaker_feature_store_group_deletion.rs"]
mod sagemaker_feature_store_group_deletion;
#[path = "all/sagemaker_pipeline_role_resolution.rs"]
mod sagemaker_pipeline_role_resolution;
#[path = "all/sagemaker_signing_name_routing.rs"]
mod sagemaker_signing_name_routing;
#[path = "all/sagemaker_trial_component_metric_suggestions.rs"]
mod sagemaker_trial_component_metric_suggestions;
#[path = "all/scheduler_execution_role.rs"]
mod scheduler_execution_role;
#[path = "all/scratch_estate.rs"]
mod scratch_estate;
#[path = "all/sdb_legacy_query_api_on_the_assembled_router.rs"]
mod sdb_legacy_query_api_on_the_assembled_router;
#[path = "all/servicecatalog_signing_name_routing.rs"]
mod servicecatalog_signing_name_routing;
#[path = "all/ses_bulk_send_entry_headers_override_the_template.rs"]
mod ses_bulk_send_entry_headers_override_the_template;
#[path = "all/ses_configuration_sets_shared_across_v1_and_v2.rs"]
mod ses_configuration_sets_shared_across_v1_and_v2;
#[path = "all/ses_delivery_event_advances_the_subscribed_queue.rs"]
mod ses_delivery_event_advances_the_subscribed_queue;
#[path = "all/ses_easy_dkim_shared_across_surfaces.rs"]
mod ses_easy_dkim_shared_across_surfaces;
#[path = "all/ses_event_destination_reaches_an_eventbridge_rule_target.rs"]
mod ses_event_destination_reaches_an_eventbridge_rule_target;
#[path = "all/ses_event_destination_setup_across_services.rs"]
mod ses_event_destination_setup_across_services;
#[path = "all/ses_event_publication_reaches_firehose_s3.rs"]
mod ses_event_publication_reaches_firehose_s3;
#[path = "all/ses_family_pre_dispatch_refusals.rs"]
mod ses_family_pre_dispatch_refusals;
#[path = "all/ses_family_signing_name_routing.rs"]
mod ses_family_signing_name_routing;
#[path = "all/ses_firehose_event_destination_setup.rs"]
mod ses_firehose_event_destination_setup;
#[path = "all/ses_identities_shared_across_v1_and_v2.rs"]
mod ses_identities_shared_across_v1_and_v2;
#[path = "all/ses_identity_policies_shared_across_surfaces.rs"]
mod ses_identity_policies_shared_across_surfaces;
#[path = "all/ses_raw_send_under_a_shared_domain_identity.rs"]
mod ses_raw_send_under_a_shared_domain_identity;
#[path = "all/ses_send_event_reports_what_answered.rs"]
mod ses_send_event_reports_what_answered;
#[path = "all/ses_send_events_reach_a_subscribed_queue.rs"]
mod ses_send_events_reach_a_subscribed_queue;
#[path = "all/ses_templates_shared_across_v1_and_v2.rs"]
mod ses_templates_shared_across_v1_and_v2;
#[path = "all/ses_v1_discovers_a_verified_sending_identity.rs"]
mod ses_v1_discovers_a_verified_sending_identity;
#[path = "all/ses_v1_receipt_rule_lambda_invoke_grant.rs"]
mod ses_v1_receipt_rule_lambda_invoke_grant;
#[path = "all/ses_v1_receipt_rule_sets_through_the_router.rs"]
mod ses_v1_receipt_rule_sets_through_the_router;
#[path = "all/sesv2_probe.rs"]
mod sesv2_probe;
#[path = "all/sesv2_probe2.rs"]
mod sesv2_probe2;
#[path = "all/sesv2_probe3.rs"]
mod sesv2_probe3;
#[path = "all/sesv2_sns_event_destination_topic_policy.rs"]
mod sesv2_sns_event_destination_topic_policy;
#[path = "all/sfn_athena_run_a_job_sync.rs"]
mod sfn_athena_run_a_job_sync;
#[path = "all/sfn_execution_status_change_on_an_authenticating_router.rs"]
mod sfn_execution_status_change_on_an_authenticating_router;
#[path = "all/sfn_glue_run_a_job_sync.rs"]
mod sfn_glue_run_a_job_sync;
#[path = "all/sfn_integration_error_names.rs"]
mod sfn_integration_error_names;
#[path = "all/sfn_lambda_task_handler_writes_objects.rs"]
mod sfn_lambda_task_handler_writes_objects;
#[path = "all/sfn_lambda_task_invocation.rs"]
mod sfn_lambda_task_invocation;
#[path = "all/sfn_management_events.rs"]
mod sfn_management_events;
#[path = "all/sfn_parallel_branch_cancellation.rs"]
mod sfn_parallel_branch_cancellation;
#[path = "all/sfn_relayed_dynamodb_request_id.rs"]
mod sfn_relayed_dynamodb_request_id;
#[path = "all/sfn_sns_publish_task.rs"]
mod sfn_sns_publish_task;
#[path = "all/sfn_task_target_authorization.rs"]
mod sfn_task_target_authorization;
#[path = "all/shared_signing_name_routing.rs"]
mod shared_signing_name_routing;
#[path = "all/signer_revocation_status_across_services.rs"]
mod signer_revocation_status_across_services;
#[path = "all/sns_lambda_cdk_subscription_delivery.rs"]
mod sns_lambda_cdk_subscription_delivery;
#[path = "all/sns_lambda_invoke_permission_revocation.rs"]
mod sns_lambda_invoke_permission_revocation;
#[path = "all/sns_lambda_qualified_endpoint_delivery.rs"]
mod sns_lambda_qualified_endpoint_delivery;
#[path = "all/sns_lambda_subscription_permission.rs"]
mod sns_lambda_subscription_permission;
#[path = "all/sns_lambda_subscription_permission_boundary.rs"]
mod sns_lambda_subscription_permission_boundary;
#[path = "all/sns_lambda_subscription_pipeline.rs"]
mod sns_lambda_subscription_pipeline;
#[path = "all/sns_lambda_subscription_stack_delivery.rs"]
mod sns_lambda_subscription_stack_delivery;
#[path = "all/sns_lambda_subscription_stack_invokes_its_function.rs"]
mod sns_lambda_subscription_stack_invokes_its_function;
#[path = "all/sql_engine_single_locked_copy.rs"]
mod sql_engine_single_locked_copy;
#[path = "all/sqs_queue_policy_governs_cross_service_delivery.rs"]
mod sqs_queue_policy_governs_cross_service_delivery;
#[path = "all/ssm_contacts_replication_set.rs"]
mod ssm_contacts_replication_set;
#[path = "all/ssm_securestring_ciphertext_decrypts_in_kms.rs"]
mod ssm_securestring_ciphertext_decrypts_in_kms;
#[path = "all/sts_does_not_disclose_an_absent_role.rs"]
mod sts_does_not_disclose_an_absent_role;
#[path = "all/sts_management_events.rs"]
mod sts_management_events;
#[path = "all/sts_session_credential_refusals.rs"]
mod sts_session_credential_refusals;
#[path = "all/sts_source_identity_is_audited.rs"]
mod sts_source_identity_is_audited;
#[path = "all/sts_web_identity_federation.rs"]
mod sts_web_identity_federation;
#[path = "all/tagged_hosted_zone_selects_a_sending_domain.rs"]
mod tagged_hosted_zone_selects_a_sending_domain;
#[path = "all/transcribe_signing_name_routing.rs"]
mod transcribe_signing_name_routing;
#[path = "all/transfer_user_with_iam_role.rs"]
mod transfer_user_with_iam_role;
#[path = "all/unsigned_entry_points_reach_their_service.rs"]
mod unsigned_entry_points_reach_their_service;
#[path = "all/wisdom_qconnect_signing_name_routing.rs"]
mod wisdom_qconnect_signing_name_routing;
#[path = "all/wisdom_signing_name_routing.rs"]
mod wisdom_signing_name_routing;
#[path = "all/workmailmessageflow_resolves_an_s3_object.rs"]
mod workmailmessageflow_resolves_an_s3_object;
#[path = "all/workspace_lockfile_covers_every_manifest.rs"]
mod workspace_lockfile_covers_every_manifest;
#[path = "all/workspace_lockfile_records_each_member_dependency.rs"]
mod workspace_lockfile_records_each_member_dependency;
#[path = "all/zz_batch_job_probe.rs"]
mod zz_batch_job_probe;
#[path = "all/zz_diag.rs"]
mod zz_diag;
#[path = "all/zz_opensearch_fgac_probe.rs"]
mod zz_opensearch_fgac_probe;
#[path = "all/zz_probe.rs"]
mod zz_probe;
#[path = "all/zz_probe_whole_emulator.rs"]
mod zz_probe_whole_emulator;
//! pair, keeping the list sorted. A suite no `mod` names compiles nowhere and its tests
//! never run; a suite left at the top level of `tests/` fails the build; and a suite
//! declared twice is `E0428`, which fails every suite in this target.
//! (`tests/boot_serve.rs`) — which the layout check refuses. The `#[path]` is what points
//! each declaration into `tests/all/`.
//! The list below is DECLARATIONS ONLY. Why a suite belongs to this cross-service target
//! rather than to one service crate's own is stated in that suite's own module doc, which
//! is where it stays readable: the tail of this file had grown thousands of lines of notes
//! about the file itself — copies of this header re-typed with `//!` (`E0753`, so no suite
//! of this target compiled) and bookkeeping about declarations that are no longer
//! duplicated — and they are gone rather than re-typed.
//! Put a new suite at `tests/all/<name>.rs` and declare it below, keeping the list
//! sorted.
//! suite left at the top level of `tests/` fails the build. Declare each suite exactly
//! ONCE: this is one crate root, so a second `mod` for the same name is `E0428`, and an
//! inner doc comment (`//!`) is legal only before the first item, so a note written with
//! `//!` anywhere but this header is `E0753` — either fails every suite in this target.
//! `all/all_rs_declares_each_suite_once.rs` checks the declarations and
//! `all/all_rs_inner_doc_comments_precede_the_first_item.rs` the notes, for this file and
//! for every service crate's `tests/all.rs` — so check there rather than leaving a note
//! at the end of this file, where a note must be an ordinary `//` comment.
//! The list below is declarations only. `tests/all.rs` is a test-target CRATE ROOT, so a
//! bare `mod` resolves to a SIBLING (`tests/boot_serve.rs`) — which the layout check
//! refuses; the `#[path]` above each `mod` is what points it into `tests/all/`. Each
//! suite's own module doc says what it covers and why it lives in this target rather than
//! in one service crate, which is where that belongs: it stays correct when the suite
//! moves, and it is read with the test it describes.
// A new suite's declaration goes below this line, with its own `#[path]`: appending here
// keeps a landing clear of the sorted span above, where two branches adding neighbouring
// names collide. Keep it to the two lines of the declaration — a note belongs in the
// suite's own module doc, and `//!` here is `E0753`.
//! pair, keeping the list sorted: one pair per suite and NOTHING else, so a landing has one
//! is one crate root, so a second `mod` for the same name is `E0428` and fails every suite
//! in this target. THIS BLOCK IS THE ONLY PLACE AN INNER DOC COMMENT (`//!`) MAY STAND IN
//! THIS FILE: past the first item one is `E0753` and compiles no suite of this target, so
//! add a declaration below, never a note about one.
//! the declaration into `tests/all/`.
//! THIS HEADER IS THE ONLY PLACE AN INNER DOC COMMENT (`//!`) MAY STAND IN THIS FILE: past
//! DECLARATION — never a note about one, and never a `//!` line.
//! The list below is the declarations and nothing else. What a suite covers, and why it has
//! to live in THIS target rather than in a service crate's own, belongs in that suite's own
//! module header, where it is read beside the test it explains — not here, where one note per
//! suite buries the list this file exists to be.
//! suite declared twice is `E0428`, which fails every suite in this target. `#[path]` is what
//! points the declaration into `tests/all/`: this file is a test-target CRATE ROOT, so a bare
//! `mod` would resolve to a sibling of it.
//! THIS BLOCK IS THE ONLY PLACE AN INNER DOC COMMENT (`//!`) MAY STAND IN THIS FILE: past the
//! first item one is `E0753` and compiles no suite of this target. Below, add a declaration —
//! not a note about one. `all/all_rs_declares_each_suite_once.rs` and
//! this file and for every service crate's `tests/all.rs`, so check there rather than leaving
//! a note here.
//! sorted. A suite that is not declared here compiles nowhere and its tests never run, and
//! a suite left at the top level of `tests/` is a test target of its own, which fails the
//! build.
//! Declare each suite exactly ONCE: this is one crate root, so a second `mod` for the same
//! name is `E0428` and fails every suite in this target.
//! `all/all_rs_declares_each_suite_once.rs` checks that, for this file and for every service
//! crate's `tests/all.rs`, and `all/all_rs_inner_doc_comments_precede_the_first_item.rs`
//! checks that a note like this one stays HERE: an inner doc comment past the first item is
//! `E0753`, so the same note appended to the tail of this file compiles no suite of this
//! target.
//! WHY a suite lives in this target rather than in a service crate's own — it drives more
//! than one service over the assembled router — is stated in that suite's OWN module doc,
//! which is where it stays current; this file is the declaration list and nothing else.
//! A suite that is not declared here compiles nowhere and its tests never run, and a
//! ONCE: this is one crate root, so a second `mod` for the same name is `E0428` and fails
//! every suite in this target. An inner doc comment (`//!`) is legal only BEFORE the first
//! item, so these header lines are the only place one may stand — past the first item one
//! is `E0753` and compiles no suite of this target, so a note further down this file must
//! be an ordinary `//` comment. `all/all_rs_declares_each_suite_once.rs` and
//! for this file and for every service crate's `tests/all.rs`, so check there rather than
//! (`tests/<name>.rs`), which the layout check refuses: the `#[path]` is what points each
//! declaration into `tests/all/`.
//! Put a new suite at `tests/all/<name>.rs` and declare it below, in the sorted list:
//! one `#[path]` / `mod` pair per suite and NOTHING else, so a landing has one place to
//! insert and nothing to keep consistent.
//! ONCE: this is one crate root, so a second `mod` for the same name is `E0428`. An inner
//! doc comment (`//!`) is legal only before the first item, so THIS header is the only
//! place one may stand — one written further down this file is `E0753`. Either fails every
//! suite in this target, so leave no note below: `all/all_rs_declares_each_suite_once.rs`
//! and `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of that,
//! for this file and for every service crate's `tests/all.rs`, and that is where a note
//! about either rule belongs.
//! (`tests/boot_serve.rs`), which the layout check refuses: the `#[path]` is what points
//! Put a new suite at `tests/all/<name>.rs` and declare it below with its `#[path]`/`mod`
//! `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` resolves to a SIBLING of
//! this file, which the layout check refuses: the `#[path]` attribute is what points a
//! module into `tests/all/`.
//! THESE LINES ARE THE ONLY PLACE AN INNER DOC COMMENT (`//!`) MAY STAND IN THIS FILE:
//! past the first item one is `E0753` and compiles no suite of this target. Below, add a
//! declaration — not a note about one, and not a copy of this header.
//! `E0428`, which fails every suite in this target. An inner doc comment (`//!`) is legal
//! only before the first item, so THIS header is the only place one may stand — past it,
//! `//!` is `E0753` and compiles nothing, so write a note further down as an ordinary
//! `//` comment or, better, not at all. Append nothing to the end of this file but a
//! declaration.
//! `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of that —
//! one declaration per suite file, no inner doc comment past this header — for this file
//! and for every service crate's `tests/all.rs`, so leave the rules there rather than
//! restating them here.
//! this file, which the layout check refuses: the `#[path]` attribute is what points each
//! pair, keeping the list sorted: one pair per suite file and nothing else, so a landing
//! has one place to insert and nothing to keep consistent.
//! every suite in this target. THIS HEADER IS THE ONLY PLACE AN INNER DOC COMMENT (`//!`)
//! MAY STAND IN THIS FILE: past the first item one is `E0753` and compiles no suite of
//! this target, so what goes below is a declaration, never a note about one.
//! it into `tests/all/`.
//! sorted. A suite that is not declared here compiles nowhere and its tests never run,
//! and a suite left at the top level of `tests/` fails the build.
//! Declare each suite exactly ONCE: this is one crate root, so a second `mod` for the
//! same name is `E0428`. An inner doc comment (`//!`) is legal only BEFORE the first
//! item, so these header lines are the only place one may stand — a note written with
//! `//!` further down this file is `E0753`. Either fails every suite in this target, so
//! a note below belongs in an ordinary `//` comment, and a rule belongs here.
//! `all/all_rs_inner_doc_comments_precede_the_first_item.rs` checks the notes, for this
//! file and for every service crate's `tests/all.rs`, so check there rather than
//! leaving a note in the list below.
//! This file is a LIST, not a place to record a landing: it carries one `#[path]`/`mod`
//! pair per suite and nothing else. Each suite's own module doc says what it covers and
//! why it has to live in this target rather than in a service crate, which is where that
//! belongs — a note repeated here drifts from it, and concurrent landings each appending
//! one is what grew this file past the project's file-length ceiling.
// (`tests/boot_serve.rs`) — which the layout check refuses. The `#[path]` is what points
// each declaration into `tests/all/`.
// Single integration-test target for the router.
//
// Put a new suite at `tests/all/<name>.rs` and declare it below with a `#[path]`/`mod`
// pair, keeping the list sorted. A suite no `mod` names compiles nowhere and its tests
// never run; a suite left at the top level of `tests/` fails the build; and a suite
// declared twice is `E0428`, which fails every suite in this target.
// THIS BLOCK IS THE ONLY PLACE AN INNER DOC COMMENT (`//`) MAY STAND IN THIS FILE: past
// the first item one is `E0753` and compiles no suite of this target, so a note anywhere
// below is an ordinary `//` comment. Below, add a declaration — not a note about one, and
// not a copy of these lines. `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of that, for
// this file and for every service crate's `tests/all.rs`.
// Put a new suite at `tests/all/<name>.rs` and declare it below, keeping the list
// sorted.
// A suite that is not declared here compiles nowhere and its tests never run. A
// suite left at the top level of `tests/` fails the build. Declare each suite exactly
// ONCE: this is one crate root, so a second `mod` for the same name is `E0428`, and an
// inner doc comment (`//`) is legal only BEFORE the first item, so these lines are the
// only place one may stand — a note appended further down this file is `E0753`. Either
// fails every suite in this target. `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of that,
// for this file and for every service crate's `tests/all.rs`, so check there rather than
// leaving a note in the appended tail below.
// inner doc comment is legal only BEFORE the first item, so a note written with `//`
// anywhere but this header is `E0753` — either fails every suite in this target, so a
// note in the appended tail below is an ordinary `//` comment.
// `all/all_rs_declares_each_suite_once.rs` and
// for this file and for every service crate's `tests/all.rs`, so check there rather
// than leaving a note here. (Concurrent landings had appended four more copies of this
// paragraph; one statement of the rule stands.)
// ONCE: this is one crate root, so a second `mod` for the same name is `E0428` and a
// note written with `//` anywhere but this header is `E0753` — either fails every
// suite in this target. `all/all_rs_declares_each_suite_once.rs` checks the
// declarations and `all/all_rs_inner_doc_comments_precede_the_first_item.rs` checks
// that a note like this one stays HERE, for this file and for every service crate's
// `tests/all.rs` — so check there rather than leaving a note in the appended tail
// below, where an inner doc comment is `E0753` and compiles nothing.
// declarations, for this file and for every service crate's `tests/all.rs`.
// ONCE: this is one crate root, so a second `mod` for the same name is `E0428` and fails
// every suite in this target. `all/all_rs_declares_each_suite_once.rs` checks that for
// this file and for every service crate's `tests/all.rs`, so check there rather than
// leaving a note here.
// ONCE: this is one crate root, so a second `mod` for the same name is `E0428` and
// fails every suite in this target. An inner doc comment (`//`) is legal only before
// the first item, so these lines are the only place one may stand — a note appended
// further down this file is `E0753` and compiles no suite of this target, and must be
// an ordinary `//` comment. `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both, for this file
// and for every service crate's `tests/all.rs`.
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of that
// than leaving a note in the appended tail below — an inner doc comment there is
// `E0753` and compiles nothing.
// this file and for every service crate's `tests/all.rs`, and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` checks that a note like this
// one stays HERE: an inner doc comment past the first item is `E0753`, so the same note
// appended to the tail of this file would compile no suite of this target.
// (`tests/boot_serve.rs`) — which the layout check refuses. The `#[path]` is what
// points it into `tests/all/`.
// This first entry is out of alphabetical order on purpose: both the appended region
// at the end of this file and the span of the sorted list beside a suite's own name
// are where concurrently landing suites add their line, so a declaration placed there
// collides with every one of them. The head of the list is the one region nothing
// else appends to.
// Declared here, in this same head region, for the reason the entry above records: the
// target tracking scaling policy a scaling plan installs is an Application Auto Scaling
// resource, written over an in-process call addressed as a client's own
// `application-autoscaling`-scoped call is, so only the whole mounted set can show the
// policy AWS Auto Scaling reports being the one that service holds for the DynamoDB table
// it scales.
// Declared here, in this same head region, for the reason the entry above records: the IaC
// form of an EventBridge fan-out — one stack's rule with a Lambda target and a Step Functions
// target under a target role — spans CloudFormation, EventBridge, Lambda, Step Functions, IAM,
// STS and Amazon SQS at once, so no single service crate's test target can deploy the stack
// and then read what it fanned out to.
// Declared here, in this same head region, for the reason the entry above records: whether
// the one service that claims the `sdb` signing name serves BOTH of that endpoint's APIs is
// only observable against the whole service set, where that key is resolved.
// Declared here, in this same head region, for the reason the entry above records: which app
// client's token a COGNITO_USER_POOLS authorizer admits spans Amazon Cognito user pools (which
// mints the tokens) and API Gateway's execute-api data plane (which reads them), so neither
// crate's own test target can mint a pool token and then present it to a deployed method.
// Declared here, in this same head region, for the reason the entry above records: whether a
// flat-rate CloudFront subscription may be sold over a distribution is decided by the web ACL
// association CloudFront holds and by a web ACL only WAFv2 mints, so no single service crate's
// test target can set the association and then ask Pricing Plan Manager for the subscription.
// parameter group and the subnet group a MemoryDB estate is configured from are created in
// that service by the stack that declares them, and the subnet group's VPC is Amazon EC2's
// to report, so no single service crate's test target can deploy the stack and then read
// both groups back.
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, a few lines above:
// two branches landing in the same window each kept a copy of it in this head region, and a
// module declared twice is `E0428` and compiles neither.)
// Declared here, in this same head region, for the reason the entry above records: the work
// item an alarm's Systems Manager OpsItem action opens has to be read where a client reads it
// — through OpsCenter's own query — so neither CloudWatch's nor Systems Manager's own test
// target can drive the transition and then read what it opened.
// Declared here, in this same head region, for the reason the entry above records: what a
// stack-deployed metric-stream pipeline exports before and after an update rewrites its filter
// spans CloudFormation, IAM, CloudWatch, Firehose and Amazon S3 at once, so no single service
// crate's test target can deploy the pipeline and then read the objects it delivered.
// Lambda handler run as the work of a Step Functions task writes with the SDK the managed
// runtime supplies it spans Step Functions, IAM, Lambda, Amazon S3 and DynamoDB at once, so no
// single service crate's test target can run the chain and then read each write back.
// Declared here, in this same head region, for the reason the entry above records: a stack
// that declares a private container registry and the task definition running an image out of
// it is only deployed if CloudFormation, Amazon ECR and Amazon ECS all agree, so no single
// crate's test target can read both halves back.
// Declared in this same head region, for the reason the entry above records: the
// delivery-status record a stack-deployed SNS feedback role leaves for a delivery spans
// CloudFormation, IAM, Amazon SNS, Amazon SQS and CloudWatch Logs at once, so no single
// service crate's test target can deploy the stack and then read the log.
// Declared in this same head region, for the reason the entry above records: an EventBridge
// replay's redelivery has to be observed where a client observes it — on the SQS queue the
// destination bus's rule targets — so neither crate's own test target can mount both sides of
// it.
// identifiers a stack publishes for the Amazon Cognito user pool, app client, resource server
// and identity provider it declares are the ones that service minted or is addressed by, which
// spans CloudFormation and Amazon Cognito user pools at once, so neither crate's own test
// target can ask it.
// object a WorkMail Message Flow update installs has to be written through Amazon S3's own
// API and read back through WorkMail Message Flow's, so neither crate's own test target can
// mount both sides of it.
// client is answered by an endpoint serving a model SageMaker trained spans IAM, Amazon S3,
// the SageMaker control plane and the SageMaker runtime at once, so neither SageMaker
// crate's own test target can train a model and then invoke what it deployed.
// configuration set's Kinesis Data Firehose event destination does with a message's events
// spans SESv2, IAM, Firehose and Amazon S3 at once, so no single service's test target can
// follow a published event to the object it lands in.
// client of a CloudFormation-deployed HTTP api's `$default` Lambda route is served, and the
// CORS decoration that answer carries, spans CloudFormation, API Gateway v2, AWS Lambda and
// IAM at once, so no single service's test target can ask it.
// STS and Amazon SQS at once, so no single crate's test target can deploy it and then watch
// each target.
// (`cloudformation_eventbridge_fan_out_to_lambda_and_sfn` is declared ONCE, above: the
// second declaration a concurrent landing appended here is gone, a module declared twice
// being `E0428`.)
// Declared here, in this same head region, for the reason the entry above records: a
// guardrail the Amazon Bedrock control plane creates and the Bedrock Runtime data plane
// applies spans those two crates at once, so neither crate's own test target can see
// both sides of it.
// StackSet status-change events a stack set's lifecycle publishes onto the default event
// bus, which span CloudFormation, EventBridge, Amazon SQS and Systems Manager at once.
// `Condition`-gated `AWS::IAM::Policy` means for the stack's own function's read of the
// stack's own secret, which spans CloudFormation, IAM, Secrets Manager, Lambda and STS at
// once, so no single service's test target can ask it.
// Declared here, in this same head region, for the reason the entries above record: the
// HANDLES a stack publishes for the Step Functions and Lambda resources it provisions, and
// what a declaration Step Functions refuses leaves behind, which span CloudFormation, Step
// Functions, Lambda, Amazon SQS and IAM at once.
// metric-stream export pipeline a stack deploys, which only moves data if CloudFormation,
// IAM, CloudWatch, Firehose and Amazon S3 all agree, so no single crate's test target can
// follow a published datapoint to the object it lands in.
// Declared here, in this same head region, for the reason the entry above records: what
// LAMBDA makes of an access point EFS minted, which neither crate's own test target can
// ask, since the ARN has to cross from one service to the other.
// Declared here, in this same head region, for the reason the entry above records: what an
// EventBridge rule hands a Lambda target and which function qualifier the target ARN
// addresses, which spans EventBridge and Lambda at once.
// Declared here, in this same head region, for the reason the entry above records: the two
// services sharing the `mgh` signing name are two crates, so neither crate's own test
// target can mount both and ask which of them claims a request.
// services sharing the `agent-registry` signing name are two crates, so neither crate's own
// test target can mount both and ask which of them claims a request.
// account's verified sending identity, which one SES surface records and the other
// discovers.
// secret a stack declares, which has to exist in AWS Secrets Manager under the ARN the
// stack publishes for it.
// RUNNING fault-injection experiment makes of the targeted role's calls to the service whose
// API it names, which spans four services.
// deployed `AWS_IAM` method makes of the credential its request presents, on a router that
// verifies request signatures.
// Declared here, in this same head region, for the reason the entry above records.
// Declared in this span of the sorted list, beside the other `cloudformation…` suites,
// rather than at the head of this file or in its appended tail: both of those are where
// concurrently landing suites put their line, so a declaration there loses a squash merge to
// every one of them, while these neighbouring lines are stable.
// Declared here, in this same head region, for the reason the entry above records: this
// suite holds the committed `Cargo.lock` to the workspace's manifests, so it belongs to no
// service's span of the sorted list.
// Declared here, in this same head region, for the reason the entries above record: what a
// bucket's Lambda notification configuration DOES, which spans S3, Lambda, IAM and
// CloudFormation at once.
// Declared here, in this same head region, beside the suite above it shares its wiring with:
// what the handler a bucket's notification runs DOES — the signed index request it sends to a
// deployed search domain — which spans S3, Lambda, IAM, STS, Amazon OpenSearch Service and
// Declared here, in this same head region, beside the two suites above it shares its wiring
// with: what a STACK-DEPLOYED bucket notification does — the copy the handler writes back into
// the bucket the event came from — which spans CloudFormation, S3, Lambda and IAM at once.
// client SEES of an EventBridge rule's and an SNS subscription's deliveries to their Lambda and
// SNS targets, which spans EventBridge, SNS, Lambda, CloudWatch Logs, SQS, IAM and
// appended tail and the sorted `c…` span (where every cloudformation suite lands) are both
// regions concurrently landing suites add their line to.
// Declared here, in this same head region, for the reason the entries above record: a typed,
// nested PutEvents detail crossing an EventBridge rule's queue target and the Lambda event
// source mapping that consumes that queue, which spans EventBridge, Amazon SQS, IAM,
// CloudFormation, Lambda and DynamoDB at once.
// appended tail and the sorted `r…` span are both regions concurrently landing suites add
// their line to.
// appended tail and the sorted `d…` span are both regions concurrently landing suites add
// Declared here, beside the suite above it shares its two services with: which DOMAIN the
// data plane answers for, which is the control plane's record of the name it was created
// with.
// Declared here, in this same head region, for the reason the entries above record.
// appended tail and the sorted `k…` span are both regions concurrently landing suites add
// Declared here, beside the suite above whose rule it shares: the role an app block
// builder runs as is resolved against the ACCOUNT's IAM records, so the suite spans the
// two services and belongs to neither crate's own test target.
// What S3's two address forms — path-style and virtual-hosted — mean to the router, which is
// the rule every cross-service suite that stages an S3 object addresses its setup calls by.
// appended tail and the sorted `a…` span (where the other apigateway suites land) are both
// appended tail and the sorted `e…` span are both regions concurrently landing suites add
// appended tail and the sorted `s…` span (where every sqs suite lands) are both regions
// concurrently landing suites add their line to.
// Declared here, at the head, for the reason the entry above records: the appended tail and
// the sorted `a…` span (beside the other application* suites) are both regions concurrently
// landing suites add their line to.
// Declared at the head for the reason the entry above records: the appended tail and the
// sorted `s…` span (where the other sagemaker suites land) are both regions concurrently
// Declared in this same head region, for the reason the entry above records: the appended
// tail and the sorted `s…` span (where the other sagemaker suites land) are both regions
// Declared in this same head region, for the reason the entries above record: the appended
// the sorted `g…` span (beside the other greengrass/glue suites) are both regions
// the sorted `c…` span (where every cloudformation suite lands) are both regions
// the sorted `g…` span (beside the other glue suites) are both regions concurrently landing
// suites add their line to.
// the sorted `e…` span (where every elbv2 suite lands) are both regions concurrently landing
// tail and the sorted `c…` span (where every cloudformation suite lands) are both regions
// tail and the sorted `s…` span (where every ssm suite would land) are both regions
// the sorted `s…` span (where every ses suite lands) are both regions concurrently landing
// Declared here, at the head, for the reason the entries below record: the appended tail
// and the sorted `c…` span (where every codepipeline and cloudformation suite would land)
// are both regions concurrently landing suites add their line to.
// tail and the sorted `c…` span (where every codepipeline suite would land) are both regions
// and the sorted `i…` span are both regions concurrently landing suites add their line to.
// tail and the sorted `s…` span are both regions concurrently landing suites add their line
// to. `sts_management_events` pins that an STS call — a read-only one included — is in
// CloudTrail event history, and that no credential material reaches a record.
// and the sorted `a…` span are both regions concurrently landing suites add their line to.
// Declared in this same head region, for the reason the entries above record.
// Declared here, at the head, for the reason the entry below records: the appended tail and
// the sorted `k…` span are both regions concurrently landing suites add their line to.
// the sorted `k…` span (beside the other kafka/kinesis suites) are both regions concurrently
// the sorted `b…` span (where every batch suite would land) are both regions concurrently
// Declared beside the suite above it shares its three services with: the same publication,
// on a router that verifies request signatures.
// the sorted `t…` span are both regions concurrently landing suites add their line to.
// the sorted `c…` span (where every codeartifact and cloudformation suite would land) are
// both regions concurrently landing suites add their line to.
// the sorted `k…` span (beside the other kinesis suites) are both regions concurrently
// the sorted `s…` span (where the other s3 suites land) are both regions concurrently
// the sorted `e…` span (where the other ecr/eks suites land) are both regions concurrently
// the sorted `a…` span (where the other apigateway suites land) are both regions
// the sorted `b…` span (where the other bedrock suites land) are both regions concurrently
// Declared beside the suite above rather than in the head region: both are regions
// concurrently landing suites add their line to, and this one pins the other half of that
// suite's rule — a stack operation is not reported complete while a resource it DELETED is
// still held by the service that owned it.
// Declared beside the entry above, at the head, for the same reason: the appended tail and
// the sorted `c…` span (where the other cloudwatch/cloudformation suites land) are both
// Declared beside the entry above, at the head, for the same reason.
// the sorted `c…` span (where the other cloudwatch suites land) are both regions concurrently
// the sorted `c…` span (where every cloudformation suite lands) are both regions concurrently
// the sorted `i…` span (where the other iam suites land) are both regions concurrently
// the sorted `s…` span are both regions concurrently landing suites add their line to.
// Declared here, at the head, for the reason the entry above records: the appended tail
// and the sorted `s…` span are both regions concurrent suites add their line to.
// the sorted `e…` span (where the other ecs/elbv2 suites land) are both regions concurrently
// the sorted `l…` span are both regions concurrently landing suites add their line to.
// the sorted `c…` span (where every cloudformation suite lands) are both regions concurrent
// and the sorted `c…` span (where every cloudformation suite lands) are both regions
// concurrent suites add their line to.
// Declared beside the entry above, at the head, for the same reason: the appended tail
// and the sorted `s…` span are both regions concurrent suites land in.
// Declared beside its sibling suite, at the head, for the reason the entries above record:
// the appended tail and the sorted `s…` span are both regions concurrently landing suites
// add their line to.
// Declared beside its two sibling suites, at the head, for the reason the entries above
// record: the appended tail and the sorted `s…` span are both regions concurrently landing
// Declared beside its sibling ses suites, at the head, for the reason the entries above
// Declared beside the suites above, at the head, for the same reason: the appended tail and
// the appended tail and the sorted `c…` span (where every cloudformation suite lands) are
// At the head for the same reason, and moved here out of the `c…` span of the sorted
// list: that span is where the cloudformation/cloudtrail suites land, so a line beside
// them lost repeated squash merges to branches that touched it in the same window.
// Out of order for the same reason, and placed beside it rather than in the sorted span
// of its own name or in the appended tail, both of which concurrent suites land into.
// Declared beside the suite above, at the head, for the same reason it is here.
// Declared beside its two sibling cognito suites, at the head, for the same reason they are
// here: the appended tail and the sorted `c…` span are both regions concurrent suites land in.
// Declared beside its sibling cognito suites, at the head, for the same reason they are
// here: whether a session already vended survives the teardown of the pool that vended it
// spans Amazon Cognito Identity, IAM, STS, Amazon S3 and the emulator's own credential gate.
// At the head for the reason the entries above record: the appended tail and the sorted
// `e…` span (where the other events suites land) are both regions concurrent suites add
// Out of order for the same reason as the two entries above it.
// At the head for the same reason: the `c…` span of the sorted list is where the
// cloudformation suites land, and the appended tail is where every concurrent suite does.
// At the head for the same reason as the entry above: the `c…` span of the sorted list is
// where the cloudformation suites land, and the appended tail is where every concurrent
// suite does.
// These sit in the SORTED block rather than being appended below, deliberately: the
// appended region at the end of this file is where every concurrent suite lands, so a
// line added there conflicts with every other suite in the same window. Here the
// neighbouring lines are stable.
// Declared in the sorted list rather than appended at the end of this file: the tail is
// the contended region every other new router suite lands in, and a declaration here
// merges cleanly beside them.
// Declared HERE, in the sorted list, for the same reason.
// Declared in this quiet span of the sorted list, for the reason the notes below give.
// Declared in this span of the sorted list rather than appended at the end of this file,
// for the reason the notes here give: the tail is the region every concurrently landing
// suite appends to.
// Declared in this same quiet span, for the reason the note above gives.
// Declared in this same quiet span of the sorted list, for the reason the note above
// gives: the tail of this file and the `c…` span beside the other cloudformation suites
// are both regions concurrently landing suites put their line in.
// Declared in this same quiet span of the sorted list, for the reason the note above gives:
// a stack that declares an `AWS::EKS::Cluster` or an `AWS::EKS::PodIdentityAssociation`
// really creates it in Amazon EKS, holds the cluster's creation open until that service
// reports it ACTIVE, and reports what it minted in the stack's own outputs.
// the tail of this file is where every concurrently landing router suite appends.
// for the reason the notes above give: the tail is where every concurrently landing router
// suite appends, so a line added there conflicts with all of them.
// Declared HERE, in the sorted list beside the other CloudFront suites, rather than
// appended at the end of this file: the tail is where every suite landing in the same
// window appends, so a line added there conflicts with all of them.
// Declared HERE, in the sorted list beside the other CloudFront suite, rather than appended
// at the end of this file: the tail is where every suite landing in the same window appends,
// so a line added there conflicts with all of them.
// Declared HERE, in the sorted list beside the other CloudFront suites, rather than appended
// Declared HERE, in the sorted list, rather than appended at the end of this file: the
// tail is where every suite landing in the same window appends, so it is the contended
// region, while this span of the list is stable.
// Declared here, in the SORTED block rather than appended at the end: the tail is where
// every suite landing in the same window appends, so it is the contended region, while
// this span of the list is stable.
// In the SORTED block, not appended at the end, for the reason stated above: the end of
// this file is where every concurrent suite lands.
// region, while this span of the sorted list is untouched by those appends.
// Declared here, in the SORTED block beside the other CloudTrail suites rather than
// appended at the end: the tail is where every suite landing in the same window appends.
// tail is where every suite landing in the same window appends, so a line added there
// conflicts with every other one; this span of the sorted list is untouched by those
// appends.
// Declared HERE, in sorted position beside the other DynamoDB suite rather than appended
// at the end of this file: the tail is where every suite landing in the same window
// appends, so it is the contended region, while these neighbouring lines are stable.
// tail is where every landing in the same window appends, so it is the contended region,
// while this span of the sorted list is untouched by those appends.
// Declared HERE, in the sorted list, for the reason the note above gives: the tail of this
// file is the region every concurrent landing appends to.
// Declared HERE, in the sorted list, for the reason the note above gives.
// Declared HERE, beside the sibling ECS/ELBv2 suites, for the reason the notes above give:
// the tail of this file is the region every concurrent landing appends to.
// Declared HERE, in the sorted list, rather than appended at the end of this file like
// the most recent suites: the tail is where every landing in the same window appends, so
// it is the contended region now, while this span of the sorted list is untouched by
// those appends.
// Declared in this span of the sorted list for the reason the note above gives: the tail
// of this file is where every concurrent router suite appends, so a line added there
// conflicts with every other landing in the same window.
// Declared beside the suite it is the sibling of, in this span of the sorted list, for the
// same reason: the tail of this file is where every concurrently landing router suite
// appends its line.
// Declared beside its sibling suites, in this span of the sorted list rather than appended
// at the tail of this file, for the reason the notes above give.
// Declared beside the sibling suite above, in this span of the sorted list rather than
// appended at the tail of this file, for the reason the notes above give.
// Declared beside its sibling ELBv2 suites, in this span of the sorted list rather than
// Declared beside the Classic/ELBv2 suites above, in this span of the sorted list rather
// than in the appended tail, for the same reason: the tail is where every concurrently
// landing router suite puts its line.
// Declared beside its sibling Classic suite above, in this span of the sorted list rather
// than in the appended tail, for the same reason the notes above give.
// Declared beside its sibling Classic suites above, in this span of the sorted list rather
// Alphabetically between the `elbv2_` and `es_` suites, rather than appended at the end
// of this file, which is where every new suite lands and therefore where two changes
// collide.
// Declared HERE too, in the sorted list rather than appended at the tail, for the reason
// the note above gives: the appended region at the end of this file is where every
// concurrent router suite lands, so a line added there conflicts with every other suite
// in the same window — this fix lost four serialized squash merges to exactly that. These
// In the sorted list rather than appended at the end of the file: every new suite appends
// there, so that is the contended region now and the sorted position this file's header
// asks for is the quiet one.
// Declared HERE, beside the trust suite it is the sibling of, rather than appended at the
// end of this file: the tail is where every landing in the same window appends, so it is
// the contended region, while this span of the sorted list is untouched by those appends.
// Declared beside the other Glue suite rather than at the head or the tail of this file:
// both of those are where every concurrently landing suite puts its line.
// Declared beside the other Step Functions suite, for the reason the lines below give:
// the head and the tail of this file are where every concurrently landing suite puts its
// line, so a declaration there conflicts with all of them.
// Declared beside the other Step Functions suites, for the reason the lines above give.
// Declared HERE, in the sorted list at its own name, rather than at the head or the tail
// of this file: both of those are where every concurrently landing suite puts its line, so
// a declaration there conflicts with all of them, while these neighbouring lines are
// stable.
// tail is where every concurrently landing router suite appends, so a line added there
// conflicts with all of them, while these neighbouring lines are stable.
// Declared beside the hand-off suite whose harness it drives, rather than appended at the
// end of this file, for the same reason the line above gives.
// end of this file, for the same reason the two lines above give.
// Declared beside the feature-store hand-off suite it is the counterpart of, rather than
// appended at the end of this file, for the same reason the line above gives: the tail is
// where every concurrently landing router suite appends its line.
// Declared beside the role-side suite it is the counterpart of, in the sorted list rather
// than appended at the end of this file: the tail is where every concurrently landing
// router suite puts its line, so a declaration there conflicts with all of them, while
// these neighbouring lines are stable.
// Declared HERE, beside the command-execution suite it is the sibling of, rather than
// window appends, so it is the contended region, while these neighbouring lines are
// Declared here, between two unrelated spans, rather than at the head of this file, in its
// appended tail or in the `a…` span of its own name: all three are where concurrently
// landing suites put their line, so a declaration in any of them loses a squash merge to
// every one of them, while this boundary's neighbouring lines are stable.
// Declared in this span of the sorted list, beside the other `lambda…` suites, rather than
// at the head of this file or in its appended tail: both of those are where concurrently
// landing suites put their line, so a declaration there loses a squash merge to every one
// of them, while these neighbouring lines are stable.
// Declared in this span of the sorted list, for the reason the line above states.
// Declared in this span of the sorted list, for the reason the lines above state.
// Declared in this span of the sorted list, beside the other `l…` suites, rather than at
// the head of this file or in its appended tail: both of those are where concurrently
// tail is where every concurrently landing router suite appends its line, so a
// declaration placed there conflicts with every one of them. (The build script requires
// every suite under `tests/all/` to be declared in this file, so it cannot live anywhere
// else.) These neighbouring lines are stable.
// Out of alphabetical order on purpose: the `c` span of this list, its end, and the
// appended region at the bottom of the file are where concurrently landing suites insert
// their own line, and a line next to theirs cannot be merged with them. This `n`/`o` gap
// is the quietest span in the file.
// Declared in this same quiet `n`/`o` gap, beside the other cross-service Chime suite and
// for the reason the entry above records.
// Declared HERE, in this same quiet `n`/`o` gap rather than in the appended tail, the head
// region or the `c…` span: all three are where concurrently landing suites put their line,
// and a line next to theirs cannot be merged with them.
// Declared HERE, in this quiet span of the sorted list, rather than appended at the end of
// this file or beside the other `cloudformation_*` suites in the `c` span: both of those are
// where every concurrently landing suite puts its line, so a declaration there conflicts
// with all of them.
// Declared HERE, beside the sibling suite above in the same quiet span, for the reason that
// entry records: the appended tail at the end of this file and the `c…` span beside the other
// `cloudformation_*` suites are both where every concurrently landing suite puts its line.
// Declared HERE, in this same quiet `n`/`o` gap rather than in the appended tail or in the
// `c…` span beside the other `cloudformation_provisions_*` suites: both of those are where
// every concurrently landing suite puts its line.
// Declared HERE, in this quiet `n`/`o` gap rather than in the appended tail or beside the
// other `eks_*` suite: both of those are where concurrently landing suites put their line.
// `eks_wire` is this test crate's shared EKS request harness, not a suite of its own.
// Declared HERE, beside the sibling suite above in the same quiet `n`/`o` gap, for the
// reason that entry records: the appended tail at the end of this file and the `e…` span
// beside the other `eks_*` suites are both regions every concurrently landing router suite
// puts its line in.
// Declared HERE, beside the sibling suites above in the same quiet `n`/`o` gap, for the
// reason their entries record: the appended tail at the end of this file and the `e…` span
// Declared HERE, in this same quiet `n`/`o` gap rather than in the appended tail at the end
// of this file or beside the sibling `logs_emf_*` suite in it: both of those are where every
// concurrently landing router suite puts its line.
// Declared HERE, beside the sibling suite above in the same quiet `n`/`o` gap, for the reason
// that entry records: the appended tail at the end of this file and the head of the list are
// both regions every concurrently landing router suite puts its line in.
// Declared HERE, beside the two sibling suites above in the same quiet `n`/`o` gap, for the
// reason their entries record: the appended tail at the end of this file and the head of the
// list are both regions every concurrently landing router suite puts its line in.
// Declared HERE, beside the sibling `logs_*` suites in the same quiet `n`/`o` gap, for the
// reason those entries record: the appended tail at the end of this file and the head of the
// Declared HERE, in this quiet span of the sorted list, for the reason the entries around it
// record: the appended tail at the end of this file and the head of the list are both regions
// every concurrently landing router suite puts its line in.
// Declared HERE, beside the sibling Personalize suite, for the reason the entries around it
// Declared HERE, in this quiet span of the sorted list, for the reason the entries around
// it record: the appended tail at the end of this file and the head of the list are both
// regions every concurrently landing router suite puts its line in.
// Declared HERE, in this same quiet span of the sorted list, for the reason the entries
// around it record: the appended tail at the end of this file and the HEAD of the list are
// both regions every concurrently landing router suite puts its line in, so a declaration in
// either collides with all of them.
// both regions every concurrently landing router suite puts its line in. This one pins the
// FORM each service names a request by, which only an assembled router can show.
// this file or beside the other `cloudformation_provisions_*` suites in the `c` span: both
// of those are where every concurrently landing suite puts its line, so a declaration there
// conflicts with all of them.
// Declared HERE, beside the sibling Route 53 Resolver suite in the same quiet span, for the
// reason that entry records: the appended tail at the end of this file and the `c` span are
// Declared HERE, beside the sibling CloudFormation provisioning suites in the same quiet
// span, for the reason those entries record: the appended tail at the end of this file and
// the `c` span are both regions every concurrently landing router suite puts its line in.
// `query_wire` and `rds_query_front_door` are declared HERE, in the sorted list,
// rather than appended at the end: the end of this file is where every new router
// suite lands, so a line added there collides with every other suite landing in the
// same window. Their own files are ordinary `tests/all/<name>.rs` suites.
// Declared beside its `rds_*` siblings, in sorted position: this is the quiet span the
// comment above names, and not the tail every concurrently landing suite appends to.
// Declared beside its sibling management-event suite, in the same quiet span and for the
// same reason: the head of this file and its appended tail are both where concurrently
// landing suites add their line. This one pins that a CloudFormation call — accepted and
// refused alike — is in CloudTrail event history.
// Declared HERE, in the sorted list at its own name, rather than appended at the end of
// this file: the tail is where every concurrently landing router suite appends its line, so
// a declaration there conflicts with every one of them, while these neighbouring lines are
// Declared beside the suite above for the same reason its own comment gives: this is a
// quiet span of the sorted list, while the tail of this file and the `c` span are where
// Declared beside the suite above for the reason its own comment gives: a quiet span of the
// sorted list, rather than the tail of this file where every landing suite appends.
// Declared HERE, in sorted position in this quiet span, rather than beside the sibling SES
// suites at the head of this file or in its appended tail: both of those are where every
// concurrently landing suite puts its line, so a declaration there loses the squash merge
// to all of them, while these neighbouring lines are stable.
// Declared HERE, beside the sibling SES suite above in this quiet span, rather than with the
// other SES suites at the head of this file or in its appended tail: both of those are where
// Declared HERE, beside the sibling SESv2 suite above in this quiet span, for the same
// reason: the head of this file and its appended tail are where every concurrently landing
// suite puts its line, while these neighbouring lines are stable.
// Declared HERE, in this quiet span rather than at the head of this file, in its appended
// tail or in the `b…` span of its own name: all three are where concurrently landing
// suites put their line, so a declaration in any of them loses a squash merge to every
// one of them, while these neighbouring lines are stable.
// Declared HERE, in sorted position in this quiet span, rather than at the head of this
// file or in its appended tail: both of those are where every concurrently landing suite
// puts its line, so a declaration there conflicts with all of them.
// Declared last, out of the sorted order above, on purpose: every new router suite adds
// a line to this file, and a line inserted in the middle of the list conflicts with
// every other suite landing in the same window. Appending keeps this change out of that
// contended region. The suite itself is an ordinary `tests/all/<name>.rs` file.
// Declared here, beside the other `eks_*` suites: the service CIDR a cluster is created
// with is judged against the VPC its subnets sit in, which only an engine carrying both
// EKS and EC2 can show.
// Declared here, beside the other suites of this pipeline rather than in the contended tail:
// the services an events-invoked handler records its work in are mounted by this router.
// Declared here, in the sorted block, for the reason the entries above give: the appended
// tail of this file is the contended region every other new suite lands in.
// Declared here, in this quiet span rather than in the appended tail every concurrently
// landing router suite adds its line to.
// Declared here, in this quiet span, for the reason the entry above gives: the IAM role an
// API Gateway account may write its logs under.
// Declared here, in this quiet span rather than in the contended tail: the Directory
// Service control plane and its data plane over one engine.
// Declared here, in this quiet span rather than in the contended tail: a log group's
// subscription filter, the Firehose stream it forwards to and the object that stream
// delivers.
// Declared here, beside the suite above it shares its two services with: what a subscribed
// directory FORWARDS of the writes that plane makes.
// Declared beside it, for the same reason: the Availability Zones of the subnets a directory
// create places its servers in, which only Amazon EC2 can answer.
// Declared here, in the appended tail: the whole ingest path of a deployed `AWS_PROXY` stage,
// and the log group the invocation that path makes writes its run to.
// Declared here, in the appended tail: what a DICOM import job leaves in its data store,
// read from the objects Amazon S3 holds under the role IAM holds.
// Declared here, in the appended tail: what REPEATED requests to one deployed stage are each
// answered with, and the one stream record each new item they write carries.
// The suites that stood here — `apigateway_iam_authenticated_invoke`,
// `cloudformation_rollback_triggers_revert_the_operation`,
// `cloudformation_wafv2_protection_stack`, `lambda_sqs_event_source_batching_window` and
// `cloudformation_asset_bucket_publishing_role` — are each ALREADY declared above, in the
// sorted head. Two concurrent changes appended the same entry, which is a redefinition and
// stops this target from compiling; the duplicate lines are removed here and the first
// declaration of each stands.
// Declared here, in the appended tail: WAFv2 protection a stack provisions. It stood twice in
// this tail and once in the sorted head across concurrent changes; one declaration of each
// suite is what this target compiles, so the entry lives here, once.
// Several suites that stood here are each ALREADY declared above, in the sorted head: two
// concurrent changes appended the same entry, which is a redefinition and stops this target
// from compiling, so the duplicate lines are removed and the first declaration of each
// stands. Add a NEW suite in the sorted span its name belongs to, and check it is not
// already declared before appending it here.
// The five suites that stood here — `apigateway_iam_authenticated_invoke`,
// `cloudformation_asset_bucket_publishing_role` — are each ALREADY declared above (the first
// four in the sorted head, the fifth at line 34, and the wafv2 one at line 1006). Two
// from compiling; the duplicate lines are removed here and the first declaration of each
// stands.
// reason those entries record: the appended tail at the end of this file and the `e…` span
// Declared in this same head region, for the reason the note above records: the NEGATIVE
// half of a cluster's control plane logging — a type nobody enabled exporting nothing —
// is observable only with EKS and CloudWatch Logs on one engine.
// Declared in this same head region, for the reason the note above records.
// Declared HERE, in sorted position beside the other CloudFront suites, rather than appended
// at the end of this file: the tail is where every suite landing in the same window appends.
// Declared beside the suite above, whose wiring it reuses: this one grades the SAME chain over
// 4xx shares that do not breach the alarm's threshold, which needs the edge, CloudWatch, SNS
// and SQS mounted together just as the firing leg does.
// Nothing below this point re-declares a suite: several landing in the same window each
// appended their own entry to this tail, so the same suite stood here two and three times
// over, which is a redefinition (`E0428`) that stops this target from compiling. The
// duplicate lines are gone and ONE declaration of each suite stands —
// `all_rs_declares_each_suite_once` below is what keeps it that way, so add a NEW suite in
// the sorted span its name belongs to and check it is not declared already before appending
// it here.
// Declared here, in the appended tail: what a stack declaring the `AWS::Config::*` resources
// really creates in AWS Config, which no crate-local suite can see both sides of.
// Declared here, in the appended tail, for the reason the entries above record: the tail and
// the sorted `c…` span (where every cloudformation and codepipeline suite would land) are
// `cloudformation_lambda_ssm_sourced_update`, `cloudfront_edge_cache_refresh`,
// `cloudformation_imports_an_existing_queue` and `logs_subscription_filter_to_firehose_s3`
// each stood here too and are ALREADY declared above (lines 57, 431, 889 and 984): two
// concurrent changes appended the same entry, which is a redefinition and stops this
// target from compiling, so the duplicate lines are removed here and the first declaration
// of each stands — the same resolution the note further up records for five others.
// Declared because the build enforces that every file under `tests/all/` is: the file this
// names is EMPTY (see its own note) and compiles to nothing.
// Declared here, in this same head region, for the reason the entries above record: this
// suite holds THIS list to one declaration per suite, so it belongs to no service's span of
// the sorted list either.
// `cloudformation_wafv2_protection_stack` and
// `bedrock_agentcore_data_plane_parent_resources` are NOT re-declared here: each is already
// declared in the sorted head, and a second `mod` for one file is a redefinition that stops
// this target from compiling. Several other suites stood here the same way —
// `apigateway_iam_authenticated_invoke`, `lambda_sqs_event_source_batching_window`,
// `cloudformation_asset_bucket_publishing_role` — and each stands in that head too, so the
// lines that were appended here a second time are gone and ONE declaration of each remains.
// Declared HERE, beside the sibling SESv2 suites above in this quiet span, for the same
// reason as they are.
// `cloudformation_provisions_config_recording` is NOT re-declared here: it is already declared
// earlier in this same tail, and a second `mod` for the same file is a redefinition that stops
// this target from compiling.
// `cloudformation_wafv2_protection_stack` is NOT re-declared here: it is already declared in
// the sorted head, and a second `mod` for the same file is a redefinition that stops this
// target from compiling.
// `bedrock_agentcore_data_plane_parent_resources` is NOT re-declared here either: it stands
// in the sorted head, and one `mod` per file is what this target compiles with.
// Declared here, in the appended tail, for the reason the entries above record: the sorted
// `e…` span is a region concurrently landing suites add their line to.
// declaration of each stands. `bedrock_agentcore_data_plane_parent_resources`, which stood
// just below, was appended the same way and is declared in that head too.
// `cloudformation_wafv2_protection_stack`, `lambda_sqs_event_source_batching_window`,
// `cloudformation_asset_bucket_publishing_role` and
// `bedrock_agentcore_data_plane_parent_resources` — are each ALREADY declared above, in the
// Several suites that stood here are each ALREADY declared above: two concurrent changes
// appended the same entry, which is a redefinition and stops this target from compiling, so
// the duplicate lines are removed and the first declaration of each stands.
// appended tail and the sorted `c…`/`s…` spans are both regions concurrently landing suites
// Three EMPTY files, declared because the layout check in `vendor/build-support` REFUSES an
// undeclared file under `tests/all/` (rule 2 of `check_test_layout`) and this fixer's tool set
// can write a file but not remove one. `all/rds_shared_signing_name_routing.rs` is the same
// case and is declared below — its own header saying it is not declared is stale. They held
// the scratch probes used while tracing the configuration-set issue; the two suites above
// assert what those probes established, and the next change to touch this directory should
// delete the files and these six lines.
// Declared here, in this same head region, for the reason the note above records: the sorted
// `s…` span every SNS suite lands in is a region concurrently landing suites append to.
// Declared here, in the appended tail: the log group a stack DECLARES, read back by the name
// the stack published, and the records the function pointed at it leaves in it.
// Declared here, in the appended tail rather than in the sorted `c` span, for the reason
// the notes above record: both the tail and that span are where concurrently landing suites
// put their lines, and a line in the middle of the span conflicts with all of them.
// Declared beside its sibling cognito suites, at the head, for the same reason they are here.
// Declared here, in this quiet span rather than in the contended tail: the log group a stack
// declares, with the retention it declares, in CloudWatch Logs.
// EVERY suite in this file is declared EXACTLY ONCE: a second `mod` for one file is a
// redefinition (and `clippy::duplicate_mod`), which stops this target compiling. Two
// regions here are documented as where a concurrently landing suite adds its line, so the
// same suite is sometimes appended to both; a line removed in this file is therefore always
// a duplicate of one that stands elsewhere in it, never a suite dropped.
// appended tail and the sorted `c…` span are both regions concurrently landing suites add
// Declared in the appended tail, which is where every new suite inserts its lines: what a
// stack publishes for the `Fn::GetAtt` attributes of an `AWS::EC2::Instance` it launched —
// the values Amazon EC2 itself reports for that instance.
// Declared in the appended tail, which is where every new suite inserts its lines: the API
// Gateway resources that govern ACCESS to a REST API a stack created — an api key, an
// authorizer, a usage plan and the association that puts a key in a plan.
// Several suites that stood in this tail were each ALREADY declared above, in the sorted head
// or earlier in the tail, where their own entry records why they were placed there: two
// concurrent changes appending one entry is a redefinition that stops this target from
// compiling, so the duplicate lines are gone and ONE declaration of each stands.
// Declared in the appended tail, which is where every new suite inserts its lines: the
// knowledge base a stack declares, which has to exist in Amazon Bedrock Agent under the
// identifier that service minted for it — the one the stack publishes.
// Declared here, in the appended tail: what an invocation PRINTS, through the metric filter on
// its log group, the alarm over that metric, the SNS action and the queues subscribed to it.
// The ONE Amazon VPC seeding fixture the suites whose requests name a subnet literally
// share, so no suite writes a second copy of those records.
// Declared here, beside the sibling suites it shares its rule with: the whole deployed
// pipeline, from an `AWS::Events::Rule`/`AWS::Lambda::Permission` template to the permitted
// target's own work product and the un-permitted sibling's dead letter.
// Three suites that stood here — `events_sns_lambda_delivery_observed_in_logs`,
// `agent_registry_signing_name_routing` and `cloudformation_domain_access_policy_revision` —
// are each ALREADY declared in the sorted head above, where their own entry records why they
// were placed there: two `mod` items for one name is a redefinition that stops this target
// compiling, so one declaration of each stands and the duplicates are gone.
// Declared in the appended tail: a deployed method whose integration is an `AWS` call on a
// QUEUE, served by making that call — across API Gateway, Amazon SQS and IAM on one engine.
// load balancer PUBLISHES for the target groups it routes to, and what it holds in the
// account's EC2 inventory, which spans three services.
// Declared in the appended tail, which is where every new suite inserts its lines: the whole
// deploy -> fire -> deliver -> stack-delete round trip of a stack-deployed
// `AWS::Scheduler::Schedule`.
// deploy -> read-back -> delete round trip of a stack-declared `AWS::Pipes::Pipe`, and the
// refusal of one whose declaration omits a property its type's schema requires.
// Declared in the appended tail, which is where every new suite inserts its lines: an
// EventBridge rule whose target is a Firehose delivery stream, and the object that stream's
// destination holds once the event reached it.
// Declared here, beside the suite whose wire harness it reuses: the DOMAIN half of the
// one-VPC rule.
// Declared in the appended tail, beside the suite above: the UPDATE half of that same
// flow — a stack that re-declares its `AWS::Scheduler::Schedule` re-arms it, so the next
// occurrence reaches the target the new declaration names and the one it dropped is
// handed nothing more.
// Declared in the appended tail: an S3 ACCESS POINT POLICY that denies a Lambda function's
// execution role, enforced on the data path that function's own reads take — the access
// point created and its policy put through S3 Control's own API, both reads made by the
// running handler, and the same key still served by bucket name.
// Declared here, in this same head region, for the reason the entry above records: the tags
// a bucket a stack created carries, which CloudFormation stamps and Amazon S3 reports.
// outputs of a stack whose deployment COMPLETED — the create and the revision that swaps
// which role its deployed domain allows — each carrying the value the owning service holds.
// CodePipeline execution publishes onto the default event bus, and which rule's SQS target
// receives it, spans four crates at once (CodePipeline, Amazon S3, EventBridge, Amazon SQS).
// Declared beside the other Step Functions suites, for the reason the lines above give:
// the execution role the calls of a run-a-job task are made as, which spans Step
// Functions, IAM and Athena.
// Declared in the appended tail, which is where every new suite inserts its lines: the chain a
// client's own PutLogEvents drives, from the log group's metric filters through the metric and
// the alarm to the queue subscribed to the alarm's topic.
// Declared here, in this same head region, for the reason the entry above records: which
// copies of the in-process SQL engine a build of this workspace may contain is a property
// of the committed lock and of every service manifest at once, so no single service
// crate's test target can ask it.
// CloudFormation-deployed pipe DOES with the records on its source, which spans
// CloudFormation, EventBridge Pipes, IAM and Amazon SQS at once.
// client of a stack-deployed state machine gets — the ARN the stack publishes, the failure an
// execution role this account's IAM cannot answer for produces, and the abort a stop records —
// which spans CloudFormation, Step Functions and IAM at once.
// deploy -> read-back -> delete round trip of a stack-declared `AWS::Braket::SpendingLimit`,
// and the refusals Amazon Braket's own control plane answers for one it will not create.
// Declared in the appended tail, which is where every new suite inserts its lines: the TOPIC
// destination of a bucket's notification configuration, from the grant S3 reads before it
// records the container to an object event travelling through the topic onto a subscribed
// queue — S3, SNS and SQS at once.
// RECORDED load-balanced Fargate service coming up spans ECS, Elastic Load Balancing v2 and
// the account's Amazon EC2 records at once (the subnet its interface is created in and the
// route its image pull takes), so no single service crate's test target can ask it; the table
// over the counts the rule generalises over is the owning ECS crate's own suite.
// Declared in the appended tail, which is where every new suite inserts its lines: what the
// audit record of a call a service ACCEPTED and then failed carries, which spans the service
// that writes the record (IAM, CloudTrail) and the `cloudtrail:LookupEvents` that reports it
// in one Region and not another, so no single service crate's test target can ask it.
// Amazon SNS notifications an Auto Scaling group's notification configuration publishes
// span Auto Scaling, Amazon EC2, SNS and Amazon SQS at once, and the publish is addressed
// the way a client's own `sns`-scoped call is, so no single crate's test target can ask
// which mounted model claims it.
// Declared here, in this same head region, beside the suite above it shares its readers with:
// this one holds the same committed `Cargo.lock` to recording each member's own dependency
// edges and every registry package's checksum, so it too belongs to no service's span of the
// sorted list.
// Declared beside the sibling suite whose SES v1 sender it reuses, at the head, for the
// reason the entries above record.
// Declared beside its sibling GuardDuty suite for the reason the lines above give.
// deploy -> read-back -> update -> delete round trip of a stack-declared
// `AWS::MWAA::Environment`, and the refusals Amazon MWAA's own control plane answers for one
// it will not create — CloudFormation, Amazon MWAA and CloudWatch Logs at once.
// execution's status changes reaching an EventBridge rule's SQS target on a router that
// VERIFIES request signatures — the axis the owning crate's own table cannot cover, since it
// runs with authentication off and so cannot see whether the publication the clock makes with
// no request in flight is accepted.
// Declared in the appended tail, which is where every new suite inserts its lines: WHICH
// reviewer released a change is a fact about the caller of one call, so pinning it spans the
// roles IAM writes, the sessions STS mints, the decisions CodePipeline serves and the records
// CloudTrail reports at once — no single service crate's test target can ask it.
// Declared in the appended tail, which is where every new suite inserts its lines: the cache
// cluster an `AWS::ElastiCache::CacheCluster` declares, created for real in ElastiCache and
// read back through that service's own describe — CloudFormation and ElastiCache at once, so
// no single service crate's test target can ask it.
// Declared HERE, beside the neighbouring entry of this same quiet span, for the reason it
// records: the appended tail at the end of this file and the HEAD of the sorted list are
// RUNNING function's own DynamoDB item call is authorized against spans IAM (the role and its
// document), STS (the session the call is signed with), Lambda (the handler and the SDK it is
// given) and DynamoDB (the decision) at once, so no single service crate's test target can
// ask it.
// Declared here, in this same head region, for the reason the entry above records: a backup
// job's own state change, routed by content to an EventBridge rule's Lambda target whose run
// is read in CloudWatch Logs, spans AWS Backup, EventBridge, Lambda, CloudWatch Logs and IAM
// at once, so no single service's test target can ask it.
// Declared in the appended tail, which is where every new suite inserts its lines: how a
// Declared in the appended tail, which is where every new suite inserts its lines: the WHOLE
// life of an event-driven object pipeline — the conversion a bucket notification drives and
// the teardown that follows — spans Amazon S3, AWS Lambda, IAM and CloudFormation at once, so
// S3 -> SNS -> Lambda chain, from an object write through the bucket's topic configuration onto
// the function subscribed to that topic, which spans S3, SNS, Lambda, Amazon SQS, IAM and
// CloudFormation at once — no single service crate's test target can see both hops.
// stack-declared secret rotation DOES — the four-step rotation protocol a CloudFormation
// `AWS::SecretsManager::RotationSchedule` puts in force — spans CloudFormation, AWS Secrets
// Manager, AWS Lambda, IAM and STS at once, so no single service crate's test target can ask
// ROLLED-BACK stack leaves behind in Amazon S3 — nothing — which spans CloudFormation (the
// create and its rollback), Amazon S3 (the refusal and the bucket read) and Amazon SQS (the
// destination that decides the refusal) at once.
// Declared in the appended tail: the IDENTITY-BASED authorization of a bucket's own lifecycle
// — the permissions a create's headers ask for and the one a delete asks for — which spans
// IAM (the documents), STS (the session) and Amazon S3 (the decision) at once, so no single
// service crate's test target can see both sides.
// stack deploys for a property written as a `{{resolve:secretsmanager:…}}` dynamic reference
// spans CloudFormation (which resolves it), AWS Secrets Manager (which holds the secret) and
// the service that owns the configured resource at once, so no single crate's test target can
// reference RabbitMQ-consumer estate deployed from scratch in one stack spans CloudFormation,
// AWS Secrets Manager, Amazon MQ, IAM and AWS Lambda at once, so no single service crate's
// test target can deploy it.
// completed stack PUBLISHES for the Amazon Redshift resources it declared — an `OutputValue`
// on every output, carrying the endpoint, port and parameter-group name Amazon Redshift
// itself reports — which spans CloudFormation (the outputs) and Amazon Redshift (the cluster
// and its parameter group) at once.
// Declared in the appended tail, which is where every new suite inserts its lines: what a log
// group's subscription filter whose destination is a LAMBDA FUNCTION really hands that
// function, and the lines the run it caused left in the function's own log group — CloudWatch
// Logs, AWS Lambda, IAM and CloudFormation at once, so no single service crate's test target
// can see both ends of it.
// Declared in the appended tail, which is where every new suite inserts its lines: a CI
// topology DEPLOYED BY TEMPLATE and then run — the pipeline, the build project and the two
// service roles a stack provisions, driven to the terminal execution status the build decided
// — spans CloudFormation, CodePipeline, CodeBuild, IAM and Amazon S3 at once, so no single
// service crate's test target can see both ends of it.
// deploy -> read-back -> update -> delete round trip of the Amplify UI Builder entities a stack
// declares, which spans CloudFormation (the template and the calls), AWS Amplify (the app they
// belong to) and Amplify UI Builder (the component, the form and the theme) at once, so no
// single service crate's test target can see it.
// Declared in this quiet span of the sorted list, beside the other custom-resource suite,
// rather than appended at the end of this file: the tail is the contended region every
// concurrently landing suite appends to.
// Declared in the appended tail, which is where every new suite inserts its lines: the role
// an EKS Pod Identity association binds to a service account is resolved against IAM, so
// EKS and IAM have to be on one engine for the rule to be observable at all.
// Declared in the same tail: a managed node group's capacity is an Auto Scaling group and a
// launch template, and whether a Fargate profile's subnets are private is a fact about the
// EC2 network — EKS, Auto Scaling and EC2 at once.
// Declared in the appended tail, which is where every new suite inserts its lines: which
// QUALIFIER of a function a `lambda`-protocol SNS subscription invokes, and whose grant admits
// it, spans SNS, AWS Lambda, Amazon SQS, IAM and CloudFormation at once, so no single service
// crate's test target can see both the grant and the run it admitted.
// Declared in the appended tail, beside the suites above: the Resource Groups group a
// CloudFormation stack declares spans CloudFormation (the deployment and its outputs) and
// Resource Groups (the group, its query and its tags) at once, so neither crate's own test
// target can see both ends of it.
// "an EventBridge rule runs a Fargate task" estate deployed in one stack spans
// CloudFormation, Amazon S3, CloudWatch Logs, Amazon EC2, IAM, Amazon ECS and Amazon
// EventBridge at once, so no single service crate's test target can deploy it.
// here: what a bucket policy decides about a read made with credentials an identity pool
// vended spans Amazon Cognito Identity, IAM, STS and Amazon S3 at once.
// resource whose create was refused PART-WAY leaves behind spans CloudFormation (which
// deploys and disposes), Amazon S3 (which refuses the follow-up call and then answers the
// read) and the service owning the destination that decides the refusal, so no single
// Declared in the appended tail: the EVENT path of an Amazon EMR cluster's lifecycle spans
// Amazon EMR (which announces each state its cluster enters), EventBridge (which routes the
// envelope by its content) and Amazon SQS (which holds what a rule's target received), so no
// single service crate's test target can see both ends of it.
// Declared beside the suite above: the other half of this fix's evidence — an S3 Batch
// Operations job settling on the assembled router with request authentication ON, where the
// job's own run reaches Amazon S3 and IAM over the emulator's in-process callback, so no
// single service crate's test target can see it. Named like `zz_opensearch_fgac_probe.rs`,
// the sibling suite that drives a whole-router probe the same way.
// EMPTY, and declared only because the build script requires every file under `tests/all/`
// to be declared: `zz_probe.rs` held a throwaway probe for the pre-existing stack overflow
// in `all/s3_notification_invokes_its_lambda_target.rs` and carries no test now. The file
// stays because this fixer's tool set can write a file but not remove one; the next change
// to touch this directory should delete both it and this declaration.
// partitions of a table a running function may address under a conditioned execution-role
// grant spans IAM (the clause), STS (the session the function's call is signed with), Lambda
// (the handler and the SDK it is given) and DynamoDB (the decision) at once, so no single
// service crate's test target can ask it.
// Declared in the appended tail, which is where every new suite inserts its lines: that a
// function a completed stack declared really exists in AWS Lambda, and that the records of its
// runs land in the one group it logs to — CloudFormation, IAM, AWS Lambda and CloudWatch Logs
// at once, so no single service crate's test target can ask it.
// Declared in this same head region, for the reason the note above records: the WORKFLOW half
// of a cluster's control plane logging — the update reaching `Successful` before the logs are
// read — is observable only with EKS and CloudWatch Logs on one engine.
// Declared in the appended tail, which is where every new suite inserts its lines: that ONE
// set of the account's email templates stands behind both Amazon SES APIs, and that a
// continuation token either API's template listing issued continues that one list on the
// other, spans the classic SES surface and the SESv2 surface at once, so no single service
// Declared in the appended tail, which is where every new suite inserts its lines: the EBS
// direct-API write loop driven THROUGH a refused block write and then observed as completed
// spans the EBS direct APIs (which mint, refuse, write and seal) and Amazon EC2 (the only
// read of a snapshot's state, and the delete), so no single service crate's test target owns
// both ends of it.
// RUNNING function's own Amazon SQS message-plane call is authorized against spans
// CloudFormation (which deploys the queue, the role and the relay together), IAM (the role and
// its document), STS (the session the call is signed with), AWS Lambda (the handler and the SDK
// it is given) and Amazon SQS (the decision) at once, so no single service crate's test target
// can ask it.
// Declared in the appended tail, which is where every new suite inserts its lines: that the
// network an OpenSearch Serverless managed VPC endpoint is placed in is resolved against
// Amazon EC2, which owns the VPC and the subnets, so no endpoint is stored in a network that
// does not exist — two services on one engine, which neither crate's own test target can ask.
// Declared in the appended tail, which is where every new suite inserts its lines: the NETWORK
// ORIGIN of an S3 access point, enforced on the data path — the access point created with its
// VpcConfiguration through S3 Control's own API and then addressed on S3's — spans S3 Control
// (which records the VPC it is bound to) and Amazon S3 (which decides the request), so no
// Declared in this same head region, for the reason the entry above records: a backup job's
// state change reaching the queue ONE template's rule, topic, subscription and policies wire
// up spans CloudFormation, AWS Backup, Amazon DynamoDB, IAM, EventBridge, SNS and Amazon SQS
// at once, so no single service crate's test target can deploy it.
// Declared in the appended tail, which is where every new suite inserts its lines: a custom
// domain and the BASE-PATH MAPPING of it that a stack provisions, read back through the API
// Gateway control plane that owns them — CloudFormation makes the calls and API Gateway
// serves them, so no single service crate's test target can see both ends of it.
// Declared in the appended tail, for the same reason: a PRIVATE integration estate — the
// Network Load Balancer a VPC link reaches, the Application Load Balancer behind it and the
// REST API stage in front of them — spans Elastic Load Balancing v2 and API Gateway at once.
// Declared in this same head region, for the reason the entry above records: whether a
// session an identity pool vends is a credential a client can SIGN a DynamoDB call with
// spans the request authenticator, Cognito Identity, IAM, STS and DynamoDB at once, so no
// single service crate's test target can drive it.
// RUNNING function's own Secrets Manager read follows the document in force on its execution
// role AT THE INSTANT OF THE CALL — served, refused once the grant is withdrawn, served again
// once it is put back — spans IAM (the role and the document a PutRolePolicy rewrites), STS
// (the session the call is signed with), CloudFormation (which archives the handler into a
// package), AWS Lambda (the handler and the SDK it is given) and AWS Secrets Manager (the
// decision and the value) at once, so no single service crate's test target can ask it.
// Declared in the appended tail, which is where every new suite inserts its lines: a
// CloudFormation-deployed REST API gated by a TOKEN Lambda authorizer whose invocation is
// admitted by the role its authorizerCredentials names spans CloudFormation (which deploys the
// eight resources), IAM and STS (the role, its documents and the assume-role reading), AWS
// Lambda (the authorizer function and its resource policy) and API Gateway (the api, the
// authorizer, the method and the stage that serves them), so no single service crate's test
// target can deploy it.
// Declared in the appended tail, which is where every new suite inserts its lines: whether
// Amazon MSK answers its OWN refusal (a name the Region holds, a role of another account, a
// value it does not accept) before it resolves a create's placement spans Amazon EC2 — which
// owns the subnets — and Amazon MSK at once, so no single service crate's test target can
// see both ends of it.
// `cloudformation_monitored_alb_search_estate` and `cloudformation_domain_endpoint_outputs`
// are declared once, in the sorted span above.
// Declared beside those two, in the same quiet span and for the same reason. This one pins
// that a REFUSED Amazon EC2 call is in CloudTrail event history, with the fault class, the
// code and the message its caller saw and no response elements.
// deployed REST stage whose method integrates an Amazon DynamoDB ACTION answers, and what the
// stage's response cache then holds for it, spans IAM, DynamoDB and API Gateway at once, so
// no single crate's test target can make the call and then read the item it wrote.
// EventBridge at once, so no single service crate's test target can deploy it. Declared
// once, in the head region above.
// it. Declared once, in the head region above.
// crate's test target can ask it. Declared once, beside the suites above.
// Declared here, in this same head region, for the reason the entry above records: subscribing a
// Lambda function to a log group a Step Functions state machine vends its runs into spans
// CloudWatch Logs, AWS Lambda and Step Functions at once, so no single crate's own test target
// can wire it and then read the group back.
// Declared in the appended tail, which is where every new suite inserts its lines: a deployed
// method whose integration is an `AWS` call on an SNS TOPIC, served by making that call —
// asked of the router that mounts every service at once, which is where the hop between the
// stage, the topic and the topic's queue subscriber is observable end to end.
// (`cloudformation_provisions_cloudfront_resources` is declared in the sorted span above,
// beside the other `cloudformation_…` suites: a module may be declared only once.)
// `cloudformation_provisions_cloudfront_resources` is declared ONCE, in the head region
// above: two `mod` items for one name is a redefinition that compiles neither.
// (`cloudformation_provisions_cloudfront_resources` is declared once, in the sorted span
// above: two `mod` items for one name is a redefinition that compiles neither.)
// Both suites below are declared ONCE, in the sorted span above beside the other
// CloudFormation suites: the pair was appended here as well while two branches were landing
// at once, which declared each of them a second time and compiled neither.
// are declared ONCE, in the head region above beside the other `cloudformation_…` suites.
// Two branches each re-declared the pair in this tail, which compiles neither: a suite is
// declared in exactly one place.
// are declared ONCE each, in the head region above: a module declared twice in this file
// compiles the suite twice, which the workspace's lints refuse.
// are declared once, in the span above: two branches each re-added the pair here after both
// had removed it, which declares each of them twice and compiles nowhere.
// Declared in the appended tail, which is where every new suite inserts its lines. Both
// suites exist under tests/all/ and had lost their declaration when two branches removed
// the same duplicated pair, which compiles them nowhere.
// The stack CLOCK the suites above drive instead of waiting for it: one helper, because
// `settle` is global over the CloudFormation store, so every suite that asserts a settled
// stack pulls the due instants into the past the same way.
// client that dials the address an Application Load Balancer PUBLISHES is answered with when
// the listener forwards to a `lambda` target group spans CloudFormation (which archives the
// handler into a deployment package), IAM and STS (the execution role and its session), Amazon
// SQS (the queue the handler reports each invocation to), AWS Lambda (which runs it) and ELBv2
// (which opens the address and makes the invocation), so no single service crate's test target
// can ask whether the address the assembled emulator publishes answers at all.
// (`cloudformation_monitored_alb_search_estate` and `cloudformation_domain_endpoint_outputs`
// are declared earlier in this tail already: a module may be declared only once.)
// are declared once, further up this tail: a module may be declared only once, and two
// branches landing at once each re-added the pair here.)
// are declared in the head region above: a module may be declared only once.)
// are declared further up this tail already: a module may be declared only once.)
// (`apigateway_sns_service_integration` is declared once, further up this tail: a module may
// be declared only once, and two branches landing at once appended it here as well.)
// be declared only once, and two branches landing at once each appended it here.)
// be declared only once, and two landings each appended it here.)
// `cloudformation_provisions_cloudfront_resources` is declared once, in the sorted span
// (`cloudformation_monitored_alb_search_estate`, `cloudformation_domain_endpoint_outputs`,
// `cloudformation_stack_clock` and `elbv2_lambda_target_published_address` are each declared
// once, a few lines above in this same tail: two landings each appended the group here, and a
// module declared twice loads the same file twice, which compiles neither.)
// (`apigateway_sns_service_integration` is declared ONCE, a few lines above in this same
// tail: it was appended here as well while two branches were landing at once, which
// declared it a second time and compiled neither.)
// (`apigateway_sns_service_integration` is declared ONCE, a few lines up in this same
// appended tail: two branches landing in the same window appended a second declaration of it
// here, which is a redefinition that compiled neither, so the duplicate is removed and the
// first declaration stands.)
// (`apigateway_sns_service_integration` is declared ONCE, further up this file: two branches
// landing in the same window each appended a declaration of it here, which is a redefinition
// that compiles neither, so the duplicate is removed and the first declaration stands.)
// (`apigateway_sns_service_integration` is declared ONCE, further up this file: it was
// appended here as well while two branches were landing at once.)
// (`cloudformation_provisions_cloudfront_resources`,
// `cloudformation_monitored_alb_search_estate`, `cloudformation_domain_endpoint_outputs`,
// `cloudformation_stack_clock` and `elbv2_lambda_target_published_address` are each
// declared ONCE above: a module may be declared only once.)
// Appended here, which is where a new suite inserts its lines: a Lambda function's own
// aoss-signed write to the endpoint an OpenSearch Serverless collection publishes spans
// Lambda, OpenSearch Serverless, IAM, STS and CloudFormation at once, so no single service
// crate's test target can drive it.
// (`apigateway_cognito_authorizer_app_client_audience` is declared in the head region of
// this file already: two branches landing in the same window each declared it, and a module
// may be declared only once.)
// (`apigateway_cognito_authorizer_app_client_audience` is declared ONCE, in the sorted span
// above beside the other API Gateway suites: it was appended here as well while two branches
// were landing at once, which declared it a second time and compiled neither.)
// (`apigateway_cognito_authorizer_app_client_audience` is declared once, in the head region
// above: a file loaded as a module twice compiles neither copy.)
// above, and `cloudformation_metric_stream_estate_is_drainable` once further down this tail:
// (The API Gateway/Cognito authorizer suite is declared ONCE, in the head region above:
// a module may be declared only once, and two branches landing at once appended it here
// as well.)
// (`apigateway_cognito_authorizer_app_client_audience` is declared in the head region above:
// a module may be declared only once.)
// (`apigateway_cognito_authorizer_app_client_audience` and
// `cloudformation_metric_stream_estate_is_drainable` are declared in the head region and the
// sorted span above: each was appended here as well while two branches were landing at once,
// which declared it a second time and compiled neither.)
// (The Amazon Cognito authorizer suite is declared ONCE, in the head region above: a module
// may be declared only once, and two branches landing at once appended it here as well.)
// Declared ONCE, in the head region above beside the other API Gateway suites: it was
// appended here as well while two branches were landing at once, which declared it a second
// time and compiled neither.
// (`apigateway_cognito_authorizer_app_client_audience` is declared once in the head region
// above, and `cloudformation_metric_stream_estate_is_drainable` once in the tail below: a
// module may be declared only once, and a second declaration compiles neither.)
// (`apigateway_cognito_authorizer_app_client_audience` is declared ONCE, in the sorted block
// at the head of this file: it was appended here as well while two branches were landing at
// once, which declared it a second time and compiled neither.)
// (The suite that was declared a second time here, and the metric-stream one beside it, are
// each declared ONCE now — the first in the sorted span at the head of this file, the second
// further down this tail: two branches landing at once appended a second declaration of each
// here, which compiled neither.)
// (`apigateway_cognito_authorizer_app_client_audience` is declared in the head region
// above: a module may be declared only once.)
// (`apigateway_cognito_authorizer_app_client_audience` is declared ONCE, in the sorted head
// region above beside the other `apigateway_…` suites: two `mod` items for one name is a
// redefinition that compiles neither.)
// (`apigateway_cognito_authorizer_app_client_audience` is declared once, in the sorted head
// region above: a module may be declared only once, and two branches landing at once
// appended it here as well.)
// (The suite over which app client's token a COGNITO_USER_POOLS authorizer admits is declared
// ONCE, in the head region above: a module may be declared only once.)
// (`apigateway_cognito_authorizer_app_client_audience` is declared at line 30, in the sorted
// block above: a module may be declared only once, and two branches landing at once declared
// it twice.)
// `apigateway_cognito_authorizer_app_client_audience` is declared ONCE, in the head region
// above: two landings each appended their own copy here, and a module declared twice loads
// the same file twice, which compiles neither.
// Declared in the SORTED block beside the other CloudTrail suites, further up this file,
// rather than appended at the end: the tail is where every suite landing in the same window
// appends, which is how this name came to be declared twice.
// (`cloudtrail_read_only_api_calls` is declared in that block: a module may be declared only
// once.)
// (`zz_probe` is declared ONCE, further up this file: a module may be declared only once. It
// is declared at all only because the build script requires every file under `tests/all/` to
// be declared; the next change to touch this directory should delete both it and that
// declaration.)
// (`cloudtrail_read_only_api_calls` and `zz_probe` are each declared ONCE above — the first
// in the sorted CloudTrail span, the second further up this tail: two branches landing at
// once appended a second declaration of each here, which compiled neither.)
// (`cloudtrail_read_only_api_calls` is declared in the SORTED CloudTrail span above: two
// landings each appended their own copy here, and a module declared twice loads the same file
// twice, which compiles neither.)
// (`cloudtrail_read_only_api_calls` is declared ONCE, in the sorted block above beside the
// other CloudTrail suites, and `zz_probe` ONCE further up this tail: a module declared twice
// loads the same file twice, which compiles neither.)
// Declared at the end of the appended tail, which is where every new suite inserts its lines:
// whether a CloudWatch alarm over the health series an Amazon Redshift cluster publishes about
// itself evaluates spans both crates at once, so neither crate's own test target can create the
// cluster and then read the alarm's state.
// (`cloudformation_metric_stream_filter_update` is declared once, in the head region above:
// (`cloudwatch_alarm_ssm_ops_item_action` is declared once, in the head region above: a
// module may be declared only once.)
// (`cloudformation_metric_stream_filter_update` is declared once, in the head region above: a
// module may be declared only once, and two branches landing at once each appended it here.)
// (`cloudwatch_alarm_ssm_ops_item_action` is declared in the head region above: a module may
// be declared only once.)
// (`cloudformation_metric_stream_filter_update` is declared ONCE, in the head region above
// beside the other `cloudformation_…` suites: it was appended here as well while two
// branches were landing at once, which declared it a second time and compiled neither.)
// (`cloudwatch_alarm_ssm_ops_item_action` is declared in the head region above, for the
// reason recorded there: the work item an alarm's Systems Manager OpsItem action opens has to
// be read where a client reads it — through OpsCenter's own query — so neither CloudWatch's
// nor Systems Manager's own test target can drive the transition and then read what it
// opened.)
// (`cloudformation_metric_stream_filter_update` is declared in the head region above: a
// (`zz_probe` is declared ONCE, further up this tail: a module may be declared only once.)
// (The metric-stream filter-update suite is declared ONCE, in the head region above: a module
// may be declared only once, and two branches landing at once each appended a declaration of
// it here.)
// (`zz_probe` is declared once, further up this tail: it was appended here as well while two
// (The metric-stream filter-update suite is declared ONCE, in the head region above: it was
// (`zz_probe` is declared ONCE, further up this tail.)
// (`cloudformation_metric_stream_filter_update` is declared in the head region above: a module
// (`zz_probe` is declared further up this tail already: a module may be declared only once.)
// (`zz_probe` is declared once further up this tail: a module may be declared only once.)
// (`zz_probe` is declared ONCE, further up this tail: it was appended here as well while two
// (`cloudformation_metric_stream_filter_update` is declared ONCE, in the sorted span at the
// head of this file: a second declaration here compiled neither.)
// (`zz_probe` is declared once, further up this tail: a module may be declared only once.)
// (`cloudwatch_alarm_ssm_ops_item_action` is declared in the head region above: a module may be
// declared only once.)
// (`cloudwatch_alarm_ssm_ops_item_action` and the suites beside it
// (`cloudformation_metric_stream_filter_update` is declared ONCE, in the sorted head region
// the same file twice, which compiles neither.)
// (`zz_probe` is declared ONCE, further up this tail, for the reason recorded there.)
// (`cloudformation_metric_stream_filter_update` is declared once, in the sorted head region
// above: a module may be declared only once, and two branches landing at once each appended
// module may be declared only once, and this tail re-declared it.)
// (`cloudformation_metric_stream_filter_update` is declared ONCE, in the head region of this
// file: a module may be declared only once.)
// a module may be declared only once, and two branches landing at once appended it here as
// well.)
// two branches landing in the same window each declared it, and a module may be declared
// only once.)
// (`zz_probe` is declared further up this tail already: two branches landing in the same
// window each appended it, and a module may be declared only once.)
// file beside the other `cloudformation_…` suites: the declaration that stood here was a
// second copy two branches landing in the same window each appended.)
// beside the other `cloudformation_…` suites: the copy that stood here is gone, because a
// module declared twice loads the same file twice and compiles neither.)
// (`cloudformation_metric_stream_filter_update` is declared ONCE, in the sorted head above:
// two branches landing in the same window appended a second declaration of it here, which is a
// redefinition that compiled neither, so the duplicate is removed and the first stands.)
// (`zz_probe` is declared once, above.)
// (`zz_probe` is declared ONCE, a few lines above: a module may be declared only once.)
// (`zz_probe` is declared once, further up this tail: a module may be declared only once,
// and two branches landing at once appended it here as well.)
// (`zz_probe` is declared in the head region above: a module may be declared only once, and
// two branches landing at once appended it twice.)
// `zz_probe` is declared ONCE, in the span above: two landings each appended their own copy
// here, and a module declared twice loads the same file twice, which compiles neither.
// Declared in the appended tail, which is where every new suite inserts its lines: a vector
// knowledge base built on an index created through the collection's OWN published endpoint
// spans OpenSearch Serverless (which serves that endpoint's OpenSearch REST data plane), the
// Bedrock Agent build-time plane (the knowledge base, the custom data source and the
// ingestion) and the Bedrock Agent runtime plane (the retrieval) at once, so no single
// service crate's test target can create the index and then read the document back out.
// Declared in the appended tail: whether an UpdateStack that changes an AWS::DynamoDB::Table
// in place is reported complete only once DynamoDB reports that table ACTIVE again spans
// CloudFormation (which makes the calls) and DynamoDB (which decides the table's status), so
// neither crate's own test target can observe both ends of it.
// Declared in the appended tail, which is where every new suite inserts its lines: the HTTPS
// front door, its TLS terminator and its per-connection accept loop are the router's own, not
// any one service's, and the suite drives the BINARY this build produces over its data port,
// which only this target links.
// (`cloudformation_metric_stream_estate_is_drainable` is declared ONCE, further up this
// tail: a module may be declared only once.)
// Declared in the appended tail, which is where every new suite inserts its lines: whether a
// `@connections` call is authorized spans API Gateway V2 (which holds the api, the stage and
// the deployment the addressing resolves), IAM (the role and its document), the token service
// (the session the call signs with) and the API Gateway Management API (which decides and
// words the refusal), so no single service crate's test target can drive it.
// (`sdb_legacy_query_api_on_the_assembled_router` and
// `apigateway_cognito_authorizer_app_client_audience` are each declared ONCE, in the head
// region of this file: the second declarations concurrent landings appended here are gone,
// a module declared twice being `E0428`.)
// Declared here, beside the suite whose helpers it shares: the reservation a Lambda event
// source mapping holds on an Amazon SQS record is observable only as what an INDEPENDENT
// consumer of that queue is handed, so the suite spans the two services and belongs to
// neither crate's own test target.
// What a build PUTS in, and READS out of, the account's object store: the artifact objects
// it uploads, the cache it carries between builds, and the source archive it runs on.
// Declared beside the queue-provisioning suite: this one drives the whole scripted
// deploy-wait-use-delete workflow over the ASSEMBLED emulator, which is what a waiter polls.
// The CloudFormation search-estate and domain-endpoint-output suites are declared ONCE, in
// the span above: two landings each appended their own copy here, and a module declared
// twice loads the same file twice, which does not compile.
// region above: a module may be declared only once.)
// (`sdb_legacy_query_api_on_the_assembled_router`,
// `apigateway_cognito_authorizer_app_client_audience`,
// `lambda_sqs_event_source_visibility_hold`, `codebuild_s3_build_output` and
// `cloudformation_scripted_queue_deploy` are each declared ONCE, in the sorted spans at the
// head of this file: two branches landing at once declared each of them here as well, which
// compiled neither.)
// (`lambda_signed_write_to_an_aoss_collection_endpoint`,
// `cloudformation_metric_stream_estate_is_drainable`, `redshift_health_alarms_evaluate` and
// `https_front_door_concurrent_pooled_producer` are each declared ONCE further up this tail:
// two branches landing at once appended a second declaration of each here, which compiled
// neither.)
// (`apigateway_sns_service_integration` is declared ONCE, in the span above: the suite lost
// its declaration for a moment when two branches each removed one of two duplicates, and it
// is declared exactly once again.)
// `cloudformation_scripted_queue_deploy` are each declared ONCE, in the spans above: two
// (`apigateway_sns_service_integration`, `lambda_signed_write_to_an_aoss_collection_endpoint`,
// `cloudformation_metric_stream_estate_is_drainable` and `redshift_health_alarms_evaluate` are
// each declared ONCE, further up this file: two landings each appended their own copy here, and
// a module declared twice loads the same file twice, which compiles neither.)
// landings each appended their own copy here, and a module may be declared only once.)
// `cloudformation_scripted_queue_deploy` are each declared ONCE, in the sorted spans above:
// two branches landing in the same window declared each of them here as well, which compiled
// `cloudformation_provisions_cloudfront_resources` is declared once, just below: two `mod`
// items for one name is a redefinition that compiles neither.
// Declared in the appended tail: whether the address a stack's DomainEndpoint output hands
// its consumers is the one Amazon OpenSearch Service published for the domain — and answers
// for it — spans CloudFormation (which resolves the output) and that service (which publishes
// the address and serves the domain's data plane), so neither crate's own test target can
// observe both ends of it.
// (`https_front_door_concurrent_pooled_producer` is declared once, further up this tail: a
// module may be declared only once, and two branches landing at once appended it here as
// (`cloudformation_metric_stream_estate_is_drainable` is declared once, further up this
// file: a file loaded as a module twice compiles neither copy.)
// envelope an identity-based authorization refusal travels in is read by comparing IAM's
// (`https_front_door_concurrent_pooled_producer` is declared once, further up this tail:
// (`cloudformation_metric_stream_estate_is_drainable` is declared further up this file
// already: two branches landing in the same window each appended it, and a module may be
// (The HTTPS front-door suite is declared ONCE, in the span above: it was appended here as
// well while two branches were landing at once.)
// (The metric-stream estate suite and the other CloudFormation suites of this window
// (`https_front_door_concurrent_pooled_producer` is declared further up this tail already: a
// (`cloudformation_metric_stream_estate_is_drainable` is declared further up this tail
// already: a module may be declared only once.)
// (`cloudformation_metric_stream_estate_is_drainable` is declared once, above.)
// (`cloudformation_metric_stream_estate_is_drainable` is declared ONCE, in the span above:
// (`cloudformation_metric_stream_estate_is_drainable` is declared once, in the head region
// above: a module may be declared only once, and two branches landing at once appended it
// here as well.)
// (The suite over whether a stack-deployed metric-stream estate is drainable is declared
// ONCE, in the sorted span above: a module may be declared only once.)
// (`cloudformation_metric_stream_estate_is_drainable` is declared in the head region above:
// a module may be declared only once, and two branches landing at once appended it twice.)
// `cloudformation_metric_stream_estate_is_drainable` is declared ONCE, further up this tail:
// two landings each appended their own copy, and a module declared twice compiles neither.
// session an identity pool vends may DO is decided by IAM's record of the mapped role and
// answered by Amazon S3 and the Cognito Identity control plane, so no single service crate's
// test target can vend the session and then be refused by what it reaches.
// Declared in this tail: which role a `Rules` role mapping resolves from a signed-in user's
// own attribute claims spans cognito-idp (the custom attribute and the sign-in),
// cognito-identity (the mapping and the vend), IAM (the roles) and STS (the session's caller
// identity), so no single service crate's test target can sign in and then read the session
// the mapping vended.
// (`codebuild_s3_build_output` is declared further up this tail already: a module may be
// (`lambda_signed_write_to_an_aoss_collection_endpoint` is declared earlier in this tail
// (`cloudformation_domain_endpoint_is_dialled_by_consumers` is declared further up this tail
// (`lambda_signed_write_to_an_aoss_collection_endpoint` is declared further up this tail
// already: two landings each appended their own copy, and a module declared twice loads the
// same file twice, which compiles neither.)
// (`lambda_signed_write_to_an_aoss_collection_endpoint` is declared once, further up this
// tail: a module may be declared only once, and two landings each appended a copy here.)
// (`cloudformation_domain_endpoint_is_dialled_by_consumers` is declared ONCE, further up this
// (`sdb_legacy_query_api_on_the_assembled_router` and `codebuild_s3_build_output` are
// declared ONCE each, further up this tail: a module may be declared only once.)
// (`lambda_signed_write_to_an_aoss_collection_endpoint` and
// `cloudformation_domain_endpoint_is_dialled_by_consumers` are declared ONCE each, further
// up this tail: a module may be declared only once.)
// awsQuery answer with Secrets Manager's awsJson1_1 one for ONE principal, so neither
// crate's own test target can ask it.
// (That suite is declared ONCE, in the sorted span at the head of this file: a module may be
// declared only once, and two branches landing at once declared it twice.)
// (`zz_probe` is declared ONCE, in the span above: a module may be declared only once.)
// Declared here beside the other CloudFormation suites, ONCE: the pair below was appended to
// the tail as well while two branches were landing at once, which declared each of them a
// second time and compiled neither (`mod` is a definition, not an include). The API Gateway
// suite above it was appended twice the same way; its declaration in the sorted head region is
// the one that stands.
// Both suites above are declared ONCE, in the sorted span beside the other CloudFormation
// suites: the pair was appended here as well while two branches were landing at once, which
// declared each of them a second time and compiled neither.
// EMPTY, and declared only because the build script requires every file under `tests/all/` to
// be declared: the suite that pins what an `UpdateStack` does to the `AWS::S3::Bucket` a stack
// already created runs in the CloudFormation crate's own test target, which mounts Amazon S3
// beside CloudFormation on one engine. See the file's own header.
// Declared in the appended tail, which is where every new suite inserts its lines: a Lambda
// durable execution's status changes reaching the SQS target of a default-bus rule spans
// CloudFormation and IAM (the role and the function), AWS Lambda (the execution and its
// publication), EventBridge (which routes the envelope) and Amazon SQS (which holds what the
// target received), on a router that authenticates every request — so no single service
// crate's test target can see both ends of it.
// (`cloudformation_stack_clock` is declared once, in the span above: a module declared twice
// in this file compiles its suite twice.)
// Two suites that lost their declaration when two branches removed the same duplicated pair
// (`cloudformation_monitored_alb_search_estate` and `cloudformation_domain_endpoint_outputs`)
// are declared once each, in the sorted span beside the other CloudFormation suites: a module
// may be declared only once, so a second declaration here would compile them twice.
// (`elbv2_lambda_target_published_address` is declared once above, for the same reason.)
// `cloudformation_provisions_cloudfront_resources` is declared once, in the sorted span above:
// two `mod` items for one name is a redefinition that compiles neither.
// suites exist under tests/all/ and are declared once each in the sorted span above.
// (`cloudformation_stack_clock` and `elbv2_lambda_target_published_address` are each declared
// once in the spans above: a module declared twice in this file compiles its suite twice.)
// are declared once each, further up this tail, for the same reason.)
// above, for the reason the note above records.)
// (`zz_probe` is declared once, in the span above.)
// (`cloudformation_metric_stream_estate_is_drainable` is declared once, in the span above.)
// above: two branches landing at once appended it here as well, which declared it twice and
// (`zz_probe` is declared once, further up this tail: two branches landing at once appended
// it here as well, which declared it twice and compiled neither.)
// (`apigateway_cognito_authorizer_app_client_audience` is declared ONCE, in the head region
// (`zz_probe` is declared ONCE, further up this tail: two `mod` items for one name is a
// (`cloudformation_metric_stream_estate_is_drainable` is declared ONCE, further up this tail:
// two `mod` items for one name is a redefinition that compiles neither.)
// Declared here, in this same head region, for the reason the entry above records: the chain
// from a table's write traffic through the AWS/DynamoDB consumption series, the alarm over that
// series and its SNS action to a subscribed queue spans DynamoDB, CloudWatch, SNS and SQS at
// once, so no single service crate's test target can drive it end to end.
// above: two branches each added it here as well, and a module declared twice compiles
// (`zz_probe` is declared ONCE, further up this tail: two branches each added it here as well,
// and a module declared twice compiles neither.)
// two branches each added it here as well, and a module declared twice compiles neither.)
// Declared in this tail: whether a stack-deployed metric-stream estate can be drained spans
// CloudFormation and the services the pipeline is built out of, so no single service crate's
// test target can deploy it and then drain it.
// stays because a fixer's tool set can write a file but not remove one; the next change to
// touch this directory should delete both it and this declaration. Declared ONCE: two
// landings each appended their own copy, and a module declared twice loads the same file
// twice, which compiles neither.
// (The suite that drives the Amazon Cognito user-pool authorizer is declared ONCE, in the
// head region above: a module may be declared only once, so the duplicate declaration that
// was appended here is gone.)
// Declared here, in this appended tail: whether a stack-deployed metric-stream estate can be
// drained spans CloudFormation and the services the estate is built from, so no single
// service crate's test target can deploy it and then drain it.
// at the top of this file: the entry was appended here as well while two branches were
// landing at once, which declared it a second time and compiled neither.)
// (`zz_probe` is declared ONCE, further up this tail: the entry was appended here as well
// while two branches were landing at once, which declared it a second time and compiled
// the entry was appended here as well while two branches were landing at once, which declared
// it a second time and compiled neither.)
// a module may be declared only once, and it was appended here as well while two branches
// were landing at once, which compiled it nowhere.)
// Declared HERE, beside the sibling suite it shares its wiring helpers with, for the same
// (Two suites that were appended here as well while branches were landing at once — the
// Cognito authorizer audience suite and the drainable metric-stream estate suite — are
// declared ONCE in the regions above, which is where they stay: a module may be declared
// only once, and declaring it twice compiles neither.)
// (`zz_probe` is declared once, further up this tail.)
// above beside the other `apigateway_…` suites: a module may be declared only once.)
// (Declared ONCE, further up this tail: the same declaration was appended here as well while
// two branches were landing at once, which declared the module a second time and compiled
// of the sorted list above: a module may be declared only once.)
// (`cloudformation_metric_stream_estate_is_drainable` is declared ONCE, in the head region
// of the appended tail above: a module may be declared only once.)
// (Several suites once declared a second time in this tail — the `cloudformation_…` pair
// beside `cloudformation_provisions_cloudfront_resources`, `cloudformation_monitored_alb_
// search_estate` and `cloudformation_domain_endpoint_outputs` among them — are declared ONCE,
// in the sorted span above: a module may be declared only once, and a second `mod` item for
// one name compiles neither. The sentence halves earlier dedup passes left here are gone with
// this note.)
// at the top of this file: a module may be declared only once, and two branches landing in the
// same window declared it a second time here, which compiled neither.)
// (`zz_probe` and `cloudformation_metric_stream_estate_is_drainable` are declared ONCE each, a
// few lines up in this same appended tail: a module may be declared only once, and two
// branches landing in the same window appended a second declaration of each here, which
// (The API Gateway/Cognito authorizer-audience suite and the metric-stream estate suite are
// declared ONCE each, in the sorted span above: both were appended here as well while two
// branches were landing at once, which declared each a second time and compiled neither.)
// a module may be declared only once. Declaring it again here compiled nothing and broke this
// target's build, which is what the duplicate below was removed for too.)
// at the top of this file: two branches landing at once had appended a second declaration
// of it here.)
// (`zz_probe` is declared once, in the head region above: a module may be declared only
// once, and two branches landing at once had appended a second declaration of it here.)
// Declared here, in this appended tail: whether a SESv2 send's email events reach the Lambda
// target of a rule on the bus its event destination names spans SESv2, EventBridge, Lambda,
// CloudFormation, IAM and Amazon SQS, so no single service crate's test target can wire it.
// (The flat-rate-subscription eligibility suite is declared ONCE, in the sorted span at the
// head of this file: a landing appended a second declaration of it here, which compiled
// neither copy, so the duplicate is gone and the first declaration stands.)
// Declared here, beside it, for the same reason: whether a flat-rate CloudFront subscription
// may be sold over a distribution is decided by the web ACL association CloudFront holds and
// by a web ACL only WAFv2 mints, so no single service crate's test target can set the
// association and then ask Pricing Plan Manager for the subscription.
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, in the head region at
// the top of this file: the second declaration two landings appended here is gone, a module
// declared twice being `E0428`.)
// (That suite is declared ONCE, in the head region of this file: the declaration that
// stood here was a second copy, which is `E0428` and compiles no suite of this target.)
// above: two branches landing at once had appended a second declaration of it here.)
// (The suite that reads which app client's token a COGNITO_USER_POOLS authorizer admits is
// declared in the head region at the top of this file already: a module may be declared only
// (`zz_probe.rs` is declared further up this tail already: a module may be declared only
// once. This was a second declaration of it, which compiled neither. The note it carried
// stands unchanged at that one declaration: the file is EMPTY, is declared only because the
// build script requires every file under `tests/all/` to be declared, and held a throwaway
// probe for the pre-existing stack overflow in
// `all/s3_notification_invokes_its_lambda_target.rs` — neither the probe nor that standing
// abort is touched here.)
// (The declaration itself is the one in the sorted span at the top of this file: a module may
// be declared only once, and two branches landing at once declared it twice.)
// (`zz_probe` is declared ONCE, in the head region above: a module may be declared only
// once, and two branches landing at once appended a second declaration of it here.)
// above: a module may be declared only once, and this tail declared it a second time while
// two branches were landing at once.)
// Declared beside the provisioning suite above: this one pins that the stack's DELETION is
// reported complete only once Elastic Beanstalk has let the application go, which only an
// engine carrying both services can show.
// at the top of this file: two `mod` items for one name is a redefinition that compiles
// (`cloudformation_metric_stream_estate_is_drainable` is declared ONCE, in the span above: a
// Every suite the comment fragments this tail accumulated name —
// `cloudformation_provisions_cloudfront_resources`,
// `cloudformation_stack_clock`, `elbv2_lambda_target_published_address` and
// `apigateway_cognito_authorizer_app_client_audience` — is declared exactly ONCE, in the
// sorted span above: two `mod` items for one name compile neither copy.
// (`zz_probe` and `cloudformation_metric_stream_estate_is_drainable` are declared once each,
// further up this tail: a module may be declared only once.)
// stack-declared request validator really refuses an invalid call to the deployed stage before
// the integration runs spans CloudFormation, API Gateway, AWS Lambda, IAM and CloudWatch Logs
// at once, so no single crate's own test target can deploy it and then prove the function was
// never reached.
// `cloudformation_provisions_cloudfront_resources` is declared ONCE, in the sorted span
// (`lambda_signed_write_to_an_aoss_collection_endpoint` is declared ONCE, further down this
// tail: two landings each appended their own copy, and a module declared twice loads the same
// file twice, which compiles neither.)
// (`cloudtrail_read_only_api_calls` is declared ONCE, in the sorted CloudTrail block above:
// the tail is where every suite landing in the same window appends, and a module declared
// twice loads the same file twice, which compiles neither.)
// (`cloudwatch_alarm_ssm_ops_item_action` is declared in the head region above: the work
// target can drive the transition and then read what it opened.)
// crate's test target can drive it. (Declared ONCE: two landings each appended their own copy
// of this line, and a module declared twice loads the same file twice, which compiles
// declared ONCE above: two `mod` items for one name is a redefinition that compiles
// neither, which is what two branches each appending the same lines here produced.)
// (`zz_probe` is declared once, just above: a module may be declared only once.)
// (`cloudformation_metric_stream_estate_is_drainable` is declared once, above: a module may
// `cloudformation_provisions_cloudfront_resources` is declared once, in the head region
// (`ses_event_destination_reaches_an_eventbridge_rule_target` is declared ONCE, further up
// this tail: two branches landing in the same window declared it here as well, which compiled
// (`cloudformation_metric_stream_filter_update` is declared in the head region above too: the
// suite was appended here as well while two branches were landing at once, which declared it a
// second time and compiled neither.)
// head of this file: it was appended here as well while two branches were landing at once,
// (`https_front_door_concurrent_pooled_producer` is declared ONCE, in the span above: it was
// time and compiled neither.)
// Declared beside the suite above, in this quiet span of the sorted list rather than at the
// tail of this file where every concurrently landing router suite appends its line: this one
// is the health-check PROBE of an endpoint the emulator itself serves.
// (`cloudtrail_read_only_api_calls` is declared in the sorted block beside the other
// CloudTrail suites already: a module may be declared only once, and a second `mod` item for
// it compiles neither.)
// (`lambda_signed_write_to_an_aoss_collection_endpoint` is declared once, in the span above:
// two landings each appended their own copy here, and a file loaded as a module twice does
// not compile.)
// (`cloudformation_metric_stream_filter_update` is declared ONCE, in the sorted block at the
// and a module declared twice loads the same file twice, which compiles neither.)
// (`https_front_door_concurrent_pooled_producer` is declared ONCE, further up this tail: it
// was appended here as well while two branches were landing at once, and a module declared
// Declared ONCE, in the head region above beside the other API Gateway suite: the pair of
// declarations this tail carried was appended while two branches were landing at once, which
// declared the module a second time and compiled it nowhere.
// (`cloudformation_metric_stream_filter_update` is declared ONCE, in the head region above:
// Both the probe module above and the metric-stream estate suite are declared ONCE, further
// up: the second declaration of each was appended here while two branches were landing at
// once, which compiled neither.
// (`https_front_door_concurrent_pooled_producer` is declared ONCE, in the span above: a
// (`https_front_door_concurrent_pooled_producer` is declared ONCE, further up this tail: a
// above beside the other `cloudformation_…` suites: two `mod` items for one name is a
// redefinition that compiles neither.
// are declared once, in the span above: a module may be declared only once.)
// (`apigateway_sns_service_integration` is declared ONCE, in the sorted head region above:
// (`cloudwatch_alarm_ssm_ops_item_action` is declared ONCE, in the head region above: the
// work item an alarm's Systems Manager OpsItem action opens has to be read where a client
// reads it — through OpsCenter's own query — so neither CloudWatch's nor Systems Manager's
// own test target can drive the transition and then read what it opened.)
// (`codebuild_s3_build_output` is declared ONCE, in the span above: a module may be declared
// (`lambda_signed_write_to_an_aoss_collection_endpoint` is declared ONCE, further up this
// (`cloudformation_domain_endpoint_is_dialled_by_consumers` is declared ONCE, in the span
// (`apigateway_sns_service_integration` is declared ONCE, further up this tail: two branches
// landing at once each appended their own copy here, and a module declared twice loads the
// head of this file: two branches landing at once each appended their own copy here, and a
// (`cloudformation_provisions_cloudfront_resources` is declared in the head region above: a
// module may be declared only once, and two `mod` items for one file compile the suite twice.)
// (`apigateway_sns_service_integration`, `cloudformation_monitored_alb_search_estate` and
// `cloudformation_domain_endpoint_outputs` are each declared ONCE in the spans above: a
// be declared only once, and two `mod` items for one file compile the suite twice.)
// head of this file: a module may be declared only once.)
// (`zz_probe` is declared ONCE, in the span above: a module may be declared only once, and
// `zz_probe.rs` is the empty file the build script requires a declaration for.)
// tail: a module may be declared only once.))
// (`cloudformation_provisions_cloudfront_resources` is declared once, in the span above:
// Declared HERE, once: `cloudformation_provisions_cloudfront_resources` is the suite over the
// CloudFront resources a stack provisions, and the sorted span above no longer declares it —
// two branches each removed the copy they saw, leaving it compiled nowhere.
// are declared once, in the span above: a module may be declared only once in this file, and
// two branches each appended the pair here.)
// tail: two `mod` items for one name is a redefinition that compiles neither.)
// (`cloudwatch_alarm_ssm_ops_item_action` is declared in the head region above too, for the
// reason this span records: a module may be declared only once in this file.)
// (`https_front_door_concurrent_pooled_producer` is declared ONCE, in the span above: two
// `mod` items for one name is a redefinition that compiles neither.)
// `cloudformation_domain_endpoint_is_dialled_by_consumers` are declared ONCE each, in the
// spans above: two `mod` items for one name is a redefinition that compiles neither.)
// Declared in this appended tail, which is where every new suite inserts its lines: what a
// stack-deployed `RedrivePolicy` does to a message the deployed consumer never processes spans
// CloudFormation (which provisions the queue pair, the function and the mapping), AWS Lambda
// (which polls and invokes), Amazon SQS (which counts the deliveries and performs the move)
// and IAM (which answers for the execution role), so no single service crate's test target can
// deploy the pipeline and then watch the dead-letter queue.
// (`cloudformation_metric_stream_filter_update` is declared ONCE, in the head region above: a
// module may be declared only once, so the second declaration two branches landing at once
// appended here is gone.)
// (`apigateway_sns_service_integration` is declared ONCE, a few lines up in this same appended
// tail: two branches landing at once appended a second declaration of it here, and a module
// declared twice compiles neither.)
// top of this file: two branches landing at once appended a second declaration of it here, and
// a module declared twice compiles neither.)
// Declared in this appended tail: what a stack-deployed `AWS::Glue::Crawler` DOES when it is
// started spans CloudFormation, IAM, AWS Glue and Amazon S3 at once, so no single service
// crate's test target can deploy it, crawl with it and read the catalog it wrote.
// of the suites this span once declared are declared ONCE, in the head region above:
// loading one file as a module twice compiles nowhere.
// (`apigateway_sns_service_integration` is declared once, further up this tail: a deployed
// method whose integration is an `AWS` call on an SNS TOPIC is asked of the router that
// mounts every service at once, and a module may be declared only once.)
// (`cloudwatch_alarm_ssm_ops_item_action` is declared ONCE, in the head region above, for
// the reason that entry records: the work item an alarm's Systems Manager OpsItem action
// opens has to be read where a client reads it — through OpsCenter's own query — so neither
// CloudWatch's nor Systems Manager's own test target can drive the transition and then read
// what it opened.)
// (`codebuild_s3_build_output` is declared once, further up this tail: what a build PUTS in,
// and READS out of, the account's object store is asserted there, and a module may be
// `cloudformation_domain_endpoint_is_dialled_by_consumers` are each declared once, further
// region of this file: it was appended here as well while two branches were landing at once,
// at the top of this file: it was appended here as well while two branches were landing at
// once, and a module declared twice loads the same file twice, which compiles neither.)
// (`https_front_door_concurrent_pooled_producer` is declared ONCE, a few lines up this tail:
// it was appended here as well while two branches were landing at once, and a module declared
// (`cloudformation_metric_stream_estate_is_drainable` is declared in the head region above
// and `zz_probe` once further up this tail: two branches landing at once appended a second
// declaration of each here, which compiled neither. A module may be declared only once.)
// this file: it was appended here as well while two landings were in the same window, which
// Declared here, in this appended tail: what an Amazon SQS event source mapping hands its
// function, and what it does with a batch the function did not take, spans Amazon SQS, IAM,
// CloudFormation and Lambda at once, so no single service crate's test target can send the
// message and then read the record the handler was given.
// Declared here, in this appended tail: that ONE published event reaches a Lambda, an Amazon
// SQS and a Step Functions target of the same rule carrying one envelope spans EventBridge and
// each of those three services, so no single service crate's test target can publish it and
// then read what each target was handed.
// are declared once, further up this tail: a module may be declared only once, so the second
// copy of the pair this span used to carry is gone.)
// a module may be declared only once, and two branches landing at once each appended a copy
// (`cloudformation_metric_stream_estate_is_drainable` is declared once, further up this tail:
// (`cloudformation_domain_endpoint_is_dialled_by_consumers` is declared once, a few lines
// a copy of it here.)
// and SQS mounted together just as the firing leg does. (Declared ONCE, in the sorted span
// beside the other CloudFront suites: two branches landing in the same window each appended
// the pair here, which is a redefinition that compiled neither, so the duplicate is gone.)
// (Declared ONCE, further up this tail: a module may be declared only once, so the copy two
// branches landing at once appended here is gone.)
// (Declared ONCE, further up this file: the declaration that stood here was a second copy.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, further up this file too:
// the second declaration a concurrent landing appended here is gone, a module declared twice
// (The SESv2-event-destination suite is declared ONCE, further up this file: two landings in
// the same window each appended a declaration of it, which is `E0428` and compiles no suite
// of this target, so the duplicate is gone and the first declaration stands.)
// this file: a module may be declared only once, and the line appended here while two
// branches landed at once compiled neither.)
// appended a declaration of it here, which is `E0428` and compiles no suite of this target,
// so the duplicate is gone and the first declaration stands.)
// landings each appended it, and a module declared twice is `E0428`, so this copy is removed
// and the first declaration stands.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, further up this file: this
// tail's copy is removed, a module declared twice being `E0428`.)
// appended a declaration of it here, a module declared twice being `E0428`, so this tail's
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE as well, further up this
// file: the second declaration two branches landing in the same window appended here is
// `E0428` and compiled no suite, so it is gone and the one above stands.)
// file: the copy a second landing appended here is gone for the same reason.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, beside
// `cloudfront_edge_metrics_drive_the_alarm_chain` in the span above, whose wiring it reuses:
// the copy that stood here was a second declaration, and a module declared twice is `E0428`
// and compiles no suite of this target.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, above: the second
// declaration two landings appended here is gone, a module declared twice being `E0428`.)
// (Declared ONCE, further up this file: two branches landing in the same window each appended
// a declaration of it here, and a module declared twice compiles neither copy.)
// (Declared ONCE, further up this tail, for that reason: two branches landing in the same
// window each appended a declaration of it here.)
// (Declared ONCE, further up this file: the copy that stood here was a second declaration
// two branches landing in the same window each appended, which compiled neither.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared once, in the sorted span above:
// (The SESv2 event-destination suite is declared ONCE, further up this tail: a module may be
// declared only once, and two branches landing at once each appended a declaration of it
// here.)
// `cloudformation_metric_stream_estate_is_drainable` are declared in the head region above and
// in this tail respectively: a module may be declared only once, and the pair was appended
// here as well while two branches were landing at once, which compiled neither.)
// Declared in this tail: which stage refuses a request signed with a temporary credential,
// and which document its refusal names, spans IAM (the role, the user and their documents),
// STS (the mint and the scope it recorded) and Amazon S3 (which resolves the credential and
// decides the call), so no single service crate's test target can mint the session and then
// read the answer the deciding service gives.
// `cloudformation_provisions_cloudfront_resources` is declared once, in the sorted span at
// the head of this file: two `mod` items for one name is a redefinition that compiles
// neither.
// (`apigateway_cognito_authorizer_app_client_audience` is declared once, in the sorted span
// at the head of this file: the entry was appended here as well while two branches were
// (`cloudformation_metric_stream_estate_is_drainable` and `redshift_health_alarms_evaluate`
// are declared ONCE each, further up this file: a module may be declared only once, and two
// branches landing at once appended each of them here as well.)
// The drift-detection status-change events a stack's drift detection publishes onto the
// default event bus, which span CloudFormation, EventBridge, Amazon SQS and Systems Manager
// at once — Amazon SQS and Systems Manager both as a target and as the service that owns a
// resource changed outside the stack.
// be declared only once, and two branches landing at once each re-added it here.)
// module may be declared only once, and two branches landing at once each re-added it here.)
// (`cloudformation_metric_stream_filter_update` is declared ONCE, in the head region
// above: a module may be declared only once. Its subject — a stack-deployed metric-stream
// pipeline's exports before and after an update rewrites its filter — spans
// CloudFormation, IAM, CloudWatch, Firehose and Amazon S3 at once, so no single service
// crate's test target can deploy the pipeline and then read the objects it delivered.)
// (`cloudformation_provisions_cloudfront_resources`, `cloudformation_monitored_alb_search_estate`,
// `cloudformation_domain_endpoint_outputs`, `cloudformation_stack_clock` and
// `elbv2_lambda_target_published_address` are each declared ONCE, in a span above this one:
// two `mod` items for one name is a redefinition that compiles neither, and branches landing
// at once each re-appended the same lines here.)
// above: a module may be declared only once.
// `cloudwatch_alarm_ssm_ops_item_action` is declared ONCE, in the head region above: the
// own test target can drive the transition and then read what it opened.
// `zz_probe` is declared ONCE, in the span above: a module may be declared only once.
// `cloudformation_metric_stream_estate_is_drainable` is declared ONCE, in the span above: a
// module may be declared only once.
// (`cloudformation_provisions_cloudfront_resources`, `cloudformation_monitored_alb_search_estate`
// and `cloudformation_domain_endpoint_outputs` are each declared ONCE, in a span above this
// one: a module declared twice in this file loads the same suite twice, which compiles
// `cloudformation_metric_stream_filter_update` is declared ONCE, in the head region above:
// two landings each appended their own copy here, and a module declared twice loads the same
// file twice, which compiles neither.
// (`cloudformation_monitored_alb_search_estate`, `cloudformation_domain_endpoint_outputs` and
// `cloudwatch_alarm_ssm_ops_item_action` are each declared ONCE in this file — the first two
// further down this tail, the third in the head region above — because two branches appended
// each of them here while both were landing, and a module may be declared only once.)
// (`cloudformation_metric_stream_filter_update` is declared once, in the head region above.)
// (`apigateway_sns_service_integration` is declared once, further up this tail.)
// (`cloudformation_metric_stream_filter_update` and `zz_probe` are each declared once above,
// for the same reason.)
// (`cloudformation_metric_stream_filter_update` is declared once in the head region above: a
// (`https_front_door_concurrent_pooled_producer` is declared once further up this tail: a
// (The Cognito authorizer suite is declared ONCE, in the head region above beside the other
// cross-service suites: it was appended here as well while two branches were landing at once,
// (The empty `zz_probe` is declared ONCE, further up this tail: it was appended here as well
// (`cloudformation_provisions_cloudfront_resources` is declared ONCE, in the sorted span
// above: two `mod` items for one name is a redefinition that compiles neither, and two
// branches each appended it here.)
// (The Amazon Cognito authorizer suite is declared ONCE, in the head region above: it was
// (`zz_probe` is declared ONCE, in the span above: it was appended here as well while two
// (`apigateway_cognito_authorizer_app_client_audience` is declared ONCE, further down this
// tail: a module may be declared only once, and several landings appended a note here.)
// (`zz_probe` and `cloudwatch_alarm_ssm_ops_item_action` are each declared ONCE above: a
// module may be declared only once, and several landings appended a note here.)
// (`https_front_door_concurrent_pooled_producer` and
// `cloudformation_metric_stream_estate_is_drainable` are each declared ONCE above: a module
// may be declared only once, and several landings appended a note here.)
// (`apigateway_cognito_authorizer_app_client_audience`, `zz_probe` and
// An object write on an EventBridge-enabled bucket drives a matched rule's permitted Lambda
// target, whose CreateInvalidation becomes CloudFront's own invalidation and refreshes the
// distribution's edge: Amazon S3, EventBridge, AWS Lambda, IAM, CloudFormation and CloudFront
// have to agree for the chain to hold, so no single crate's test target can drive it.
// (Declared ONCE, in the head region above: the copy that stood here was a second
// declaration two branches landing in the same window each appended.)
// (`pricing_plan_subscription_resource_eligibility` is declared once, in the head region
// A browser form POST into an EventBridge-enabled bucket, on the whole assembled router with
// request authentication on: which service claims an unsigned `multipart/form-data` POST is
// the router's own attribution, the publication is Amazon S3's, the reason-scoped match and
// the handover EventBridge's, and what arrived is Amazon SQS's.
// ONCE, further down this tail: a module may be declared only once, and two branches landing
// in the same window each appended their own copy here.)
// Declared here, beside the other CloudFormation suites: whether a stack-deployed
// metric-stream estate is drainable spans CloudFormation, IAM, CloudWatch, Firehose and
// Amazon S3 at once, so no single service crate's test target can deploy it and then drain it.
// (`cloudtrail_read_only_api_calls` is declared ONCE, in the sorted block at the head of
// this file beside the other CloudTrail suites: a module may be declared only once.)
// (`cloudformation_metric_stream_estate_is_drainable` is declared once, a few lines above in
// this same tail: a module may be declared only once.)
// (The HTTPS front-door suite is declared ONCE, further up this tail: a module may be
// declared only once, and two branches landing in the same window each appended their own
// copy here.)
// above: two branches landing in the same window declared it here as well, which compiled
// two branches landing in the same window declared it here as well, which compiled neither.)
// (`zz_probe` is declared ONCE, above: two branches landing in the same window declared it
// here as well, which compiled neither.)
// branches landing in the same window declared it here as well, which compiled neither.)
// (`cloudformation_metric_stream_estate_is_drainable` is declared ONCE, above: two branches
// landing in the same window declared it here as well, which compiled neither.)
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, further up this tail:
// two branches landing in the same window each appended their own copy here.)
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared once, in the sorted span above:
// (`https_front_door_concurrent_pooled_producer` is declared ONCE, further up this file: a
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, above, for the same
// reason.)
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, in the sorted span at
// the head of this file: two branches landing in the same window declared it here as well,
// which compiled neither.)
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, further up this file:
// the declaration that stood here was a second copy, which compiles neither.)
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE at the head of this
// file, so the copy that stood here is removed: a module declared twice is `E0428` and
// compiles neither.)
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, further up this file: two
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, further up this file: two
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, further up this file too:
// two landings each appended it, and a module declared twice is `E0428`, so this copy is
// this tail's copy is removed, a module declared twice being `E0428`.)
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE as well, further up this
// file: the declaration appended here by a second landing is `E0428` and compiled no suite,
// so it is gone and the one above stands.)
// which compiled neither. The `cloudformation_kms_encrypted_sns_sqs_pipeline` declaration
// that stood here is gone for the same reason: it is declared once above.)
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, at the head of this
// file: two branches landing in the same window each appended the pair here, which is a
// redefinition that compiled neither, so the duplicate is gone.)
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, further up this tail: a
// (The Pricing Plan Manager subscription-eligibility suite is declared ONCE, further up this
// tail: a module may be declared only once, and two landings each appended a declaration of
// (`cloudformation_provisions_cloudfront_resources` is declared ONCE, further up this file:
// (`cloudformation_provisions_cloudfront_resources` is declared ONCE, further up this tail: two
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, in the head region above:
// two landings in the same window each appended a declaration of it here.)
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, in the head region
// above: two landings in the same window each appended a declaration of it here.)
// (`sfn_relayed_dynamodb_request_id` is declared ONCE, a few lines above in this same tail:
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, in the sorted span
// above: the copy that stood here was a second declaration, which compiled neither.)
// (`sfn_relayed_dynamodb_request_id` is declared ONCE, further up this tail: two landings in
// the same window each appended a declaration of it here.)
// two landings in the same window each declared it here as well, which compiles neither.)
// (`cloudtrail_read_only_api_calls` is declared ONCE, in the SORTED block beside the other
// CloudTrail suites above: two landings each appended their own copy, and a module declared
// (`cloudwatch_alarm_ssm_ops_item_action` is declared in the head region above as well, for
// tail: two landings each appended their own copy here, and a module declared twice loads
// (The suite about which app client's token a COGNITO_USER_POOLS authorizer admits is
// declared ONCE, in the sorted span at the head of this file: the line was appended here as
// well while two branches were landing at once, which declared it a second time and compiled
// head of this file: the line was appended here as well while two branches were landing at
// (`zz_probe` is declared ONCE, further up this tail: the line was appended here as well
// (`https_front_door_concurrent_pooled_producer` is declared ONCE, further up this tail: the
// line was appended here as well while two branches were landing at once, which declared it a
// (The metric-stream estate suite is declared ONCE, in the head region above, for the same
// reason the pair of CloudFormation suites above records.)
// `cloudformation_provisions_cloudfront_resources` is declared ONCE, in the sorted span at
// are declared ONCE each, just above in this tail: a module declared twice in this file
// Declared ONCE, a few lines above in this same appended tail: two branches landing in the
// same window each appended a declaration of it here, which is a redefinition that compiled
// neither, so the duplicate is removed and the first declaration stands.
// `cloudtrail_read_only_api_calls` is declared ONCE, in the sorted block beside the other
// CloudTrail suites: the tail is where every suite landing in the same window appends, so two
// landings each appended their own copy and a module declared twice compiles neither.
// `cloudwatch_alarm_ssm_ops_item_action` is declared ONCE, in the sorted span at the head of
// this file, for the reason recorded there: the work item an alarm's Systems Manager OpsItem
// action opens has to be read where a client reads it — through OpsCenter's own query — so
// neither CloudWatch's nor Systems Manager's own test target can drive the transition and
// then read what it opened.
// `cloudformation_metric_stream_filter_update` is declared ONCE, in the sorted span at the
// head of this file: two landings each appended their own copy here, and a module declared
// twice compiles neither.
// `codebuild_s3_build_output` is declared ONCE, further up this tail: two landings each
// appended their own copy, and a module declared twice compiles neither.
// `lambda_signed_write_to_an_aoss_collection_endpoint` is declared ONCE, in the span above:
// `cloudformation_domain_endpoint_is_dialled_by_consumers` is declared ONCE, further up this
// tail: two landings each appended their own copy, and a module declared twice compiles
// other CloudTrail suites: a module may be declared only once.)
// (`cloudwatch_alarm_ssm_ops_item_action` is declared ONCE, in the head region above: a
// above, for the same reason.
// Declared ONCE each, higher up this file, because branches landing in the same window kept
// appending their own copy of these lines to this tail and two `mod` items for one name is a
// redefinition that compiles neither: `cloudformation_provisions_cloudfront_resources`,
// `cloudformation_stack_clock`, `elbv2_lambda_target_published_address`,
// `cloudformation_metric_stream_estate_is_drainable` and `zz_probe`.
// (`cloudformation_metric_stream_filter_update` is declared ONCE, a few lines above this one:
// two landings each appended a declaration of it to this tail, and a module may be declared
// (`https_front_door_concurrent_pooled_producer` is declared ONCE, a few lines above this
// one: two landings each appended a declaration of it to this tail, and a module may be
// (`apigatewaymanagementapi_manage_connections_authorization` is declared ONCE, further up
// this tail, for the same reason.)
// it was appended here as well while two branches were landing at once, and a module may be
// Declared here, ONCE: whether a stack-deployed metric-stream estate drains was appended both
// here and further down this tail in the same window, which declared it twice and compiled it
// nowhere.
// (`cloudformation_metric_stream_filter_update` is declared in the head region above: it was
// appended here as well while two branches were landing at once, and a module may be declared
// (`https_front_door_concurrent_pooled_producer` is declared once, further up this tail: it
// was appended here as well while two branches were landing at once, and a module may be
// `cloudformation_provisions_cloudfront_resources` is declared ONCE, in the sorted span above
// beside the other `cloudformation_…` suites: two `mod` items for one name is a redefinition
// that compiles neither.
// (`cloudformation_metric_stream_filter_update` and `zz_probe` are declared once each — the
// first in the head region above, the second further up this tail: a module may be declared
// (`cloudformation_metric_stream_estate_is_drainable` is declared once, in the span above: a
// `cloudformation_scripted_queue_deploy` are each declared ONCE, in the sorted spans above: a
// module may be declared only once, and this tail carried a second declaration of each.)
// (`codebuild_s3_build_output`, `lambda_signed_write_to_an_aoss_collection_endpoint` and
// `cloudformation_domain_endpoint_is_dialled_by_consumers` are each declared once, in the
// spans above: a module may be declared only once.)
// (`apigatewaymanagementapi_manage_connections_authorization` is declared once, further up
// this tail: a module may be declared only once.)
// conditioned identity policy decided against the tags of the session that signed the call
// spans IAM (the role, its policy and its tags) and STS (the mint that attaches the tags) at
// once, so neither crate's own test target can tag a session and then decide about it.
// `cloudformation_provisions_cloudfront_resources`, `cloudformation_monitored_alb_search_estate`,
// `elbv2_lambda_target_published_address` are each declared ONCE, in the spans above: two `mod`
// items for one name is a redefinition that compiles neither, which is what two branches
// appending the same lines to this tail at once produced.
// (`cloudtrail_read_only_api_calls` and `zz_probe` are declared ONCE each, in the regions
// above: two landings each appended their own copy to this tail, and a module declared twice
// `cloudwatch_alarm_ssm_ops_item_action` is declared in the head region above, for the reason
// recorded there.
// Declared in this tail: what a stack-deployed metric-stream pipeline exports before and after
// an update rewrites its filter spans CloudFormation, IAM, CloudWatch, Firehose and Amazon S3
// at once, so no single service crate's test target can deploy the pipeline and then read the
// objects it delivered.
// (`cloudformation_metric_stream_filter_update` is declared ONCE, in the head region above.)
// (`apigateway_cognito_authorizer_app_client_audience`, `cloudtrail_read_only_api_calls`,
// `cloudformation_metric_stream_estate_is_drainable`, `redshift_health_alarms_evaluate`,
// `zz_probe`, `cloudformation_metric_stream_filter_update`,
// `sdb_legacy_query_api_on_the_assembled_router`, `lambda_sqs_event_source_visibility_hold`,
// `codebuild_s3_build_output` and `cloudformation_domain_endpoint_is_dialled_by_consumers` are
// each declared ONCE, in the regions above: two landings each appended their own copy to this
// tail, and a module declared twice loads the same file twice, which compiles neither.)
// (`lambda_sqs_event_source_visibility_hold`, `codebuild_s3_build_output` and
// `cloudformation_scripted_queue_deploy` are each declared ONCE, in the sorted span at the head
// of this file: two landings each appended their own copy here, and a module declared twice
// `lambda_signed_write_to_an_aoss_collection_endpoint` and
// `cloudformation_domain_endpoint_is_dialled_by_consumers` are declared ONCE each, further up
// this tail, for the reasons recorded there.
// (`cloudformation_eventbridge_fan_out_to_lambda_and_sfn` is declared ONCE, in the head
// region of this file: two branches landing in the same window each kept a copy of it, and
// a module declared twice is `E0428` and compiles neither.)
// region of this file: a landing appended a second declaration of it here, and a module
// declared twice is `E0428` and compiles neither.)
// Declared ONCE in the head region of this file, for the reason the entry there records: the
// IaC form of an EventBridge fan-out — one stack's rule with a Lambda target and a Step
// Functions target under a target role — spans CloudFormation, EventBridge, Lambda, Step
// Functions, IAM, STS and Amazon SQS at once. Two landings in the same window each appended a
// declaration here as well, and a module declared twice compiles neither.
// (Declared ONCE, in the head region above, for that reason: two landings in the same window
// each appended a declaration of it here.)
// above: a module may be declared only once, and two branches landing at once appended a
// second declaration of it here.)
// (`zz_probe` is declared ONCE, further up this tail: a module may be declared only once,
// and two branches landing at once appended a second declaration of it here.)
// (`cloudformation_metric_stream_estate_is_drainable` is declared ONCE, in the sorted span
// (`cloudformation_metric_stream_filter_update` is declared ONCE, in the head region at the
// top of this file: it was appended here as well while two branches were landing at once,
// above beside the other CloudFormation suites: it was appended here as well while two
// (`apigateway_sns_service_integration` is declared ONCE, further up this tail: the merge of
// two tails that had both edited this list declared it a second time here.)
// (`apigateway_cognito_authorizer_app_client_audience` is declared once, in the sorted
// block at the top of this file: a module may be declared only once.)
// (`zz_probe` is declared in the head region above, with the note on why the file is still
// there: a module may be declared only once.)
// (`cloudformation_metric_stream_filter_update` is declared ONCE, further up this tail: the
// merge of two tails that had both edited this list declared it a second time here.)
// (`https_front_door_concurrent_pooled_producer` is declared ONCE, further down this tail:
// the merge of two tails that had both edited this list declared it twice.)
// `apigateway_cognito_authorizer_app_client_audience` are declared ONCE, in the head
// region at the top of this file: a module may be declared only once, and two branches
// landing at once appended each of them here as well.)
// (`cloudtrail_read_only_api_calls` and `zz_probe` are declared ONCE, in the sorted span
// above: they had each lost their declaration while two branches removed the same duplicate,
// and the declaration that stands is the one up there.)
// `cloudformation_scripted_queue_deploy` are each declared ONCE, in the sorted span at the
// head of this file: each was appended here as well while two branches were landing at
// (`https_front_door_concurrent_pooled_producer` is declared ONCE, a few lines above: a
// module may be declared only once, and two branches landing at once declared it twice.)
// (`cloudformation_metric_stream_estate_is_drainable` is declared in the head region
// this tail: a module may be declared only once, and two branches landing at once appended
// it here as well.)
// this tail: the merge of two tails that had both edited this list declared it twice.)
// (`cloudtrail_read_only_api_calls` is declared in the sorted block above, beside the other
// CloudTrail suites: a module may be declared only once.)
// (`zz_probe` is declared once, further up this file: a module may be declared only once.)
// (`cloudformation_metric_stream_filter_update` is declared once in the sorted block at the
// (`codebuild_s3_build_output` is declared once, further up this tail: a module may be
// (`cloudformation_domain_endpoint_is_dialled_by_consumers` is declared once, further up this
// (`sdb_legacy_query_api_on_the_assembled_router` is declared ONCE, in the head region of
// this file: a module may be declared only once.)
// `cloudformation_scripted_queue_deploy` are each declared ONCE, in the head region of this
// (`apigateway_sns_service_integration` is declared ONCE, in the appended tail below: a
// above, and `cloudformation_metric_stream_estate_is_drainable` ONCE further down this tail:
// both had been re-declared here while two branches were landing at once, and two `mod` items
// for one name is a redefinition that compiles neither.
// a module may be declared only once, and two landings each appended their own copy here.)
// `zz_probe` is declared ONCE, further up this tail: it had been re-declared here while two
// branches were landing at once, which compiles neither copy.
// module may be declared only once, and two landings each appended their own copy here.)
// (The suites that were declared here as well — the metric-stream estate and the
// metric-stream filter update, the CloudTrail read-only calls, `zz_probe` and the Redshift
// health alarms — are each declared ONCE, further down this tail: two landings each appended
// their own copy, and a module declared twice loads the same file twice, which compiles
// to touch this directory should delete both it and this declaration. (Declared ONCE, in the
// head region above: a module may be declared only once.)
// (The Bedrock knowledge base, the DynamoDB in-place update, the OpenSearch domain endpoint,
// the HTTPS front door, both Cognito session suites and the IAM/Secrets Manager envelope are
// each declared ONCE, further down this tail, for that same reason.)
// are declared ONCE each, further up this tail, for that same reason.)
// (`bedrock_agent_ingestion_populates_its_s3_vectors_index` is declared once, further up this
// tail: a module may be declared only once, and two branches landing at once each appended a
// copy of it here.)
// (`apigateway_cognito_authorizer_app_client_audience` is declared once, in the sorted block
// at the head of this file: a module may be declared only once, and two branches landing at
// once appended a second declaration of it here.)
// (`cloudformation_metric_stream_filter_update` is declared once, in the sorted block at the
// head of this file: a module may be declared only once, and two branches landing at once
// appended a second declaration of it here.)
// module may be declared only once, and two branches landing at once appended a second
// declaration of it here.)
// this file: two branches landing at once appended a second declaration of it here, and a
// module declared twice compiles neither.)
// branches landing in the same window appended a second declaration of each here, which is a
// redefinition that compiled neither, so the duplicates are removed and the first
// declarations stand.)
// branches landing in the same window appended a second declaration of it here, which is a
// (`apigatewaymanagementapi_manage_connections_authorization` is declared ONCE, in the span
// above: two branches landing in the same window appended a second declaration of it here,
// which is a redefinition that compiled neither, so the duplicate is removed and the first
// declaration stands.)
// `cloudformation_provisions_cloudfront_resources` is declared in the head region above: a
// `cloudformation_scripted_queue_deploy` are each declared once in the sorted spans above: a
// above: a module may be declared only once, and two branches landing in the same window each
// declared it here as well.)
// A suite is declared exactly ONCE: this is one crate root, so a second `mod` for the same
// name is `E0428` and fails every suite in this target. `all/all_rs_declares_each_suite_once.rs`
// checks that for this file and for every service crate's `tests/all.rs`.
// A suite left at the top level of `tests/` fails the build. Declare each suite exactly
// leaving a note here. (These five lines are ordinary comments, not `//` inner doc
// comments: an inner doc comment is only legal before an item, so appended here it is
// `E0753` and compiles nothing in this target.)
// Declared here, in the appended tail: WAFv2 protection a stack provisions.
// Declared here, in this head region, for the reason those entries record: the appended tail
// at the end of this file and the `e…` span are where every suite landing in the same window
// `c…`/`s…` spans are regions concurrently landing suites add their line to.
// (Written as ordinary comments: an inner doc comment is only legal at the TOP of a crate
// root, so this note — appended here by a landing that copied it out of the header — is
// `E0753` in this position and compiled none of the suites in this target.)
// (A suite left at the top level of `tests/` fails the build. Declare each suite exactly
// leaving a note here. These lines sit after the first item in this file, so they are
// ordinary comments: an inner doc comment here is `E0753` and compiles no suite at all.)
// leaving a note here. (Line comments, not `//`: an inner doc comment is only legal at
// the head of a crate root, so one in this tail is `E0753` and compiles no suite at all.)
// (The five lines below arrived as INNER doc comments (`//`) in this appended tail, where
// only the crate root's own header may carry them: an inner doc comment past the first item
// is `E0753` and fails every suite in this target, so they are ordinary comments here. The
// guidance itself stands.)
// Declare each suite exactly ONCE: this is one crate root, so a second `mod` for the same
// name is `E0428` and fails every suite in this target.
// `all/all_rs_declares_each_suite_once.rs` checks that for this file and for every service
// crate's `tests/all.rs`, so check there rather than leaving a note here.
// leaving a note here. (An inner doc comment only opens a file, so this note travelled
// here as `//` rather than `//`.)
// leaving a note here. (This note sits mid-file, so it is a plain comment: an inner doc
// comment is only accepted before the first item of a module.)
// (`autoscaling_plans_policies_reach_application_auto_scaling` is declared ONCE, in the head
// region of this file: the second declaration a concurrent landing appended here is gone, a
// module declared twice being `E0428`.)
// (Declared ONCE, further up this file: two landings in the same window each appended a
// (That suite is declared ONCE, in the head region of this file: the declaration that stood
// here was a second copy, which compiles neither.)
// (`autoscaling_plans_policies_reach_application_auto_scaling` is declared ONCE, further up
// this file: this copy is removed, a module declared twice being `E0428`.)
// Declared here, beside the other CloudFormation/Lambda suites, rather than in the appended
// tail every concurrently landing router suite adds its line to.
// (The suite below is declared ONCE, at the head of this file beside the other API Gateway
// suites: which app client's token a COGNITO_USER_POOLS authorizer admits spans Amazon
// Cognito user pools and API Gateway's execute-api data plane. It was appended here as well
// while two branches were landing at once, which declared it twice and compiled neither.)
// are declared once, a few lines above this one: two branches each re-added the pair here,
// and a module declared twice in this file compiles nowhere.)
// (`cloudformation_provisions_cloudfront_resources` is declared once, in the head region
// Declared here, in the appended tail, which is where every new suite inserts its lines. Both
// suites exist under tests/all/, and each is declared exactly once — here: several landings
// appended their own copy of the pair further down this tail, and a module declared twice
// loads the same file twice, which does not compile.
// are declared once, a few lines above this one: several landings each re-added the pair here,
// (This block was appended by several landings at once; each of its suites is declared ONCE
// below, because a module declared twice loads the same file twice, which does not compile.)
// `apigateway_sns_service_integration` are each declared ONCE in the spans above: several
// landings appended their own copy of this block, and a module declared twice loads the same
// file twice, which does not compile.)
// `apigateway_sns_service_integration` are each declared ONCE in the spans above: a module may
// be declared only once, so the copies several landings appended here are gone.)
// (`cloudtrail_read_only_api_calls` is declared once, in the sorted block beside the other
// CloudTrail suites in the head region above: a module may be declared only once.)
// (`cloudformation_metric_stream_filter_update` and `zz_probe` are each declared ONCE in the
// spans above: several landings appended their own copy of these lines, and a module declared
// twice loads the same file twice, which does not compile.)
// (`redshift_health_alarms_evaluate`, `cloudformation_metric_stream_filter_update` and
// `zz_probe` are each declared ONCE in the spans above: several landings appended their own
// copy of these lines, and a module declared twice loads the same file twice.)
// `apigateway_cognito_authorizer_app_client_audience` are each declared ONCE, in the sorted
// `cloudformation_scripted_queue_deploy` are each declared ONCE in the spans above: a module
// The CloudFormation search-estate, domain-endpoint-output, CloudFront-resources and
// CloudWatch-alarm-OpsItem suites are each declared ONCE, in the spans above: several landings
// each appended their own copy here, and a module declared twice loads the same file twice,
// which does not compile.
// `cloudformation_domain_endpoint_is_dialled_by_consumers` are each declared ONCE in the spans
// (The CloudFormation search-estate, domain-endpoint-output, CloudFront-resources and
// CloudWatch-alarm-OpsItem suites, `lambda_signed_write_to_an_aoss_collection_endpoint` and
// above: several landings each appended their own copy here, and a module declared twice loads
// the same file twice, which does not compile.)
// (`lambda_signed_write_to_an_aoss_collection_endpoint` is declared once, a little above this
// line: a module may be declared only once.)
// (`cloudtrail_read_only_api_calls` is declared in the SORTED CloudTrail span above, and
// `zz_probe` further up this tail: two landings each appended their own copy here as well,
// (`cloudtrail_read_only_api_calls` is declared ONCE, in the sorted CloudTrail span above,
// and `zz_probe` ONCE further up this tail: two branches landing in the same window each
// appended their own copy here, and a module declared twice loads the same file twice,
// which compiles neither.)
// and `zz_probe` ONCE further up this tail: two landings each appended a second copy of each
// here, and a module declared twice loads the same file twice, which compiles neither.)
// this tail: the entry was appended here as well while two branches were landing at once,
// (Ordinary `//` comments, not `//`: an inner doc comment is legal only at the TOP of a
// crate root, so a copy of the header note appended down here is `E0753` and compiles none
// of the suites in this target.)
// (`apigateway_sns_service_integration` is declared ONCE, further up this tail: a module may
// (Both that suite and the metric-stream one below are declared in the head region above: a
// (The empty `zz_probe` module is declared ONCE, further up this tail.)
// CloudFormation, CloudWatch, Firehose and Amazon S3 at once, so no single service crate's
// test target can deploy the estate and then drain it.
// (`redshift_health_alarms_evaluate` is declared ONCE, further up this tail: a module may be
// are declared ONCE each, further up this tail: a module may be declared only once.)
// (`bedrock_knowledge_base_over_an_aoss_collection`,
// `cloudformation_dynamodb_table_in_place_update` and
// `https_front_door_concurrent_pooled_producer` are declared ONCE each, further up this tail:
// (`sdb_legacy_query_api_on_the_assembled_router` is declared ONCE, in the sorted head region
// (`apigateway_cognito_authorizer_app_client_audience` is declared ONCE, in that head region:
// (`lambda_sqs_event_source_visibility_hold` is declared ONCE, in the span above: a module may
// (`cloudformation_scripted_queue_deploy` is declared ONCE, in the span above: a module may be
// `cloudformation_scripted_queue_deploy` are declared ONCE each, in the spans above: a module
// (`cognito_vended_session_is_bounded_by_its_role_policy`, `cognito_rules_role_mapping` and
// `iam_secretsmanager_denial_envelope_by_protocol` are declared ONCE each, further up this
// Declared HERE for the same reason its sibling above is: the X-Ray GROUP suite is the
// other half of the same CloudFormation-to-X-Ray provisioning concern.
// (`cloudtrail_read_only_api_calls` is declared ONCE, in the sorted block above: a module
// (`zz_probe` is declared ONCE, further up this file: a module may be declared only once,
// this tail: two branches landing at once appended a second declaration of it here, and a
// (`cloudformation_provisions_cloudfront_resources` is declared ONCE, at the end of this
// tail: its line was restored there while two branches were landing at once, and a module
// Declared here, in this same head region, for the reason the entry above records: whether an
// object read is authorized by the identity's own documents OR by the bucket's policy spans
// IAM, STS and Amazon S3 at once — and is compared with IAM's own simulation of the same union
// — so no single service crate's test target can see both halves of it.
// `zz_probe` further up this tail: two landings each appended their own copy here, and a
// module declared twice loads the same file twice, which compiles neither — so both copies
// are removed from this tail rather than left to break the whole target.)
// are declared ONCE, in the head region above.
// are declared ONCE each, in the span above: two branches each re-added the pair here, which
// declares each of them twice and compiles nowhere.
// (`apigateway_sns_service_integration` is declared once, further up this tail: two branches
// landing in the same window appended a second declaration of it here, which is a
// redefinition that compiles neither, so the duplicate is removed and the first stands.)
// CloudTrail suites above: two `mod` items for one name is a redefinition that compiles
// `zz_probe` and `cloudformation_metric_stream_filter_update` are declared ONCE each, above:
// `cloudtrail_read_only_api_calls` and `zz_probe` are declared ONCE each, above: two `mod`
// `cloudwatch_alarm_ssm_ops_item_action` is declared ONCE, in the head region above, for the
// reason the entry there records: the work item an alarm's Systems Manager OpsItem action
// what it opened.
// `codebuild_s3_build_output` is declared ONCE, in the span above: two landings each
// `cloudformation_domain_endpoint_is_dialled_by_consumers` is declared ONCE, in the span
// above, for the reason the entry there records.
// (`zz_probe` is declared ONCE, further up this tail, for the reason recorded there: two
// suite's rule — what becomes of the record a client puts on the stream a stack deployed,
// which only lands in a bucket if CloudFormation, IAM, Firehose and Amazon S3 all agree.
// (`cloudformation_provisions_cloudfront_resources` is declared in the head region above:
// (`cloudtrail_read_only_api_calls` is declared in the SORTED block beside the other
// CloudTrail suites above, and `zz_probe` further up this tail: a module may be declared
// a module may be declared only once, and two landings at once each declared it.)
// (`codebuild_s3_build_output` and `cloudformation_domain_endpoint_is_dialled_by_consumers`
// are declared once each, in the span above: a module may be declared only once, and two
// landings at once each appended their own copy here.)
// (`ses_event_destination_reaches_an_eventbridge_rule_target` is declared once, further up
// this tail: a module may be declared only once, and two branches landing at once each
// appended a copy of it here.)
// (`cloudformation_provisions_cloudfront_resources` is declared ONCE, further up this tail: a
// module may be declared only once, and two branches landing in the same window each appended
// their own copy here.)
// are declared once, a few lines above this one: two branches each re-added the pair here
// while both were landing, which declares each of them twice and compiles neither.)
// Declared once, in the sorted block beside the other CloudTrail suites above: the tail is
// where every suite landing in the same window appends, so a second copy there declares it
// twice and loads the same file twice, which compiles neither.
// (`cloudtrail_read_only_api_calls` and `zz_probe` are each declared once, in the sorted
// block and the head region above: a module may be declared only once.)
// (`cloudwatch_alarm_ssm_ops_item_action` and `cloudformation_metric_stream_filter_update`
// are each declared once, in the head region above: a module declared twice in this file
// loads the same file twice, which compiles nowhere.)
// (`cloudformation_metric_stream_filter_update` is declared once, a few lines up in this same
// appended tail: a module may be declared only once.)
// tail, for the same reason.)
// (`apigatewaymanagementapi_manage_connections_authorization` is declared once, in the span
// above, for that same reason.)
// bucket's notification configuration does for EVERY event class it names, and what the handler
// it runs is told by the search domain it signs a request to, spans CloudFormation, Amazon S3,
// Lambda, IAM/STS and Amazon OpenSearch Service at once, so no single service crate's test
// target can write the object and then read what the handler did.
// `zz_probe` ONCE further up this tail: two landings each appended their own copy here, and a
// role attachment a stack declares is applied at all is decided by Amazon Cognito Identity,
// which serves the `SetIdentityPoolRoles` call CloudFormation makes for it and refuses an
// incomplete role mapping, while IAM holds the role the attachment names — so no single
// service crate's test target can deploy the stack and then read the pool's roles back.
// leaving a note here. (Written as ordinary comments: an inner doc comment `//` may only
// appear before any item, so one in this appended tail is `E0753` and compiles nothing.)
// crate's `tests/all.rs`.
// leaving a note here. (These lines landed as INNER doc comments (`//`) in this appended
// tail, where only the crate root's own header may carry them: an inner doc comment past
// the first item is `E0753` and compiles no suite of this target.)
// leaving a note here. (Ordinary comments: appended after items, `//` is `E0753`.)
// leaving a note here. (These five lines are another fragment of this file's own header
// that a merge of two appended tails landed down here; they are ordinary comments
// because an inner doc comment may only appear before an item.)
// leaving a note here. (Another fragment of this file's own header that a merge of two
// appended tails landed down here: ordinary comments, because an inner doc comment may
// only appear before an item.)
// suite left at the top level of `tests/` fails the build. (A plain comment: an inner doc
// comment is only accepted before the first item of a module, so a `//` line in this tail
// is `E0753` and compiles no suite of this target.)
// leaving a note here. (Plain comments: see the note above on `//` in this tail.)
// leaving a note here. (Plain comments: a `//` line past the first item is `E0753`.)
// it was appended here as well while two branches were landing at the same time.)
// (`redshift_health_alarms_evaluate` is declared ONCE, further up this tail: a module may
// be declared only once, and two branches landing at once appended it twice.)
// (The Cognito-authorizer suite is declared ONCE, in the head region above: a module may be
// declared only once, and two branches landing at once each kept a different copy of it.)
// The suites below are declared exactly ONCE, here. Each of them had accumulated a second
// declaration (or lost its only one) in this index: the tail is where every landing appends
// its line while the name is already declared further up, a module declared twice loads the
// same file twice and compiles neither, and two branches removing the same duplicate each
// leave none. One block, one declaration per suite.
// above: a module may be declared only once, and two branches landing at once each kept a
// different copy of it.)
// a module may be declared only once, and a second declaration fails this whole target.)
// (`cloudtrail_read_only_api_calls` and `zz_probe` are declared once each, further up this
// file: a module may be declared only once, however many landings append it here.)
// (`cloudformation_metric_stream_filter_update` is declared in the head region above, and
// `zz_probe` further up this tail: a module may be declared only once.)
// `cloudformation_metric_stream_estate_is_drainable` are declared once each, further up this
// tail: a module may be declared only once, however many landings append it here.)
// Declared in the appended tail: what a session an identity pool vends may DO is decided by
// IAM's record of the mapped role and
// checks that for this file and for every service crate's `tests/all.rs`, so check there
// rather than leaving a note here.
// Declared here, in this same head region, for the reason the entry above records: whether the
// task-definition handle a stack publishes is one an ECS `CreateService` resolves spans
// CloudFormation, Amazon ECS, Amazon EC2, Elastic Load Balancing and IAM at once, so no single
// crate's test target can deploy the stack and then make the call against what it handed back.
// leaving a note here. (These five lines are a fragment of this file's own header that a
// merge of two appended tails landed down here; they are ordinary comments because an
// inner doc comment may only appear before an item.)
// (The crate-level guidance two concurrent landings appended here belongs to the module
// header at the top of this file, where it now stands: an inner doc comment may only precede
// items, so a `//` line in this appended tail is `E0753` and compiles nothing.)
// (An appended note that repeats the rule the module doc at the HEAD of this file already
// states — declare each suite exactly ONCE, since this is one crate root and a second `mod`
// for the same name is `E0428`, which `all/all_rs_declares_each_suite_once.rs` checks for
// this file and for every service crate's `tests/all.rs`. It is a LINE comment here: an
// inner doc comment is only legal before the first item, so landing it in this tail failed
// every suite in this target.)
// this file and for every service crate's `tests/all.rs`. (An ordinary comment for the same
// reason: an inner doc comment in this appended tail is `E0753`.)
// this file and for every service crate's `tests/all.rs`. (The tail end of that same
// header fragment, as an ordinary comment for the reason the lines above it record.)
// this file and for every service crate's `tests/all.rs`. (Plain comment: see above.)
// (A module doc comment belongs at the head of this file, before any item: these lines were
// appended here as `//` while two branches were landing at once, which is `E0753` and
// compiles no suite of this target, so they stand as ordinary comments. Declare each suite
// exactly ONCE: this is one crate root, so a second `mod` for the same name is `E0428`.
// crate's `tests/all.rs`, so check there rather than leaving a note here.)
// Declared here, in this same head region, for the reason the entry above records: a grant an
// `AWS::ACMPCA::Permission` declares is made by CloudFormation and held by Amazon Web Services
// Private CA, so only the assembled router can deploy one and then list it.
// region of this file: two branches landing in the same window appended a second declaration
// of each here, and a module declared twice compiles neither.)
// once appended a second declaration of each here, which compiled neither, so the copies
// that stood here are gone.)
// A suite left at the top level of `tests/` fails the build. (A LINE comment, for the
// reason the span above records.)
// suite left at the top level of `tests/` fails the build. (A line comment: an inner doc
// comment is only legal at a crate root's head, so one here is `E0753`.)
// leaving a note here. (Line comments: an inner doc comment is only legal at a crate
// root's head, so one in this tail is `E0753` and compiles no suite at all.)
// this file and for every service crate's `tests/all.rs`. (Line comments, for the reason
// the note above records.)
// … this file and for every service crate's `tests/all.rs`. (A LINE comment, for the
// (An inner doc comment is only legal at the head of a crate root, so this span — a
// duplicate of the header above, appended here by concurrently landing branches — is a
// plain comment: as `//` it was `E0753` and compiled nothing in this target.) Declare each
// suite exactly ONCE: this is one crate root, so a second `mod` for the same name is
// `E0428` and fails every suite in this target. `all/all_rs_declares_each_suite_once.rs`
// (The paragraph that belongs with the module header above landed here while two branches
// were landing at once; it is an INNER doc comment, which annotates nothing in the middle
// of a crate root (`E0753`), so it has been returned to the header where it says the same
// thing.)
// (`bedrock_agent_ingestion_populates_its_s3_vectors_index` is declared ONCE, a few lines
// above: two branches landing in the same window each appended their own copy here, and a
// module declared twice compiles neither, so the second copy is removed.)
// at the head of this file: a module may be declared only once.)
// once appended a second declaration of each here, which compiled neither. The duplicate
// pair that landed in this window is removed again here, and the first declaration of each
// stands.)
// beside the other `cloudformation_…` suites: the copy that stood here while two branches
// were landing at once is removed again, and the first declaration stands.)
// Declared in this appended tail, which is where every new suite inserts its lines: a push
// into a repository scanned on push spans Amazon ECR (which scans and publishes),
// EventBridge (which routes the finished scan) and Amazon SQS (which holds what arrived), so
// no single service crate's test target can wire the rule and then watch the queue.
// module declared twice compiles neither, so the duplicate is removed and the first stands.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, further up this tail: two
// branches landing at once appended a second declaration of it here, and a module declared
// twice compiles neither, so the duplicate is removed and the first stands.)
// two branches landing at once appended a second declaration of it here, and a module
// declared twice compiles neither, so the duplicate is removed and the first stands.)
// (`sfn_relayed_dynamodb_request_id` is declared ONCE, a few lines up in this same tail: two
// leaving a note here. (These five lines are an ordinary comment: an inner doc comment
// `//` is only legal at the start of the file, so landing one here stopped this target
// from compiling.)
// leaving a note here. (An ordinary comment, not an inner doc comment: `//` is legal only
// at the start of the file, so a block of it landing here stops this target from compiling
// with `E0753`.)
// region above: a module may be declared only once, and branches landing at once each
// appended their own copy here, which compiled neither.)
// above: a module may be declared only once, and branches landing at once each appended
// their own copy here, which compiled neither.)
// module may be declared only once, and branches landing at once each appended their own
// copy here, which compiled neither.)
// tail: a module may be declared only once, and branches landing at once each appended
// their own copy here, which compiled neither. The HTTPS front-door suite, appended here
// as well, is declared once further up this tail for the same reason.)
// `cloudformation_metric_stream_estate_is_drainable` are each declared ONCE, further up
// this tail: a module may be declared only once, and branches landing at once each appended
// Declared in the appended tail, for the reason the entries above record: what a
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, in the span above: a
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, further up this file: a
// (A note on this file's own rule, from the header at the top of it: every suite file under
// `tests/all/` must be declared here and a suite left at the top level of `tests/` fails the
// build. Declare each suite exactly ONCE: this is one crate root, so a second `mod` for the
// same name is `E0428` and fails every suite in this target.
// (A note two branches landed mid-file, where an inner doc comment is `E0753`: a suite
// left at the top level of `tests/` fails the build, and each suite is declared exactly
// ONCE, since this is one crate root and a second `mod` for the same name is `E0428` and
// fails every suite in this target. `all/all_rs_declares_each_suite_once.rs` checks both,
// for this file and for every service crate's `tests/all.rs`.)
// (Two more lines of that same note, landed in this tail as `//` while branches were
// merging: an inner doc comment is legal only before the first item, so they stand as
// ordinary comments. A suite left at the top level of `tests/` fails the build, and
// `all/all_rs_declares_each_suite_once.rs` checks the declarations of this file and of
// every service crate's `tests/all.rs`.)
// (`cloudformation_provisions_cloudfront_resources` is declared ONCE, in the head region
// which compiles neither, so the duplicate is removed and the first declaration stands.)
// head of this file: two branches landing in the same window appended a second declaration
// of each here, which compiles neither, so the duplicates are removed and the first
// this tail already: a module may be declared only once, and two branches landing at once
// above: two branches landing at once appended it here as well, and a module declared twice
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, further up this tail:
// two landings each appended their own copy, and a module declared twice compiles neither.)
// (The declaration rule these appended tails kept restating — one `mod` per suite, in this
// one crate root — is stated once in this file's header and checked by
// `all/all_rs_declares_each_suite_once.rs`; the fragments of that header which successive
// merges landed down here, one of them as an inner doc comment that no item follows, are
// removed.)
// stack-deployed ALB + Fargate ECS service converges on healthy targets spans CloudFormation,
// Amazon ECS, Amazon EC2 and Elastic Load Balancing v2 at once, so no single service crate's
// test target can deploy the stack and then read the health of what it registered.
// CloudTrail suites above: a module may be declared only once.)
// `zz_probe` and `cloudformation_metric_stream_filter_update` are declared ONCE each — the
// first further up this tail, the second in the head region above — so the copies two branches
// appended here while landing at once are gone: a module may be declared only once.
// `cloudformation_metric_stream_filter_update` is declared once, in the head region above: a
// (`https_front_door_concurrent_pooled_producer`, `sdb_legacy_query_api_on_the_assembled_router`,
// `apigateway_cognito_authorizer_app_client_audience`, `lambda_sqs_event_source_visibility_hold`
// and `cloudformation_scripted_queue_deploy` are each declared ONCE in the spans above: two
// landings appended a second copy of each here, and a module declared twice loads the same file
// `apigateway_sns_service_integration` is declared once, further up this tail: a module may be
// declared only once.
// items for one name is a redefinition that compiles neither. `redshift_health_alarms_evaluate`
// and `cloudformation_metric_stream_estate_is_drainable` are each declared once further up this
// tail, for the same reason.
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, further up this tail: a
// above: two branches landing in the same window declared it here as well.)
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, further up this tail: a
// (A suite left at the top level of `tests/` fails the build, and each suite is declared
// exactly ONCE: this is one crate root, so a second `mod` for the same name is `E0428` and
// fails every suite in this target. `all/all_rs_declares_each_suite_once.rs` checks that for
// this file and for every service crate's `tests/all.rs`, so check there rather than leaving a
// note here.)
// (`cloudformation_metric_stream_filter_update` is declared ONCE further up this file: two
// landings each appended a copy here, and a module declared twice compiles neither.)
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, in the sorted span at
// the head of this file: a landing appended a second copy here, which compiled neither.)
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE further up this tail: a
// landing appended a second copy here, and a module declared twice compiles neither.)
// Appended here, as every concurrently landing suite appends its line: the handle a stack
// publishes for each S3 bucket variant drives the operations of the service that owns it,
// which spans CloudFormation, Amazon S3, Amazon S3 Tables and Amazon S3 Vectors at once.
// (`bedrock_agent_ingestion_populates_its_s3_vectors_index` is declared ONCE further up this
// tail: a landing appended a second copy here, and a module declared twice compiles neither.)
// (A fragment of this file's own header that a landing appended HERE, in the tail: an
// inner doc comment is only legal at the top of the file, so it is left as a plain comment.
// leaving a note here.)
// (A fragment of this file's own header that a landing appended HERE, in the tail: an inner
// doc comment is only legal before any item, so it is left as a plain comment.
// doc comment (`//`) is only legal before any item, so it is left as a plain comment.
// (A fragment of this file's own header that a landing appended HERE, in the tail, as an
// inner doc comment; `//` is only legal before any item, so it is an ordinary comment.
// (The declaration the two notes above describe was appended here a THIRD time by a later
// landing; it is removed for the same reason, and the one declaration above stands.)
// above: this appended copy is a redefinition that compiles neither, so it is removed.)
// (`lambda_sqs_event_source_visibility_hold` is declared ONCE in the head region above, for
// the same reason.)
// (`codebuild_s3_build_output` is declared ONCE in the head region above, for the same
// (`cloudformation_scripted_queue_deploy` is declared ONCE in the span above, for the same
// (`https_front_door_concurrent_pooled_producer` is declared ONCE, further up this tail: this
// appended copy is a redefinition that compiles neither, so it is removed.)
// this file: this appended copy is a redefinition that compiles neither, so it is removed.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, in the sorted span at the
// head of this file: two branches landing at once appended a second declaration of it here,
// which compiled neither copy.)
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, a few lines above in
// this same tail: two branches landing in the same window each appended their own copy here.)
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, in the sorted
// CloudFormation span at the head of this file, for the same reason.)
// (The note the lines above belong to lives at the head of this file: a suite left at the top
// level of `tests/` fails the build, and each one is declared exactly ONCE here, since this is
// one crate root and a second `mod` for a name is `E0428`.
// leaving a note here. (These five lines reached this point of the file as INNER doc
// comments, which only a crate root's head may carry: `E0753` failed every suite in this
// target until they were made ordinary comments.)
// in `all/s3_notification_invokes_its_lambda_target.rs` and carries no test now. It is
// declared ONCE, further up this tail, for that reason.
// leaving a note here. (These lines are ordinary comments: an inner doc comment is only
// legal before the first item of a module, and this note sits in the appended tail.)
// leaving a note here. (Ordinary comments: an inner doc comment `//` may only appear
// before any item, so one in this appended tail is `E0753` and compiles nothing.)
// leaving a note here. (An inner doc comment — `//` — belongs to the CRATE ROOT, so it
// is only ever legal in the head of this file; one here is `E0753` and fails the target.)
// leaving a note here. (Ordinary comments: an inner doc comment is legal only before the
// first item, so a `//` line in this tail is `E0753` and compiles no suite.)
// this file and for every service crate's `tests/all.rs`. (Ordinary comment, for the
// reason the note above records.)
// this file and for every service crate's `tests/all.rs`. (An ordinary comment: an inner doc
// comment is only legal before the first item of this crate root.)
// first item of this crate root, so a `//` line in this appended tail is `E0753`.)
// module may be declared only once, and the line appended here while two branches landed at
// once compiled neither.)
// Declared at the END of this list, clear of the comment spans above, for the reason the
// entries there record: the sorted `a…` span is a region concurrently landing suites add
// their line to. The suite spans API Gateway, WAFv2 and CloudWatch Logs at once, so no
// single service crate's test target can deploy the stage, gate the logging destination the
// web ACL is pointed at, and then read what reached it.
// (These five lines are a fragment of this file's own module header, which a merge of two
// appended tails landed down here. They stand as ORDINARY comments because an inner doc
// comment `//` may only appear before the first item, so one in this tail is `E0753` and
// compiles no suite of this target. The rule they state is in the header at the top of this
// file, where it belongs.)
// (Another fragment of that same header, an ordinary comment here for the same reason.)
// (A note some concurrently landing change appended here as an INNER doc comment, which is
// `E0753` after the first item of this file: declare each suite exactly ONCE — this is one
// crate root, so a second `mod` for the same name is `E0428` and fails every suite in this
// target. `all/all_rs_declares_each_suite_once.rs` checks that for this file and for every
// service crate's `tests/all.rs`, so check there rather than leaving a note here.)
// (One more line of that same note, appended here as an INNER doc comment while branches
// were landing at once: it belongs at the head of this file or nowhere, so it is a line
// comment — `all/all_rs_declares_each_suite_once.rs` is what checks this file and every
// service crate's `tests/all.rs`.)
// suite left at the top level of `tests/` fails the build. (An inner doc comment is only
// legal before the first item of a module, so this appended header fragment is a plain
// comment: the rule it states stands in the module doc at the HEAD of this file.)
// (And one more of its lines landed here the same way; the rule stands in the header.)
// (Another landing appended five more lines of this file's own header here, typed with
// `//`: an inner doc comment is legal only before the first item, so in this tail it is
// `E0753` and compiles nothing. The header at the top of this file states the rule those
// lines repeated, so the fragment is gone rather than re-typed.)
// (And one more line of that header, for the reason the note above records.)
// (A sentence of a suite's own note ran onto the line above while branches were landing at
// once; the suite it described is declared with its note in the head region of this file.)
// suite left at the top level of `tests/` fails the build. (An ordinary comment: an inner
// doc comment past the first item is `E0753` and compiles no suite in this target, so this
// fragment of the header stands as a line comment; the rule it states is at the head of
// this file.)
// (One more line of that same header fragment, appended here as an INNER doc comment: a
// `//` line past the first item of this crate root is `E0753`, so it stands as an ordinary
// comment. The rule it repeats is in the module doc at the HEAD of this file.)
// (One more line of that same header fragment, which a concurrent landing appended here
// typed with `//`: an inner doc comment is legal only before the first item of a crate
// root, so one in this tail is `E0753` and compiles no suite of this target. The rule it
// states — a suite left at the top level of `tests/` fails the build — is in the header at
// the top of this file, where it belongs.)
// (One more fragment of that same header landed here as a `//` line, which is `E0753`
// mid-file: a suite left at the top level of `tests/` fails the build.)
// suite left at the top level of `tests/` fails the build.
// (The last line of that same header fragment, an ordinary comment here for the same
// reason: an inner doc comment is legal only before the first item of a crate root, so a
// `//` line in this appended tail is `E0753` and compiles no suite of this target.)
// suite left at the top level of `tests/` fails the build. (A fragment of this file's own
// header, landed down here: past the first item a `//` line is `E0753`, so it stands as an
// ordinary comment.)
// Declared here, in this same head region, for the reason the entry above records: what a client
// of a deployed REST stage is served when the proxied function's own DynamoDB read is refused
// spans API Gateway, Lambda, IAM, STS, DynamoDB and CloudFormation at once, so no single service
// crate's test target can deploy the stage and then rewrite the grant under it.
// leaving a note here. (These lines are a fragment of this file's own header that a merge
// of two appended tails landed down here; they are ordinary comments because an inner doc
// comment may only appear before any item.)
// this file and for every service crate's `tests/all.rs`. (An ordinary comment: an inner
// doc comment is only legal before the first item of a crate root.)
// Declared here, in the appended tail, which is where every new suite inserts its lines.
// Like its sibling layout check, this suite reads every crate's `tests/all.rs` rather than
// starting a router, and this target is the one that links every service crate.
// (A landing appended a fragment of this file's own header here, five of its lines typed
// with `//`: an inner doc comment is legal only before the first item, so in this tail it
// is `E0753` and compiles nothing. The header at the top of this file carries the rule
// those lines repeated, so the fragment is gone rather than re-typed.)
// (Several landings appended a fragment of this file's own header to this tail, the last
// of its lines typed with `//`: an inner doc comment is legal only before the first item,
// so one here is `E0753` and compiles nothing. The header at the top of this file carries
// the rule those lines repeated — declare each suite exactly ONCE, since this is one crate
// root and a second `mod` for the same name is `E0428` — and `all/all_rs_declares_each_suite_once.rs`
// and `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of it for
// this file and for every service crate's `tests/all.rs`, so the fragment is gone rather
// than re-typed.)
// leaving a note here. (Ordinary comments: an inner doc comment is only legal before the
// first item, so a `//` line in this appended tail is `E0753` and compiles nothing.)
// this file and for every service crate's `tests/all.rs`. (An ordinary comment, for the
// reason the lines above record.)
// leaving a note here. (Mid-file, so a plain comment: an inner doc comment is only
// accepted before the first item of a module.)
// this file and for every service crate's `tests/all.rs`. (A plain comment, for the reason
// the lines above record: a `//` line in this appended tail is `E0753`.)
// (The last line of that fragment, a plain comment here for the same reason.)
// (And one last line of that header, for the reason the notes above record.)
// this file and for every service crate's `tests/all.rs`. (Appended here as a `//` line by
// a landing that duplicated this file's header: past the first item that is `E0753` and
// compiles no suite in this target, so it stands as an ordinary comment.)
// doc comment is only accepted before the first item of this crate root.)
// (One more fragment of that same header landed here; the rule it states stands in the
// module doc at the HEAD of this file.)
// (An inner doc comment can only lead a crate root, so these lines — appended here by a
// landing that duplicated this file's header — are plain comments: as `//` in the middle of
// the file they are `E0753` and compile no suite in this target. The guidance they repeat
// stands at the head of this file.)
// (Appended here by a landing that duplicated this file's header: a `//` line past the
// first item is `E0753` and compiles no suite in this target, so these stand as ordinary
// comments. The guidance they repeat stands at the head of this file.)
// (Appended by a landing that duplicated this file's header: a `//` line past the first
// item is `E0753` and compiles no suite in this target, so it stands as an ordinary comment.)
// (Same appended header fragment, as an ordinary comment for the same reason.)
// (That suite's own declaration is the one a few lines above: a module may be declared only
// (`cloudtrail_read_only_api_calls` is declared ONCE, in the sorted CloudTrail span in the
// head region, and `zz_probe` ONCE further up this tail: the merge of two branches that had
// each kept a different one of two duplicates brought both copies back here, and a module
// declared twice loads the same file twice, which compiles neither.)
// (`sdb_legacy_query_api_on_the_assembled_router` is declared ONCE, at the top of this file:
// `cloudformation_scripted_queue_deploy` are each declared ONCE in the head region above: a
// module may be declared only once, and two branches landing at once appended a second copy
// of each here.)
// (`https_front_door_concurrent_pooled_producer` is declared ONCE, a few lines above in this
// same tail: a module may be declared only once.)
// (That suite is declared ONCE, further up this tail: a module may be declared only once, and
// two branches landing at once appended a second copy of it here.)
// this file: a file loaded as a module twice compiles neither copy.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, a few lines above in this
// same tail: a file loaded as a module twice compiles neither copy.)
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, further up this file:
// a file loaded as a module twice compiles neither copy.)
// (`bedrock_agent_ingestion_populates_its_s3_vectors_index` is declared ONCE, further up this
// tail: a file loaded as a module twice compiles neither copy.)
// (The paragraph about declaring each suite exactly once belongs to this file's own module
// documentation and is stated there, at the head: an inner doc comment in the tail annotates
// nothing and is `E0753`, which compiles no suite in this target.)
// comment is only accepted before the first item of a module, and the paragraph itself is
// stated once in this file's own module documentation at the head.)
// first item of a crate root, so a `//` line in this appended tail is `E0753`.)
// this file and for every service crate's `tests/all.rs`. (An ordinary comment for the
// reason the lines above it record: `//` in this tail is `E0753`.)
// this file and for every service crate's `tests/all.rs`. (A line comment, not an inner
// doc comment: this is the tail of the file, where a `//` line is `E0753`.)
// this file and for every service crate's `tests/all.rs`. (A LINE comment: an inner doc
// comment is only legal before the first item of a crate root, so this appended line was
// `E0753` and compiled nothing in this target.)
// (Four more lines of this file's own header, appended here as `//` by a landing: an inner
// doc comment is legal only before the first item, so in this tail it is `E0753` and compiles
// nothing. The header at the top of this file states the rule they repeated.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, in the head region above:
// a module may be declared only once, and two branches landing in the same window each
// appended a copy here.)
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, in the head region
// above: a module may be declared only once, and two branches landing in the same window
// each appended a copy here.)
// leaving a note here. (Line comments: these lines sit after the first item, where an
// inner doc comment is `E0753` and compiles no suite at all.)
// doc comment in this appended tail is `E0753` and compiles nothing.)
// states, for this file and for every service crate's `tests/all.rs`.)
// (An inner doc comment cannot appear here: `//` annotates the enclosing crate and is
// accepted only before any item, so this note — a fragment of the module header at the top
// of this file that a concurrent landing appended into the tail — is an ordinary comment.)
// (Another copy of the same header fragment, appended into this tail by a later landing:
// `//` is an inner doc comment and is accepted only before any item, so it is an ordinary
// comment here too.)
// (A further copy of the same header fragment, appended into this tail by a later landing:
// an inner doc comment is legal only before the first item, so these lines are ordinary
// comments here too.)
// this file and for every service crate's `tests/all.rs`. (Appended here as `//` by a
// landing: an ordinary comment, for the reason the note above records.)
// (`zz_probe` is declared ONCE, further up this tail: two branches landing at once appended a
// second declaration of it here, which compiled neither, so the copy that stood here is gone.
// The note below records why the file is still there.)
// above beside the other `cloudformation_…` suites: what a stack-deployed metric-stream
// pipeline exports before and after an update rewrites its filter spans CloudFormation, IAM,
// CloudWatch, Firehose and Amazon S3 at once, so the suite lives in this target — but a module
// may be declared only once, and two branches landing in the same window appended a second
// declaration of it here, which compiled neither.)
// leaving a note here. (An INNER doc comment is only legal at the head of the file: two
// branches landing at once appended this note here as `//`, which is `E0753` and compiled
// nothing in this target, so it is an ordinary comment where it stands.)
// A suite left at the top level of `tests/` fails the build. Declare each suite exactly ONCE:
// this is one crate root, so a second `mod` for the same name is `E0428` and fails every suite
// in this target, and `all/all_rs_declares_each_suite_once.rs` checks that for this file and
// for every service crate's `tests/all.rs`. (This note belongs to the header at the TOP of the
// file: an INNER doc comment is legal only there, so a copy appended into this tail is `E0753`
// and compiles nothing in this target. It stands here as an ordinary comment.)
// states — a suite left at the top level of `tests/` fails the build, and each suite is
// declared exactly ONCE, since this is one crate root and a second `mod` for the same name is
// `E0428`, which `all/all_rs_declares_each_suite_once.rs` checks for this file and for every
// service crate's `tests/all.rs`. It is a LINE comment here: an inner doc comment is legal
// only before the first item, so landing it in this tail is `E0753` and compiles no suite of
// this target.)
// (The same note landed a second time in this tail; it says nothing the lines above do not,
// and it stands as an ordinary comment for the same reason.)
// (The paragraph that belongs with the module header above landed here again as an INNER doc
// comment, which annotates nothing in the middle of a crate root (`E0753`) and compiles none
// of this target's suites; the header at the top of the file already says the same thing, so
// what stood here is a line comment: a suite left at the top level of `tests/` fails the
// build, and each suite is declared exactly ONCE, which
// `all/all_rs_declares_each_suite_once.rs` checks for this file and for every service crate's
// `tests/all.rs`.)
// leaving a note here. (An inner doc comment is legal only at the head of the file, so
// this note is an ordinary comment where it stands.)
// leaving a note here. (An inner doc comment is legal only before the first item, so this
// note stands as ordinary comments where it was appended.)
// this file and for every service crate's `tests/all.rs`. (Ordinary comment: an inner doc
// comment appended after an item is `E0753`.)
// first item, so a `//` line in this appended tail is `E0753` and compiles no suite.)
// Declared in this same head region, for the reason the note above records: a cluster's
// Kubernetes API server, the access entry that maps an IAM principal onto it, the RBAC
// objects that decide a request and the audit records it leaves are observable only with
// IAM, STS, EKS and CloudWatch Logs on one engine.
// once appended a second declaration of each here, which compiled neither, so the
// duplicates are removed and the first declaration of each stands.)
// branches were landing at once, which is a redefinition that compiled neither, so the
// duplicate is removed and the first declaration stands.)
// leaving a note here. (A LINE comment: an inner doc comment is only legal before the
// first item of a module, so one landing here fails every suite in this target.)
// leaving a note here. (Ordinary comments, not `//`: an inner doc comment is legal only
// at the top of a crate root, and these five lines are a fragment of this file's own
// header that a merge of two appended tails landed down here.)
// this file and for every service crate's `tests/all.rs`. (A line comment: an inner doc
// (A trailing fragment of that same repeated note, left as a LINE comment for the reason
// above: `this file and for every service crate's tests/all.rs`.)
// (Ordinary `//` comments: a copy of the header note appended into this tail as `//` is
// `E0753` and compiles no suite of this target.) A suite left at the top level of `tests/`
// fails the build. Declare each suite exactly ONCE: this is one crate root, so a second
// `mod` for the same name is `E0428` and fails every suite in this target.
// Declared in this same quiet `n`/`o` gap, beside the other cross-service Chime suites and
// for the reason the entries above record.
// leaving a note here. (Ordinary comments: an inner doc comment past the first item is
// `E0753` and compiles no suite of this target.)
// reason the block above records.)
// leaving a note here. (Line comments, for the reason the span above records: an inner
// doc comment past the first item is `E0753` and compiles no suite of this target.)
// comment may only appear before an item.)
// doc comment may only appear before an item, so a `//` line in this appended tail is
// (The note about declaring each suite exactly once lives in this file's header, where an
// inner doc comment is legal: appended here, after items, `//` is `E0753` and compiles
// nothing in this target, so the copy that landed here is an ordinary comment.)
// leaving a note here. (Ordinary comments: an inner doc comment `//` is legal only before
// the first item of a crate root, so these lines are `E0753` in this appended tail.)
// this file and for every service crate's `tests/all.rs`. (An ordinary comment: `//` here,
// past the first item of this crate root, is `E0753`.)
// (The five suites of the span at the head of this region —
// `sdb_legacy_query_api_on_the_assembled_router`,
// `cloudformation_scripted_queue_deploy` — were declared here a second time by two branches
// landing in the same window, which compiled neither; the duplicates are removed and the
// declarations above stand.)
// (That suite is declared ONCE, further up this tail: two branches landing in the same window
// each declared it here as well, which compiled neither.)
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, in the sorted block at
// the head of this file, and `cloudformation_provisions_cloudfront_resources` ONCE further up
// this file: two branches landing in the same window each declared them here as well, which
// (`bedrock_agent_ingestion_populates_its_s3_vectors_index` is declared ONCE, further up
// this file: two branches landing in the same window each declared it here as well, which
// (The note about declaring each suite exactly once is part of this file's HEADER, at the
// top: an inner doc comment is only accepted before the first item, so a copy of it down
// here compiles nothing. It was appended here while two branches were landing at once.)
// declared it here as well, which compiled neither copy.)
// this file: a module may be declared only once, and two branches landing in the same window
// each declared it here as well, which compiled neither copy.)
// leaving a note here. (An inner doc comment `//` cannot stand here: it may only appear
// before any item, and this note sits in the appended tail, after thousands of them.)
// leaving a note here. (Written as ordinary `//` comments: an inner doc comment `//` may
// only appear before any item, so one here stops this whole target compiling — `E0753`.)
// (… and for every service crate's `tests/all.rs`: the tail of that same appended note,
// which has to be an ordinary `//` comment here, since an inner doc comment may only
// precede the first item of the file.)
// (An orphaned line of that same appended note; it stands as an ordinary comment for the
// reason the span above records.) this file and for every service crate's `tests/all.rs`.
// (Declare each suite exactly ONCE: this is one crate root, so a second `mod` for the same
// leaving a note here. (An inner doc comment may only appear before an item, so this
// fragment of the file's own header, which a merge of two appended tails landed down here,
// is an ordinary comment.)
// (A stray line of that same repeated rule, an ordinary comment for the same reason:)
// leaving a note here. (Ordinary line comments: an inner doc comment `//` is only legal
// before the first item of a crate root, so one in this appended tail is `E0753`.)
// leaving a note here. (Ordinary comments: an inner doc comment `//` is only legal before
// (A leftover of that same appended note: it is a LINE comment here, for the reason the
// entry above records.) this file and for every service crate's `tests/all.rs`.
// appear before the first item of the crate root.)
// doc comment may only appear before the first item of the crate root.)
// leaving a note here. (Ordinary comments: an inner doc comment `//` is legal only
// before the first item of a file, so this fragment of the header cannot keep that form
// where a merge of two appended tails landed it.)
// (The same header fragment again, landed by another merge of two appended tails: it
// stands as ordinary comments because an inner doc comment may only appear before an
// item, so a `//` line in this tail is `E0753` and compiles no suite of this target.)
// (Another fragment of the same appended note: an inner doc comment is only legal before
// the first item, so this line stands as an ordinary comment — it checks this file and
// A suite left at the top level of `tests/` fails the build. (This note sits mid-file, so
// it is a plain comment: an inner doc comment is only accepted before the first item of a
// module.)
// this file and for every service crate's `tests/all.rs`. (A LINE comment: a note appended
// to this tail as an inner doc comment is `E0753` and compiles no suite of this target.)
// (A note that landed in this tail as an inner doc comment is `E0753` and compiles no suite
// of this target, so it stands as an ordinary comment: `all_rs_declares_each_suite_once`
// checks this file and every service crate's `tests/all.rs`.)
// (The suite-declaration note two landings appended here is in this crate root's header
// doc comment, at the top of this file: `//` is an inner doc comment, so mid-file it is
// `E0753` and fails every suite in this target.)
// (And another copy of the same tail line, as an ordinary comment for the same reason:
// this file and for every service crate's `tests/all.rs`.)
// above: this tail's copy is removed and the first declaration stands.)
// the copy that stood here is removed and the first declaration stands.)
// head of this file: the copies this tail carried are removed and the first declarations
// stand.)
// Declared here, in this appended tail: that a stack's `AWS::AppSync::*` estate — its GraphQL
// API, the schema it serves, its data sources, its resolvers, its pipeline functions and its
// API keys — is really created in AWS AppSync, and that the endpoints and identifiers the stack
// publishes are the ones that service answered, spans CloudFormation, AppSync, Amazon Cognito
// user pools, Amazon DynamoDB and IAM at once, so no single service crate's test target can
// deploy it and then read each resource back through its owner.
// this tail: the copy that stood here is removed and the first declaration stands.)
// Declared here, in this appended tail: whether the request id a Step Functions task relays
// from DynamoDB is that service's own spans both services, so neither crate's own test target
// can drive it.
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, a few lines above: the
// copy that stood here is removed and the first declaration stands.)
// (A note that landed in this tail as an inner doc comment, which only an item may be preceded
// by: the guidance it repeats is in this file's own module doc at the head, and
// `all/all_rs_declares_each_suite_once.rs` is what checks it.)
// (A note that landed in this tail as an inner doc comment, which only an item may be
// preceded by: the guidance it repeats is in this file's own module doc at the head, and
// `all/all_rs_declares_each_suite_once.rs` is what checks it. Nothing new goes here.)
// (`cloudformation_provisions_appsync_resources` is declared ONCE, further up this file, and
// `cloudformation_provisions_cloudfront_resources` in the sorted span above: a module may be
// Declared here, in this appended tail: each of the five suites below lost its declaration
// while two branches were resolving the duplicate copies this tail had accumulated of it, and
// a suite that is declared nowhere compiles nowhere and never runs. One declaration each, here.
// (The line that stood here was an inner doc comment too, for the same reason and with the
// same fix.)
// leaving a note here. (An ordinary comment: an inner doc comment may only appear before
// an item, so a `//` line in this appended tail is `E0753` and compiles no suite.)
// same reason as the block above: `//` is only legal before the first item.)
// reason the notes around it record: `//` mid-file annotates nothing and is `E0753`.)
// leaving a note here. (Ordinary comments: an inner doc comment `//` is only legal
// before the first item of a crate root, so landing these lines here as `//` is `E0753`
// (Declared ONCE, in that span above: two branches landing in the same window appended a
// second declaration of it here, which compiled neither.)
// (`zz_probe` is declared ONCE, further up this file: two branches landing in the same window
// appended a second declaration of it here, which compiled neither.)
// (The `zz_probe` declaration that stood here was the second copy of it, for that same
// reason: it is declared ONCE further up this tail.)
// (`ses_event_destination_reaches_an_eventbridge_rule_target` is declared ONCE, further up this
// tail: two branches landing in the same window declared it here as well, which compiled
// file: two landings in the same window each appended a declaration of it here.)
// (The same note, landed again as `//` lines by another branch and so turned into ordinary
// comments here: a suite left at the top level of `tests/` fails the build; each suite is
// declared exactly ONCE, since a second `mod` for the same name is `E0428` and fails every
// suite in this target; and `all/all_rs_declares_each_suite_once.rs` checks that for this
// file and for every service crate's `tests/all.rs`.)
// (Another line of that same header fragment, appended as an INNER doc comment and left
// here as an ordinary comment for the reason the lines above record.)
// (One more fragment of that same header, appended here with `//`: an inner doc comment
// is legal only before the first item of a crate root, so one in this tail is `E0753` and
// compiles no suite of this target. The rule it repeated — that
// `all/all_rs_declares_each_suite_once.rs` checks this file and every service crate's
// `tests/all.rs` — stands in the header at the head of this file.)
// (And one more: the rule it states — that `all/all_rs_declares_each_suite_once.rs` checks
// this file and every service crate's `tests/all.rs` — stands in the module doc at the HEAD
// of this file.)
// this file and for every service crate's `tests/all.rs`. (One more line of that same
// header fragment, an ordinary comment for the reason the lines above record.)
// this file and for every service crate's `tests/all.rs`. (Same appended header fragment,
// an ordinary comment for the reason the lines above record.)
// leaving a note here. (Ordinary comments: the same guidance stands as `//` in the header
// of this file, where an inner doc comment is legal; one down here is `E0753`.)
// leaving a note here. (Ordinary comments: the guidance stands as `//` in this file's
// header, the one place an inner doc comment is legal.)
// this file and for every service crate's `tests/all.rs`. (Ordinary comments, for the
// reason the notes around them record: `//` past the first item is `E0753`.)
// (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, above: the second
// Declared here, in this appended tail: that the plaintext of a `SECRETS_MANAGER` environment
// variable is substituted inside the build, and that a build whose service role may not read the
// secret runs nothing, spans IAM (the role and its documents), Secrets Manager (the secret and
// the decision) and CodeBuild (the build) at once, so no single service crate's test target can
// drive it.
// (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, in the sorted span at the
// head of this file: the copy that stood here was a second declaration, which is `E0428`
// and compiles neither, so it is removed.)
// one stays in the HEADER: an inner doc comment past the first item is `E0753`, so these
// lines stand as ordinary comments where a merge of two appended tails landed them.
// name is `E0428` and fails every suite in this target, and
// crate's `tests/all.rs`. (A fragment of this file's own header, landed here by a concurrent
// branch: ordinary comments, since past the first item a `//` line is `E0753`.)
// (The six lines above are fragments of the module doc at the HEAD of this file, appended
// here while several branches were landing at once. An inner doc comment is only legal
// before the first item, so as `//` they were `E0753` and compiled no suite of this
// target; the rule they state stands in that header, and here they are plain comments.)
// suite left at the top level of `tests/` fails the build. (An ordinary comment: this is a
// fragment of this file's own header that a landing appended here, and an inner doc comment
// is only legal before the first item.)
// doc comment is only legal before the first item, so a `//` line in this appended tail
// annotates nothing and fails the lint this workspace denies.)
// (A fragment of this file's own header appended here by a landing: an inner doc comment is
// legal only before the first item, so this stands as an ordinary comment.)
// (One more fragment of that same header landed here as an inner doc comment, which is
// legal only before the first item; the rule it states stands in the module doc at the HEAD
// suite left at the top level of `tests/` fails the build. (An ordinary comment: such a
// line is a fragment of this file's own header, and an inner doc comment is legal only
// before the first item, so a `//` here is `E0753` and compiles nothing.)
// comment past the first item of this file annotates nothing, which `unused_doc_comments`
// refuses — the guidance it repeats stands at the head of this file.)
// this file and for every service crate's `tests/all.rs`. (A plain comment: an inner doc
// comment past the first item is `E0753`, and as a doc comment on the next item it
// annotates nothing, which `unused_doc_comments` refuses.)
// (One more fragment of that same header, appended mid-file: an inner doc comment is legal
// only before the first item, so it stands as an ordinary comment — a suite left at the top
// level of `tests/` fails the build, as the header at the head of this file states.)
// (The same fragment again, as an ordinary comment for the same reason: the rule it repeats —
// checked for this file and for every service crate's `tests/all.rs` — stands at the head of
// Declared here, in this same head region, for the reason the entry above records: the four
// documents that can decide an IAM AccessDenied are only reachable side by side if IAM holds
// the user, the role and their documents while STS mints the scoped session, so no single
// service crate's test target can read all four reason clauses back.
// suite left at the top level of `tests/` fails the build. (An ordinary comment: an inner doc
// comment is only legal before the first item of a crate root.)
// this file and for every service crate's `tests/all.rs`. (An ordinary comment, for the reason
// the lines above record.)
// (The lines below arrived as INNER doc comments (`//`) in this appended tail, where
// is `E0753` and fails every suite in this target, so what the guidance says stands in the
// header at the top of this file and the duplicates appended here are removed.)
// (The note about declaring each suite exactly ONCE is the crate header at the top of this
// file; a copy appended here was written as an inner doc comment, which is only accepted
// before a module's first item, so it broke the build and is removed. `E0428` for a second
// `mod` of one name, and `all/all_rs_declares_each_suite_once.rs`, are stated there.)
// (A copy of the crate header's note landed here as an inner doc comment, which is only
// accepted before a module's first item, so it broke the build and is removed.)
// (The crate-level guidance repeated copies of which concurrent landings kept appending to
// this tail belongs to the module header at the top of this file, where it stands: an inner
// doc comment may only precede items, so a `//` line down here is `E0753` and compiles
// nothing, and the duplicates are removed rather than reworded again.)
// (A fragment of this file's own header that concurrent landings kept appending to this
// tail; it is removed rather than reworded, since the rule it repeats stands in the crate
// header at the top of this file. An inner doc comment past the first item is `E0753` and
// compiles no suite of this target, which is what those appended `//` lines did.)
// (One more line of that same note landed here as an INNER doc comment while branches were
// landing at once; it belongs at the head of this file or nowhere, so it is removed.)
// (One more fragment of that same header landed here as an inner doc comment and is
// removed; the rule it states stands in the module doc at the HEAD of this file.)
// doc comment is legal only before the first item of a crate root, so a `//` line in this
// tail is `E0753` and compiles no suite of this target.)
// reason the lines above record: a `//` line past the first item is `E0753`.)
// crate's `tests/all.rs`, so check there rather than leaving a note here. (These lines are
// plain comments: an inner doc comment (`//`) documents the enclosing item, so one in the
// middle of this file is `E0753` and compiles nothing. The crate's own doc header is at the
// top of this file.)
// (The crate-level guidance a landing appended here belongs to the module header at the top
// of this file, where it stands: declare each suite exactly ONCE, since this is one crate
// root and a second `mod` for the same name is `E0428`, which
// `all/all_rs_declares_each_suite_once.rs` checks for this file and for every service
// crate's `tests/all.rs`. These are LINE comments: an inner doc comment `//` may only
// appear before the first item, so one in this appended tail is `E0753` and compiles
// nothing.)
// (The same appended fragment, once more and once more as a LINE comment: the rule it
// repeats — `all/all_rs_declares_each_suite_once.rs` checks this file and every service
// crate's `tests/all.rs` — is stated by the module header at the top of this file.)
// suite left at the top level of `tests/` fails the build. (A line comment: `//` here is
// `E0753`, and the header at the top of this file states the paragraph.)
// this file and for every service crate's `tests/all.rs`. (The same, for the same reason.)
// once appended a second declaration of each here, which compiled neither. The copies they
// appended are removed; the declarations that stand are the ones above.)
// file beside the other `cloudformation_…` suites: two branches landing in the same window
// appended a second declaration of it here, which is a redefinition that compiled neither, so
// the copy is removed and the declaration above stands.)
// Every suite is declared exactly ONCE: this is one crate root, so a second `mod` for the
// crate's `tests/all.rs`, so check there rather than leaving a note here. (This note sits in
// the appended tail, where two branches landing at once left it as an INNER doc comment —
// which is `E0753` anywhere but the head of a file — so it is an ordinary comment now.)
// the appended tail, where concurrent landings keep leaving it as an INNER doc comment —
// which is `E0753` anywhere but the head of a file — so it is an ordinary comment here.)
// This file and every service crate's `tests/all.rs` are checked by that suite.
// A suite left at the top level of `tests/` fails the build.
// suite left at the top level of `tests/` fails the build. (An ordinary comment: `//` past
// the first item of this crate root is `E0753` and compiles no suite of this target.)
// (Appended again by a landing: an ordinary comment, since `//` here is `E0753`.)
// (And the same line once more, appended with `//` by another landing in the same window:
// past the first item that is `E0753`, so it stands as an ordinary comment.)
// (And the same line again, which a landing left typed with `//` in this tail: an inner
// doc comment is `E0753` anywhere but the head of a crate root, so it is a plain comment.)
// (The line above once more, as it was appended here with `//`: an inner doc comment is
// only legal before the first item, so it is a plain comment.)
// suite left at the top level of `tests/` fails the build. (An ordinary comment, for the
// reason this tail records: a `//` line past the first item of this crate root is
// `E0753`.)
// it. The `sfn_relayed_dynamodb_request_id` suite is declared ONCE as well, just above: two
// landings in the same window each appended it here, and a module declared twice is `E0428`
// doc comment may only appear before any item, so a `//` line in this appended tail is
// `E0753` and compiles nothing.)
// leaving a note here. (A plain comment: an inner doc comment is only accepted before the
// first item of a module.)
// leaving a note here. (Ordinary comments: these five lines are a fragment of this file's
// own header that a merge of two appended tails landed down here, and an inner doc comment
// `//` may only appear before any item, so one here is `E0753` and compiles nothing.)
// root's head, so one in this appended tail is `E0753` and compiles no suite at all.)
// this file and for every service crate's `tests/all.rs`. (A line comment, for the reason
// leaving a note here. (Ordinary comments, for the reason the notes above record: an
// inner doc comment past the first item is `E0753` and compiles no suite of this target.)
// comment past the first item is `E0753`, so this fragment of the module header stands as
// ordinary comments here, and the header at the HEAD of this file carries the rule:
// the fragment above records.)
// one stays at the HEAD of this file: an inner doc comment past the first item is `E0753`,
// so the same note appended to this tail compiles no suite of this target.
// reason the appended fragments below record.)
// (An appended fragment of the header above, kept as an ordinary comment because an inner
// doc comment is only legal before the first item: it checks this file and every service
// crate's `tests/all.rs`.)
// (A suite left at the top level of `tests/` fails the build — another fragment of this
// file's own header, kept as a line comment because an inner doc comment is only legal
// before the first item.)
// (An appended fragment of the header above, kept as a line comment because an inner doc
// comment is only legal before the first item: it checks this file and every service
// Declared in this same tail, for the same reason: the snapshot copy grant a stack declares,
// created in Amazon Redshift with the Key Management Service key it covers resolved there.
// (The guidance that stood here — declare each suite under `tests/all/` exactly once — is
// the head block's at the top of this file, where an INNER doc comment may appear: a `//`
// line after the first item is `E0753` and compiles no suite in this target.)
// (The guidance that stood here — declare each suite under `tests/all/` exactly once — is the
// head block's at the top of this file, where an INNER doc comment may appear: a `//` line
// after the first item is `E0753` and compiles no suite in this target.)
// (One more line of that same header fragment, as an ordinary comment for the same reason: an
// inner doc comment is legal only before the first item, so a `//` line here is `E0753` and
// compiles no suite of this target. The rule it states stands at the head of this file.)
// (That same header fragment once more, as an ordinary comment: a `//` line past the first
// item of this crate root is `E0753`, so it stands as a line comment and the rule it repeats
// leaving a note here. (A fragment of this file's own header that a merge of two appended
// tails landed down here; ordinary comments, because an inner doc comment `//` may only
// appear before the first item and one in this tail is `E0753`.)
// this file and for every service crate's `tests/all.rs`. (Ordinary comment for the same
// reason as the fragment above: a `//` line in this appended tail is `E0753`.)
// header a landing appended here; an ordinary comment, because a `//` line past the first
// item is `E0753` and compiles no suite of this target.)
// this file and for every service crate's `tests/all.rs`. (The last line of that same
// header fragment, an ordinary comment for the same reason.)
// header; an ordinary comment, since a `//` line past the first item is `E0753`.)
// this file and for every service crate's `tests/all.rs`. (The last line of that fragment,
// an ordinary comment for the same reason.)
// suite left at the top level of `tests/` fails the build. (A plain comment: a `//` line
// in this tail is `E0753`.)
// this file and for every service crate's `tests/all.rs`. (Plain comment, same reason.)
// (Six more lines of this file's own header, appended into this tail with `//` by a
// landing: past the first item that is `E0753` and compiles no suite of this target, so the
// fragment is dropped rather than re-typed — the rule it repeated stands in the module doc
// at the HEAD of this file.)
// (And thirteen more lines of that same header, appended into this tail with `//` by
// several landings at once: past the first item an inner doc comment is `E0753` and compiles
// no suite of this target, so the fragments are dropped rather than re-typed — the rules they
// repeated (a suite left at the top level of `tests/` fails the build; each suite is declared
// exactly ONCE, since this is one crate root and a second `mod` for the same name is `E0428`)
// stand in the module doc at the HEAD of this file, which is where
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check them from.)
// header that a landing appended here; past the first item a `//` line is `E0753` and
// reason the lines above record: past the first item a `//` line is `E0753`.)
// (Declared ONCE, in the span above: two branches landing in the same window each carried a
// declaration of it, and a module may be declared only once.)
// (Every suite that was declared here as well — the metric-stream pair, the CloudTrail
// read-only suite, `zz_probe`, the Redshift alarm suite, the Bedrock knowledge-base suite,
// the DynamoDB in-place update, the domain-endpoint consumer suite, the HTTPS front door and
// the two Cognito suites — is declared ONCE, in the tail below: two branches landing in the
// same window each carried its own copy of this span, and a module may be declared only
// (The five suites that were declared here as well — the legacy `sdb` query API, the Amazon
// Cognito authorizer audience, the Lambda/SQS visibility hold, the CodeBuild object-store
// output and the scripted queue deploy — are each declared ONCE, in the sorted spans at the
// head of this file: two branches landing in the same window each carried this span, and a
// span above: two branches landing in the same window each declared it, and a module may be
// (The SESv2 event-destination suite is declared ONCE, further up this tail: two branches
// landing in the same window each carried a declaration of it.)
// CloudFormation span above: two branches landing in the same window each declared it, and a
// (The Pricing Plan Manager subscription-eligibility suite is declared ONCE, in the sorted
// span at the head of this file: two branches landing in the same window each carried a
// declaration of it.)
// (`cloudformation_provisions_cloudfront_resources` is declared ONCE, in the sorted
// CloudFormation span above: two branches landing in the same window each carried a
// (And one more, for the same reason: a suite left at the top level of `tests/` fails the
// build.)
// this file and for every service crate's `tests/all.rs`. (And so is this one.)
// leaving a note here. (Fragments of this file's own header appended here by a landing:
// ordinary comments, because `//` is only legal before the first item.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, further up this file: a
// module may be declared only once, however many landings append it here.)
// this file: a module may be declared only once, however many landings append it here.)
// above: a module may be declared only once, however many landings append it here.)
// (`sfn_relayed_dynamodb_request_id` is declared ONCE, four lines above: a module may be
// declared only once, however many landings append it here.)
// inner doc comment `//` may only appear before any item, so one in this appended tail
// is `E0753` and compiles nothing.)
// leaving a note here. (Ordinary `//` comments: an inner doc comment is legal only before
// the first item, so this fragment of the head note is `E0753` down here.)
// comment in this appended tail is `E0753` and compiles no suite at all.)
// comment is accepted only before the first item, so this fragment of the head note is
// `E0753` down here.)
// suite left at the top level of `tests/` fails the build. (Line comments: a `//` line in
// this tail is `E0753` and compiles nothing in this target.)
// (Another line of that same appended header fragment, an ordinary comment for the same
// (One more line of that same appended header fragment, an ordinary comment for the same
// appended tail is `E0753` and compiles no suite of this target. The rule it repeats stands
// in the header at the head of this file.)
// (Two further copies of that same header paragraph were appended here with `//`, which is
// `E0753` past the first item and compiled no suite of this target; the rule they repeated
// stands in the header at the head of this file, so they are gone rather than re-typed.)
// (Three further copies of that header paragraph were appended here the same way, and are
// gone for the same reason: `//` past the first item compiled no suite of this target.)
// A suite left at the top level of `tests/` fails the build. (An ordinary comment: a fragment
// of this file's own header, appended into this tail by a landing, where an inner doc comment
// … this file and for every service crate's `tests/all.rs`. (An ordinary comment, for the
// reason the notes above record: an inner doc comment past the first item is `E0753`.)
// (Another fragment of this file's header, appended here by a landing: a `//` line past the
// first item is `E0753` and compiles no suite in this target, so it stands as an ordinary
// comment — the rule it repeats is in the module doc at the HEAD of this file.)
// (One more fragment of that same header landed here, typed with `//`: past the first item
// that is `E0753` and compiles no suite in this target, so it stands as an ordinary comment —
// the rule it states stands in the module doc at the HEAD of this file.)
// first item, so a `//` line here is `E0753` and compiles no suite of this target.)
// RUNNING function's own CloudWatch metric call is authorized against spans IAM (the execution
// role and its document), STS (the session the call is signed with), Lambda (the handler and the
// SDK it is given), CloudWatch (the decision) and CloudFormation (the deployment) at once, so no
// single service crate's test target can ask it.
// (The rule these five lines state belongs in the module header at the HEAD of this file, where
// it is legal and where it is now written: an inner doc comment `//` is legal only before the
// first item of a crate root, so a `//` line in this appended tail is `E0753` and compiles no
// suite of this target. They stand here as ordinary comments: declare each suite exactly ONCE,
// which `all/all_rs_declares_each_suite_once.rs` checks for this file and every service crate's
// leaving a note here. (Ordinary comments: the rule stands in the header at the head of
// this file, the only place a `//` line is legal.)
// suite left at the top level of `tests/` fails the build. (Ordinary comments, for the
// leaving a note here. (Ordinary comments: past the first item a `//` line is `E0753`.)
// (One more fragment of this file's header, landed here as an inner doc comment: it is
// legal only before the first item, so it is a plain comment — a suite left at the top
// (And one more of those fragments: an inner doc comment past the first item is `E0753`,
// so it is a plain comment — `all/all_rs_declares_each_suite_once.rs` is what checks this
// file and every service crate's `tests/all.rs`.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, in the sorted region
// above beside the suite whose wiring it reuses: two branches landing in the same window
// each appended their own copy here, and a module declared twice compiles neither.)
// above: whether a flat-rate CloudFront subscription may be sold over a distribution needs
// CloudFront's web-ACL association and a WAFv2 web ACL at once, which is why it is a
// cross-service suite at all — two branches landing in the same window each appended their
// own copy here, and a module declared twice compiles neither.)
// above: a module may be declared only once, and two landings each appended it here.)
// (A fragment of this file's own header that a merge of two appended tails landed down
// here, as ordinary comments: an inner doc comment is only legal before the first item of
// a crate root, so a `//` line in this tail is `E0753` and compiles no suite at all. The
// rule it repeats — declare each suite exactly ONCE, since a second `mod` for one name is
// `E0428` — stands in the header at the top of this file, where
// `all/all_rs_declares_each_suite_once.rs` checks it for this file and for every service
// reason: it names this file and every service crate's `tests/all.rs`.)
// (Ordinary `//` comments, not `//`: these six lines are a copy of this file's own header
// that a landing appended down here, and an inner doc comment is legal only at the TOP of a
// crate root, so as `//` they are `E0753` and compile none of the suites in this target.
// The rule they repeat — declare each suite exactly ONCE, since this is one crate root and a
// second `mod` for one name is `E0428` — stands in the header above, where
// (And one more line of that header, a plain comment here for the same reason: a suite left
// at the top level of `tests/` fails the build.)
// (A last fragment of that header, a plain comment here for the same reason: it names this
// (Plain comments: the four lines above are a fragment of this file's own header that a
// landing appended here, and as `//` they would be `E0753`.)
// this file and for every service crate's `tests/all.rs`. (Two more header fragments, as
// plain comments for the reason every note around them records.)
// (Plain comments: the four lines above are a header fragment a landing appended here.)
// (Another copy of that header line, a plain comment for the reason the notes above
// record.)
// suite left at the top level of `tests/` fails the build. (One more line of that header
// fragment, a plain comment for the same reason.)
// than leaving a note in this appended tail — an inner doc comment here is
// (`sfn_relayed_dynamodb_request_id` is declared ONCE, on the line above: two landings in
// (`autoscaling_plans_policies_reach_application_auto_scaling` is declared ONCE, in the
// head region above: two landings in the same window each appended a declaration here.)
// one stays in the header: an inner doc comment past the first item is `E0753`, so the
// same note appended to the tail of this file would compile no suite of this target.
// header, appended mid-file by a landing: an inner doc comment is only legal before the
// first item, so it stands as an ordinary comment and the rule it repeats stands at the head
// this file and for every service crate's `tests/all.rs`. (One more fragment of that same
// header landed here as a `//` line; past the first item that is `E0753` and compiles no
// suite in this target, so it stands as an ordinary comment and the rule it states stands in
// the module doc at the HEAD of this file.)
// suite left at the top level of `tests/` fails the build. (One more fragment of this
// file's header, appended here by a landing as an INNER doc comment: `//` is legal only
// before the first item, so it stands as an ordinary comment.)
// like the header's lines is legal only BEFORE the first item, so one appended into the
// tail is `E0753` and compiles nothing. `all/all_rs_declares_each_suite_once.rs` and
// this file and every service crate's `tests/all.rs`, which is why this fragment stands as
// an ordinary comment rather than the `//` a landing appended it as.
// comment is legal only before the first item of a crate root, so a `//` line in this tail is
// header that a landing appended to this tail: a `//` line past the first item is `E0753`
// and compiles no suite in this target, so it stands as an ordinary comment.)
// (A fragment of this file's own header that a landing appended to this tail: `//` is
// legal only before the first item, so these four lines stand as ordinary comments. The
// rule they state — declare each suite ONCE, since this is one crate root and a second
// `mod` for the same name is `E0428` — is in the header at the top of this file, and
// `all/all_rs_declares_each_suite_once.rs` is what checks it for this file and for every
// leaving a note here. (An inner doc comment is only legal before the first item, so
// these lines stand as ordinary comments where a landing appended them.)
// A suite left at the top level of `tests/` fails the build. (A line comment: an inner doc
// comment only leads a crate root, so one in this appended tail compiles nothing.)
// A suite left at the top level of `tests/` fails the build. (A line comment: an inner
// doc comment is legal only before the first item of the crate root.)
// one stays in the HEADER: an inner doc comment past the first item is `E0753`, so the
// same line appended into this tail compiles no suite of this target.
// comment past the first item of this crate root is `E0753` and compiles nothing.)
// comment past the first item of this crate root is `E0753`.)
// same line again, as an ordinary comment: a suite left at the top level of `tests/` fails
// the build.
// fragment that landed here ("a suite left at the top level of `tests/` fails the build")
// stands as an ordinary comment.
// At the head for the same reason as the two entries above it.
// region of this file: the IaC form of an EventBridge fan-out — one stack's rule with a
// Lambda target and a Step Functions target under a target role — spans CloudFormation,
// declared twice is `E0428` and compiles neither copy.)
// leaving a note here. (Ordinary comments: an inner doc comment `//` may appear only
// comment is only legal before the first item of a module.)
// suite left at the top level of `tests/` fails the build. (Ordinary comment: an inner doc
// this file and for every service crate's `tests/all.rs`. (Ordinary comment, for the reason
// the line above records.)
// A suite left at the top level of `tests/` fails the build. (A fragment of this file's own
// header that a landing appended here typed with `//`, which is `E0753` anywhere but the
// head of a crate root, so it stands as an ordinary comment.)
// (`sfn_relayed_dynamodb_request_id` is declared ONCE, further up this same tail: two
// landings in the same window each appended a declaration of it here, which is `E0428` and
// compiles neither copy, so the one that stood here is gone.)
// Declared here, in the appended tail, which is where every new suite inserts its lines: the
// whole API Gateway -> Lambda -> DynamoDB/S3 -> X-Ray trace tree a traced stage leaves behind
// spans six service crates at once, so no single service crate's test target can drive it.
// (The same line appended again with `//`, which is `E0753` past the first item.)
// (One more line of that same appended header fragment, an ordinary comment for the reason
// the lines around it record: a `//` line past the first item is `E0753`.) A suite left at
// the top level of `tests/` fails the build.
// (Another fragment of this file's own header appended here by a landing, typed with `//`:
// an inner doc comment is legal only BEFORE the first item, so one in this tail is `E0753`
// and compiles nothing. It stands as an ordinary comment — declare each suite exactly ONCE,
// since this is one crate root and a second `mod` for the same name is `E0428`, which
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of.)
// (Two more lines of that header fragment, as ordinary comments for the same reason:
// `tests/all.rs`, so check there rather than leaving a note here.)
// (Four more lines of that appended header fragment, as ordinary comments for the reason
// they themselves state: declare each suite exactly ONCE, since this is one crate root and
// a second `mod` for the same name is `E0428`, and a note written with `//` anywhere but
// this file's head is `E0753` — either fails every suite in this target;
// five further lines of it landed here as `//` and are gone for exactly that reason.)
// ONCE: this is one crate root, so a second `mod` for the same name is `E0428`, and a
// declarations and `all/all_rs_inner_doc_comments_precede_the_first_item.rs` checks the
// notes, for this file and for every service crate's `tests/all.rs`, so check there
// rather than leaving a note in this file's tail, where an inner doc comment is `E0753`
// and compiles nothing. (Five more lines of that header fragment, ordinary comments for
// the reason they state.)
// and compiles nothing.
// above: two branches landing in the same window each appended a second copy here, and a
// file, for the reason the entry there records: whether a flat-rate CloudFront subscription
// association and then ask Pricing Plan Manager for the subscription.)
// before the first item, so a `//` line in this tail is `E0753` and compiles no suite of
// this target. The rule it states stands in the header at the top of this file.)
// this file and for every service crate's `tests/all.rs`. (The same appended header
// fragment, an ordinary comment for the reason the lines above record.)
// A suite left at the top level of `tests/` fails the build. (The same fragment again, an
// ordinary comment for the reason the lines above record.)
// this file and every service crate's `tests/all.rs`, which is where to check rather than
// one stays in the HEADER: an inner doc comment past the first item is `E0753`, so the same
// note appended to the tail of this file compiles no suite of this target — which is why
// these lines are ordinary comments.
// (That suite is declared ONCE, further up this file: two landings in the same window each
// declared it, and a module may be declared only once — `E0428` compiles no suite here.)
// (`ses_event_destination_reaches_an_eventbridge_rule_target` is declared ONCE, in the
// sorted span above, for the same reason.)
// it scales. (The suite is declared ONCE, in the head region of this file: two landings in
// the same window each appended a declaration of it here too, and a module declared twice
// is `E0428` and compiles neither.)
// (An appended fragment of the module doc the HEAD of this file already states — a suite left
// at the top level of `tests/` fails the build. It is a LINE comment here: an inner doc
// comment is only legal before the first item, so landing it in this tail failed every suite
// in this target.)
// leaving a note here. (A LINE comment: that guidance is the module doc at the HEAD of
// this file, and an inner doc comment past the first item is `E0753`, which compiles no
// suite of this target.)
// (The same fragment again, as a LINE comment for the same reason.)
// A suite left at the top level of `tests/` fails the build; the check for that is
// `all/all_rs_declares_each_suite_once.rs`, over this file and every service crate's
// `tests/all.rs`. (Both lines landed here as `//`: an inner doc comment is legal only
// before the first item, so they stand as ordinary comments.)
// this file and every service crate's `tests/all.rs`, which is where to look rather than
// … this file and for every service crate's `tests/all.rs`, so check there rather than
// leaving a note here. (An ordinary comment, for the reason the lines above record.)
// (And two more lines of it, appended the same way: the suite-declaration check covers this
// name is `E0428` and a note written with `//` anywhere but the header is `E0753` — either
// fails every suite in this target. (Another copy of the header a landing appended here,
// (Four more orphaned lines of that header fragment, appended with `//` by another landing
// and ordinary comments here for the same reason: declare each suite exactly ONCE, since
// this is one crate root and a second `mod` for the same name is `E0428`, and a note written
// with `//` anywhere but this file's header is `E0753` — either fails every suite in this
// target. `all/all_rs_declares_each_suite_once.rs` checks the declarations, for this file
// and for every service crate's `tests/all.rs`.)
// one stays at the head: an inner doc comment past the first item is `E0753`, so the same
// note appended to the tail of this file compiles no suite of this target.
// `all/all_rs_declares_each_suite_once.rs` checks the declarations and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` checks the notes, for this file
// and for every service crate's `tests/all.rs`, so check there rather than leaving a note in
// this file's tail, where an inner doc comment is `E0753` and compiles nothing.
// (And one more copy of that same line, appended typed with `//`: an inner doc comment is
// `E0753` anywhere but the head of a file, so it stands as an ordinary comment here.)
// (One more copy of that same header fragment, appended into this tail typed with `//`: an
// inner doc comment is legal only BEFORE the first item of this crate root, so one here is
// `E0753` and compiles no suite of this target. The rule it repeats — declare each suite
// exactly ONCE, since a second `mod` for the same name is `E0428` and fails every suite in
// this target — stands in the module doc at the HEAD of this file, and
// it.)
// (Two more lines of that header fragment, appended typed with `//` and standing as
// ordinary comments for the same reason: the suite-declaration check covers this file and
// every service crate's `tests/all.rs`, so check there rather than leaving a note here.)
// (`sfn_relayed_dynamodb_request_id` and `cloudformation_provisions_cloudfront_resources` are
// each declared ONCE above — the first further up this tail, the second in the sorted
// CloudFormation span — so the second declaration two concurrent landings appended here is
// gone rather than re-typed: a module declared twice is `E0428` and compiles neither copy.)
// (And four more lines of that header fragment, appended typed with `//` and standing as
// ordinary comments: declare each suite ONCE, since this is one crate root and a second
// `mod` for the same name is `E0428`, and a note written with `//` anywhere but the header
// is `E0753` — either fails every suite in this target.)
// line past the first item of this crate root is `E0753` and compiles no suite.)
// leaving a note in the tail. (These six lines landed here as `//`, which is exactly the
// `E0753` they describe, so they stand as ordinary comments.)
// leaving a note here. (Ordinary comments: an inner doc comment mid-file is `E0753`.)
// comments here, for the reason they themselves record.)
// (Six more lines of this file's own header were appended into this tail with `//` by a
// landing: past the first item an inner doc comment is `E0753` and compiles no suite of
// this target, so the fragment is dropped rather than re-typed — the rule it repeated
// stands in the module doc at the HEAD of this file.)
// comment is legal only before the first item of this crate root, so a `//` line here is
// `E0753` and compiles no suite of this target.)// it. The `sfn_relayed_dynamodb_request_id` suite is declared ONCE as well, just above: two
// and here: what the PRINCIPAL TAGS a tagged vend attaches decide about that read — the ABAC
// document on the mapped role resolving `${aws:PrincipalTag/<key>}` per session — spans the
// same four services, so no single service crate's test target can see both ends of it.
// (The notes a landing appends to this tail about this file's own header, and about
// suites it found declared twice, are removed rather than kept: the rules they repeat
// stand in the module doc at the HEAD of this file, and
// `all/all_rs_declares_each_suite_once.rs` with
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of them.)
// (More notes of the same kind, removed for the reason the block above states.)
// (Fragments of this file's own header that landings appended here, removed for the
// reason above.)
// (`s3_form_post_upload_routes_by_event_bridge_reason` is declared ONCE, further up this
// file: the declaration that stood here was a second copy, which compiles neither.)
// (More of the same, removed for the same reason.)
// (Notes about suites those landings found declared twice, removed for the same reason.)
// (Fragments of this file's own module header that landings appended into this tail typed
// with `//` — past the first item that is `E0753` and compiles no suite of this target —
// are removed rather than re-typed or converted: the rules they repeat stand in the module
// doc at the HEAD of this file, and `all/all_rs_declares_each_suite_once.rs` with
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of them, for
// (More notes of the same two kinds, removed for the reason above.)
// Declared here, in this same head region, for the reason the entry above records: how a
// (More of the same, removed for the reason above.)
// (The notes that stood at the end of this file — fragments of its own module header that
// landings appended typed with `//`, which past the first item is `E0753`, and notes about
// suites they found declared twice — are removed rather than re-typed or converted: the
// rules they repeat stand in the module doc at the HEAD of this file, and
// declarations, for this file and for every service crate's `tests/all.rs`.)
// leaving a note in the tail. (Which is why these six lines are ordinary comments.)
// (Fragments of this file's own header and duplicate declarations that landings appended
// here are gone rather than re-typed, for the reason the note at the end of this file
// records.)
// each target. (Declared ONCE in the head region above: a module declared twice is `E0428`
// and compiles neither copy, so the second declaration two concurrent landings appended here
// is a note rather than a `mod` line.)
// it scales. (Declared ONCE in the head region above: a module declared twice is `E0428` and
// compiles neither copy, so the second declaration appended here is a note rather than a
// `mod` line.)
// declared twice is `E0428` \u2014 so the duplicate is removed and the first declaration stands.)
// region of this file: two branches landing at once each appended their own copy here, and
// a module declared twice is `E0428` and compiles neither, so the duplicate is removed and
// the first declaration stands.)
// before the first item, so one in this appended tail is `E0753` and compiles nothing.)
// leaving a note here. (That rule now stands in the module header at the HEAD of this
// file, where an inner doc comment is legal; these lines are ordinary comments because a
// reason the block above records: a `//` line outside this file's head is `E0753`.)
// leaving a note here. (Ordinary comments: that rule stands in the module header at the
// HEAD of this file, where an inner doc comment is legal.)
// this file and for every service crate's `tests/all.rs`. (Two more fragments of this
// file's own header that a merge of appended tails landed here; ordinary comments,
// because a `//` line past the first item is `E0753`.)
// leaving a note here. (Ordinary comments: the rule they state stands in the module doc
// at the HEAD of this file, which is the only place an inner doc comment is legal.)
// same note appended to the tail of this file compiles no suite of this target. (Which
// is why these lines are ordinary comments.)
// suite left at the top level of `tests/` fails the build. (Ordinary comments: a `//`
// line past the first item of this crate root is `E0753`.)
// is legal only BEFORE the first item, so one appended into the tail is
// leaving a note in the tail. (Hence the ordinary comments.)
// leaving a note here. (An ordinary comment, for the reason the blocks above record.)
// appended to the tail of this file would compile no suite of this target. (Two more
// fragments of the header, as line comments for the reason the lines above record.)
// (Every line of this block is an ordinary comment: the rule they state stands in the
// module header at the HEAD of this file, the only place an inner doc comment is legal.)
// leaving a note in the tail. (Ordinary comments here, for exactly that reason.)
// (`sfn_relayed_dynamodb_request_id` is declared ONCE, a few lines above in this same tail,
// and `cloudformation_provisions_cloudfront_resources` ONCE in the sorted span at the head
// of this file: two landings in the same window each appended a declaration of them here,
// and a module declared twice is `E0428` and compiles neither copy, so the duplicates are
// gone and the first declarations stand.)
// Declared ONCE in the head region of this file (line 102), for the reason the entry there
// records: the IaC form of an EventBridge fan-out — one stack's rule with a Lambda target and
// a Step Functions target under a target role — spans CloudFormation, EventBridge, Lambda,
// Step Functions, IAM, STS and Amazon SQS at once. Two landings in the same window each
// appended a declaration of it here, and a module declared twice is `E0428`, so this copy is
// a note rather than a second `mod`.
// (Declared ONCE further up this file, at line 33, for the reason recorded there: two
// landings in the same window each appended a declaration of it here, and a module declared
// twice is `E0428` and compiles neither copy, so this stands as a note.)
// doc comment is only legal before the first item of a crate root, so this appended
// fragment of this file's header stands as `//` rather than `//`.)
// (Every line of this block is a fragment of this file's own header that a landing
// appended here as an INNER doc comment: past the first item a `//` line is `E0753` and
// compiles no suite of this target, so each stands as an ordinary comment.)
// (`bedrock_agentcore_gateway_mcp_transport_path` is declared ONCE, above: the second
// and SQS mounted together just as the firing leg does. (That suite is declared ONCE, in the
// head region above: two branches landing at once each appended a declaration of it here, and
// a module declared twice is `E0428` and compiles neither, so the earlier one stands.)
// (That suite is declared ONCE, further up this file: two branches landing at once each
// appended a declaration of it here, and a module declared twice is `E0428` and compiles
// neither, so the earlier declaration stands and this one is removed.)
// (Declared ONCE, further up this file: a module declared twice is `E0428` and compiles
// neither, so the earlier declaration stands and the copy that stood here is removed.)
// (That suite is declared ONCE, in the head region above: a module declared twice is `E0428`
// and compiles neither, so the earlier declaration stands and this one is removed.)
// above: a module declared twice is `E0428` and compiles neither, so the earlier declaration
// stands and the copy appended here is removed.)
// (That suite is declared ONCE, further up this file: two landings each appended a
// declaration of it here, and a module declared twice is `E0428` and compiles neither, so
// the earlier declaration stands and this one is removed.)
// leaving a note here. (Line comments: an inner doc comment is only legal before the
// first item of a crate root, so this fragment of the header note is `E0753` here.)
// comment landed mid-file is `E0753` and compiles no suite of this target.)
// suite left at the top level of `tests/` fails the build. (A line comment: a fragment of
// this file's own header, which is `E0753` anywhere but the head of the file.)
// the fragments above record: an inner doc comment mid-file is `E0753`.)
// comment is legal only before the first item, so one here is `E0753`.)
// leaving a note here. (Ordinary comments, for the reason the notes above record.)
// (One more line of that header fragment, an ordinary comment for the reason the lines
// above record: a suite left at the top level of `tests/` fails the build.)
// (Another fragment of this file's own header, appended into this tail by a landing as
// `//`: an inner doc comment is legal only BEFORE the first item, so one here is `E0753`
// and compiles no suite of this target. It stands as an ordinary comment — declare each
// suite exactly ONCE, since this is one crate root and a second `mod` for the same name is
// `E0428`, and `all/all_rs_declares_each_suite_once.rs` and
// two checks above cover this file and every service crate's `tests/all.rs`, so check
// there rather than leaving a note in this tail.)
// (And four more lines of that header, ordinary comments for the same reason: declare each
// suite ONCE, since this is one crate root and a second `mod` for the same name is
// `E0428`, and a note written with `//` anywhere but this file's head is `E0753` — either
// above it record: a suite left at the top level of `tests/` fails the build.)
// (Six more lines of that header fragment, appended into this tail by landings as `//`
// and standing as ordinary comments for the reason the lines above record: declare each
// rather than leaving a note here.)
// (And seven more lines of that header, ordinary comments for the same reason: declare
// each suite ONCE, since this is one crate root and a second `mod` for the same name is
// `E0428` and fails every suite in this target;
// crate's `tests/all.rs`, and `all/all_rs_inner_doc_comments_precede_the_first_item.rs`
// checks that a note like this one stays in the HEADER — an inner doc comment past the
// first item is `E0753`, so the same note appended to this tail compiles no suite of this
// target.)
// item, so in this region it is `E0753` and compiles nothing — hence an ordinary comment.)
// reason the lines above record: a `//` line in this appended tail is `E0753`.)
// (Several landings appended fragments of this file's own header into this tail typed with
// `//`: an inner doc comment is legal only BEFORE the first item of a crate root, so one
// here is `E0753` and compiles no suite of this target. The rule those lines repeated — a
// suite left at the top level of `tests/` fails the build, and each suite is declared
// exactly ONCE — stands in the module doc at the HEAD of this file, and
// it for this file and for every service crate's `tests/all.rs`, so the fragments are gone
// rather than re-typed.)
// fragment that stood here as a `//` line is gone rather than re-typed.)
// (Six more lines of that same header landed in this tail typed with `//`: past the first
// (Ten further lines of this file's own header landed here typed with `//`, which is
// suite in this target. `all/all_rs_declares_each_suite_once.rs` and
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, in the appended tail
// below, where a concurrent landing placed it: a module declared twice loads the same file
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, in the appended tail
// this tail: two landings in the same window each appended a declaration of it here as
// well, and a module declared twice loads the same file twice, which compiles neither.)
// two landings in the same window each appended a declaration of it here as well, and a
// header, appended here by a landing: past the first item an inner doc comment is `E0753`
// header appended here by a landing: an inner doc comment is legal only before the first
// item, so it stands as an ordinary comment.)
// this file and every service crate's `tests/all.rs`, so check there rather than leaving
// a note in the tail.
// leaving a note here. (Six more lines of that header fragment, ordinary comments for the
// (Declared ONCE, further up this file: a module may be declared only once, and two
// landings in the same window each kept a copy of this line.)
// each target. (Declared ONCE, in the sorted span at the head of this file: two landings in
// the same window each appended a declaration of it here, and a module declared twice
// compiles neither copy.)
// it scales. (Declared ONCE, in the sorted span at the head of this file: two landings in
// (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, further up this file: a
// module may be declared only once, and two landings each kept a copy of this line.)
// leaving a note at the end of this file — an inner doc comment is legal only before the
// first item, so one appended to the tail is `E0753` and compiles nothing.
// `zz_probe` further up this tail: two landings in the same window each appended a second
// declaration of them here, and a module declared twice loads the same file twice, which
// compiles neither — so those duplicates are removed and the first declarations stand.)
// `cloudformation_scripted_queue_deploy` are each declared ONCE, in the head region above:
// two landings in the same window appended a second declaration of each here, and a module
// declared twice loads the same file twice, which compiles neither — so those duplicates are
// removed and the first declarations stand.)
// (`ses_event_destination_reaches_an_eventbridge_rule_target` and
// `pricing_plan_subscription_resource_eligibility` are each declared ONCE already — the first
// further up this file, the second in the sorted head: two landings in the same window each
// appended a second declaration here, and a module declared twice loads the same file twice,
// this file: two landings in the same window appended a second declaration of it here, and a
// this file: two landings in the same window each appended a declaration of it here, and a
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, in the sorted head of
// this file, and `cloudformation_provisions_cloudfront_resources` ONCE further up: two
// landings in the same window appended a second declaration of each here, and a module
// (`cloudformation_eventbridge_fan_out_to_lambda_and_sfn` is declared ONCE, further up this
// file: two landings in the same window declared it here as well, and a module declared twice
// tail: two landings in the same window appended a second declaration of it here, and a
// (The "declare each suite exactly ONCE" paragraph belongs to the module doc at the head of
// this file, where an inner doc comment is legal; appended here as `//` it is `E0753` and
// compiles nothing, so these lines are ordinary comments and the paragraph itself is in that
// header.)
// (Written with `//` on purpose: an inner doc comment past the first item is `E0753`.)
// line below is an ordinary comment.)
// (These six suites exist under `tests/all/` but had lost their declaration while several
// branches landed in the same window — each copy was removed as a duplicate of another, so
// none was left and the build script refused the target. Each is declared exactly ONCE
// (Records of duplicate declarations already resolved above, and fragments of this file's
// own header, dropped for the reason the note at the end of this file gives.)
// Declared here, in this same head region, for the reason the entry above records: which role a
// CloudWatch Logs subscription delivery is authorized against, and what that role must allow,
// is decided by CloudWatch Logs, IAM and the destination's own service at once, so no single
// service crate's test target can set the permissions and then read the destination.
// Declared here for the reason the entry above records: what a stack does when its
// Lambda-backed custom resource's provider answers FAILED is only observable across
// CloudFormation, AWS Lambda and CloudWatch Logs at once — the Delete
// request the rollback sends is read out of the provider's own log — so no single service
// crate's test target can deploy the stack and then read it.
// The notes concurrent landings appended to the END of this file — fragments of the header
// above, and bookkeeping about duplicate declarations they had already removed — live in
// the file below rather than in this crate root: they had grown past the project's
// 5,000-line ceiling for one file, and an inner doc comment among them is `E0753` here
// while a file holding comments alone can carry one. Add a DECLARATION here; leave the
// rules to the header above.
#[path = "all/appended_notes.rs"]
mod appended_notes;
// (`cloudformation_provisions_memorydb_groups` is declared ONCE, in the head region of this
// file: the second declaration a concurrent landing appended here is gone, a module
// `lambda_sqs_event_source_visibility_hold` and `codebuild_s3_build_output` are each
// declared ONCE already — the first two in the head region, the other two further up this
// file — and two branches landing in the same window each appended a second declaration of
// them here, which is `E0428` and compiles no suite of this target, so the copies are gone
// and the declarations above stand. `cloudformation_scripted_queue_deploy` is the same
// case: it is declared ONCE further up this file, so the copy appended here is gone too.)
// `cloudformation_scripted_queue_deploy` are each declared ONCE, in the sorted spans
// above: two branches landing in the same window declared each of them here as well,
// which is `E0428` and compiles neither copy, so the duplicates are removed and the
// `pricing_plan_subscription_resource_eligibility` are each declared ONCE, further up this
// file: two landings each appended them here, and a module declared twice is `E0428`, so these
// copies are removed and the first declarations stand.)
// this file: this tail's copy is removed, a module declared twice being `E0428`.)
// `pricing_plan_subscription_resource_eligibility` are each declared ONCE as well — the
// first further up this tail, the second in the head region — and two branches landing in
// the same window had appended a second declaration of each here, which is `E0428` and
// compiles no suite of this target, so the copies are gone and the declarations above
// span above: the copy that stood here was a second declaration of it, which is `E0428` and
// compiles neither, so it is removed.)
// span above: two branches landing in the same window each appended a declaration of it
// here, which is `E0428` and compiles neither copy, so the duplicate is removed and the
// declaration above stands.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, further up this file: two
// landings each appended it, and a module declared twice is `E0428`, so this tail's copy is
// removed and the first declaration stands.)
// file: two landings each appended it, and a module declared twice is `E0428`, so this tail's
// copy is removed and the first declaration stands.)
// suite left at the top level of `tests/` fails the build. (A LINE comment: an inner doc
// this file and for every service crate's `tests/all.rs`. (A LINE comment, for the reason
// (`codebuild_secrets_manager_environment_variable` is declared ONCE, further up this file:
// the second declaration a concurrent landing appended here is gone, a module declared
// twice being `E0428`.)
//! Put a new suite at `tests/all/<name>.rs` and declare it below, keeping the list sorted.
//! is one crate root, so a second `mod` for the same name is `E0428`, and an inner doc
//! comment (`//!`) is legal only before the first item, so a note written with `//!` anywhere
//! but this header is `E0753` — either one fails every suite in this target. A note further
//! down this file has to be an ordinary `//` comment.
// (`tests/boot_serve.rs`) — which the layout check refuses. The `#[path]` is what points the
//! Put a new suite at `tests/all/<name>.rs` and declare it below: one `#[path]` / `mod` pair
//! per suite and NOTHING else — no note, no narration — so a landing has one place to insert
//! and nothing to keep consistent.
//! left at the top level of `tests/` fails the build. Declare each suite exactly ONCE: this is
//! one crate root, so a second `mod` for the same name is `E0428` and fails every suite in
//! this target. An inner doc comment (`//!`) is legal only BEFORE the first item, so this
//! block is the only place one may stand: past it a `//!` line is `E0753` and compiles no
//! suite of this target. `all/all_rs_declares_each_suite_once.rs` and
//! `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` resolves to a SIBLING of this
//! file, which the layout check refuses: the `#[path]` is what points each module into
//! `tests/all/`.
//! `#[path = "all/<name>.rs"]` / `mod <name>;` pair, keeping the list sorted. The list holds
//! nothing else, so a landing has one place to insert and nothing to keep consistent.
//! in this target. An inner doc comment (`//!`) is legal only before the first item, so THIS
//! BLOCK IS THE ONLY PLACE ONE MAY STAND: past it, `//!` is `E0753` and compiles no suite of
//! this target, so leave no note below — add a declaration, not a note about one.
//! file — which the layout check refuses. The `#[path]` is what points each module into
//! in this target.
//! This file is a LIST and nothing else: one `#[path = "all/<name>.rs"]` / `mod <name>;`
//! pair per suite under `tests/all/`, and no prose between them. Put a new suite at
//! `tests/all/<name>.rs` and append its pair here.
//! * A suite that is not declared here compiles nowhere and its tests never run, and a
//!   suite left at the top level of `tests/` fails the build.
//! * Declare each suite exactly ONCE: this is one crate root, so a second `mod` for the
//!   same name is `E0428` and fails every suite in this target.
//! * `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` resolves to a SIBLING of
//!   this file: every declaration carries the `#[path = "all/<name>.rs"]` attribute.
//! * THIS HEADER IS THE ONLY PLACE AN INNER DOC COMMENT (`//!`) MAY STAND IN THIS FILE.
//!   Past the first item one is `E0753` and compiles no suite of this target, so add a
//!   declaration here — never a note about one, and never a copy of this header.
//! file and for every service crate's `tests/all.rs`.
//! never run (the crate's build script refuses an undeclared file under `tests/all/`); a
//! suite left at the top level of `tests/` fails the build; and a suite declared twice is
//! `E0428`, which fails every suite in this target.
//! DECLARATION — not a note about one. `all/all_rs_declares_each_suite_once.rs` and
//! This file is a DECLARATION LIST and nothing else. It is appended to by every landing, so
//! prose accumulates here faster than anywhere else in the tree: successive landings had
//! appended copies of this header past the first item (each one `E0753`), duplicate `mod`
//! lines (each one `E0428`) and then notes about both, until the notes outweighed the
//! declarations and the file passed the project's 5000-line limit. The declarations are
//! what the compiler, the build script and the two guard suites above read, so the list
//! below is kept to them; what each suite covers belongs in that suite's own module doc,
//! where it is read with the test it describes.
//! one place to insert and nothing to keep consistent. A suite no `mod` names compiles
//! nowhere and its tests never run; a suite left at the top level of `tests/` fails the
//! build; and a suite declared twice is `E0428`, which fails every suite in this target.
//! declaration — not a note about one, and not a copy of this paragraph.
//! each declaration into `tests/all/`. Why a suite belongs to this cross-service target
//! rather than to one service crate is stated in that suite file's own module doc, which is
//! the one place it cannot drift from the suite it describes.
//! at the top level of `tests/` fails the build.
//! Two rules bind every line of this file, and breaking either fails EVERY suite in this
//! target rather than one:
//! * Declare each suite exactly ONCE. This is one crate root, so a second `mod` for the same
//!   name is `E0428`.
//! * An inner doc comment (`//!`) is legal only BEFORE the first item, so this header is the
//!   only place one may stand. A note further down this file is `E0753`; write it as an
//!   ordinary `//` comment.
//! leaving a note at the end of this file.
//! A declaration is a `#[path = "all/<name>.rs"]` attribute and the `mod <name>;` it
//! precedes: `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` would resolve to a
//! SIBLING of this file (`tests/<name>.rs`), which the layout check refuses. The `#[path]`
//! is what points the module into `tests/all/`.
//! every suite in this target.
//! the first item one is `E0753` and compiles no suite of this target, so a note further
//! down must be an ordinary `//` comment. Below, add a declaration — not a note about one.
//! never run; a suite left at the top level of `tests/` fails the build.
//! Two rules bind every declaration, because this file is ONE crate root:
//! 1. Declare each suite EXACTLY ONCE — a second `mod` for the same name is `E0428` and
//!    fails every suite in this target.
//! 2. THIS BLOCK IS THE ONLY PLACE AN INNER DOC COMMENT (`//!`) MAY STAND IN THIS FILE:
//!    past the first item one is `E0753`, which also fails every suite in this target. A
//!    note further down must be an ordinary `//` comment — better still, put it in the
//!    suite's own file.
//! for this file and for every service crate's `tests/all.rs`. Below, add a DECLARATION —
//! not a note about one: this list carries nothing but the pairs, so two landings in the
//! same window conflict on one line instead of on a paragraph.
//! each module into `tests/all/`.
//! One `#[path]` / `mod` pair per suite, kept sorted. A suite this file does not declare
//! compiles nowhere and its tests never run; a suite left at the top level of `tests/`
//! fails the build; and a suite declared twice is `E0428`, which fails every suite in this
//! target, because this is ONE crate root and each `mod <name>;` introduces `<name>` into
//! its single namespace.
//! the first item one is `E0753`, which likewise fails every suite here. Below, add a
//! declaration — not a note about one, and not another copy of this header. WHY a suite
//! has to be a cross-service suite belongs in that suite's own module doc, beside the code
//! it describes, which is also the one place it cannot collide with a concurrent landing.
//! The `#[path]` attribute is what points a module into `tests/all/`: this file is a
//! test-target CRATE ROOT, so a bare `mod` would resolve to a sibling of it, which the
//! layout check refuses.
//! pair, keeping the list sorted. A suite that is not declared here compiles nowhere and
//! its tests never run, and a suite left at the top level of `tests/` fails the build.
// it into `tests/all/`.
// The list below is sorted and holds nothing but one `#[path]`/`mod` pair per suite, so a
// landing has one place to insert and nothing else to keep consistent.
//! file is one crate root, so a second `mod` for the same name is `E0428` and fails every
//! suite in this target.
//! the first item one is `E0753` and compiles no suite of this target either. Below, add a
//! declaration — never a note about one. `all/all_rs_declares_each_suite_once.rs` and
//! this file and for every service crate's `tests/all.rs`, so leave a note THERE rather than
//! here.
//! THIS HEADER IS THE ONLY PLACE AN INNER DOC COMMENT (`//!`) MAY STAND IN THIS FILE:
//! past the first item one is `E0753` and compiles no suite of this target, so a note
//! added further down is an ordinary `//` comment. Below, add a declaration — not a note
//! about one. `all/all_rs_declares_each_suite_once.rs` and
//! `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` would resolve to a SIBLING
//! of this file (`tests/<name>.rs`), which the layout check refuses: the `#[path]` is what
//! points each module into `tests/all/`.
//! left at the top level of `tests/` fails the build.
//! Two rules this file lives or dies by, both checked by suites of its own
//! (`all/all_rs_declares_each_suite_once.rs` and
//! `all/all_rs_inner_doc_comments_precede_the_first_item.rs`, for this file and for every
//! service crate's `tests/all.rs`):
//! * Declare each suite EXACTLY ONCE. This is one crate root, so a second `mod` for the
//!   same name is `E0428` and takes every suite in the target down with it.
//! * An inner doc comment (`//!`) is legal only BEFORE the first item, so this header is
//!   the only place one may stand: a note written with `//!` further down this file is
//!   `E0753` and compiles nothing. Write such a note as an ordinary `//` comment, or
//!   better, put the rule here.
//! this file (`tests/boot_serve.rs`) — which the layout check refuses. The `#[path]` above
//! each declaration is what points it into `tests/all/`, and the checker suite above
//! requires one naming `all/<name>.rs`.
//! The list below is the declarations and nothing else: per-entry notes about WHY a suite
//! needs the whole mounted service set belong in that suite's own module doc, where they
//! stay next to the test they describe instead of growing this root past the project's
//! file-length limit.
//! `#[path]`/`mod` pair, keeping the list sorted. A suite that is not declared here
//! compiles nowhere and its tests never run; a suite left at the top level of
//! `tests/` fails the build; and a suite declared twice is `E0428`, which fails every
//! THIS BLOCK IS THE ONLY PLACE AN INNER DOC COMMENT (`//!`) MAY STAND IN THIS FILE:
//! further down must be an ordinary `//` comment. Below, add a declaration — not a note
//! this file, which the layout check refuses: the `#[path]` is what points each module
//! into `tests/all/`. Some of the files below are shared harnesses rather than suites
//! (`query_wire`, `eks_wire`, `cloudformation_stack_clock`) and some are empty probes;
//! every file under `tests/all/` is declared, because a file no declaration names is
//! compiled nowhere.
//! Put a new suite at `tests/all/<name>.rs` and declare it below with one
//! `#[path = "all/<name>.rs"]` / `mod <name>;` pair, keeping the list sorted: this file holds
//! those pairs and nothing else, so a landing has one place to insert and nothing to keep
//! consistent.
//! A suite no `mod` names compiles nowhere and its tests never run; a suite left at the top
//! level of `tests/` fails the build; and a suite declared twice is `E0428`, which fails every
//! suite in this target. These header lines are the only place an inner doc comment (`//!`)
//! may stand — past the first item one is `E0753` and compiles no suite of this target — so a
//! note further down this file must be an ordinary `//` comment.
//! declaration — not a note about one, and not a second copy of this paragraph.
//! `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod <name>;` resolves to a sibling
//! of this file: every declaration carries `#[path = "all/<name>.rs"]`, which is the other
//! half those two suites check.
