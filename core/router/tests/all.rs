// SPDX-License-Identifier: CC0-1.0

// Single integration-test target for the router: the canonical DECLARATION LIST, and
// nothing else.
// Below this header the file holds one `#[path = "all/<name>.rs"]` / `mod <name>;` pair per
// suite file under `tests/all/`, sorted by name, and no other line. Put a new suite at
// `tests/all/<name>.rs` and insert its pair where its name sorts to; what that suite covers,
// and why it drives the assembled router rather than one service crate, belongs in its OWN
// module header, where it is read beside the test it explains.
// EVERY comment in this file is an ordinary one, this header included, and that is
// deliberate: an inner doc comment documents the module it stands in, so the language accepts
// it only ahead of the first item, and one a landing leaves below the list is `E0753` and
// compiles no suite of this target. With none anywhere in the file, no appended, re-typed or
// merged copy of a line of it can be one.
// `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` would resolve to a sibling of
// this file (`tests/<name>.rs`), which the layout check refuses: the `#[path]` is what points
// each declaration into `tests/all/`. A suite no `mod` here names is compiled nowhere and its
// tests never run, and a suite declared twice is `E0428` — either fails this whole target.
// `all/all_rs_carries_no_inner_doc_comment.rs`, `all/all_rs_declares_each_suite_once.rs`,
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs`,
// `all/all_rs_is_a_canonical_declaration_list.rs`,
// `all/declaration_roots_are_canonical_and_complete.rs` and
// `all/every_suite_file_is_declared.rs` pin those rules, so check there rather than leaving a
// note here; `all/appended_notes.rs` records what the notes earlier landings had appended to
// this file said.
#[path = "all/agent_registry_signing_name_routing.rs"]
mod agent_registry_signing_name_routing;
#[path = "all/all_rs_carries_no_inner_doc_comment.rs"]
mod all_rs_carries_no_inner_doc_comment;
#[path = "all/all_rs_declares_each_suite_once.rs"]
mod all_rs_declares_each_suite_once;
#[path = "all/all_rs_inner_doc_comments_precede_the_first_item.rs"]
mod all_rs_inner_doc_comments_precede_the_first_item;
#[path = "all/all_rs_is_a_canonical_declaration_list.rs"]
mod all_rs_is_a_canonical_declaration_list;
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
#[path = "all/appended_notes.rs"]
mod appended_notes;
#[path = "all/appended_notes_tail.rs"]
mod appended_notes_tail;
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
#[path = "all/autoscaling_plans_delete_withdraws_its_registration.rs"]
mod autoscaling_plans_delete_withdraws_its_registration;
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
#[path = "all/cloudformation_creates_an_ecs_capacity_provider.rs"]
mod cloudformation_creates_an_ecs_capacity_provider;
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
#[path = "all/cloudformation_private_macro_change_sets.rs"]
mod cloudformation_private_macro_change_sets;
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
#[path = "all/cloudformation_rest_api_redeployment.rs"]
mod cloudformation_rest_api_redeployment;
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
#[path = "all/cloudformation_stack_set_status_change_events.rs"]
mod cloudformation_stack_set_status_change_events;
#[path = "all/cloudformation_stack_update_redirects_a_schedule.rs"]
mod cloudformation_stack_update_redirects_a_schedule;
#[path = "all/cloudformation_stackset_failed_instance_events.rs"]
mod cloudformation_stackset_failed_instance_events;
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
#[path = "all/cloudtrail_iam_credential_write_audit.rs"]
mod cloudtrail_iam_credential_write_audit;
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
#[path = "all/declaration_roots_are_canonical_and_complete.rs"]
mod declaration_roots_are_canonical_and_complete;
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
#[path = "all/events_lambda_target_permission_granted_after_the_rule.rs"]
mod events_lambda_target_permission_granted_after_the_rule;
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
#[path = "all/every_suite_file_is_declared.rs"]
mod every_suite_file_is_declared;
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
#[path = "all/lambda_in_function_assumed_role_session.rs"]
mod lambda_in_function_assumed_role_session;
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
#[path = "all/lambda_sqs_event_source_attempt_reservation.rs"]
mod lambda_sqs_event_source_attempt_reservation;
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
#[path = "all/s3_notification_lambda_invoke_grant_gates_delivery.rs"]
mod s3_notification_lambda_invoke_grant_gates_delivery;
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
#[path = "all/s3_sourced_table_derived_key_schema.rs"]
mod s3_sourced_table_derived_key_schema;
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
#[path = "all/zz_glue_job_source_permission_probe.rs"]
mod zz_glue_job_source_permission_probe;
#[path = "all/zz_opensearch_fgac_probe.rs"]
mod zz_opensearch_fgac_probe;
#[path = "all/zz_probe.rs"]
mod zz_probe;
#[path = "all/zz_probe_whole_emulator.rs"]
mod zz_probe_whole_emulator;
// Put a new suite at `tests/all/<name>.rs` and add ONE `#[path = "all/<name>.rs"]` /
// `mod <name>;` pair below, in its sorted position; nothing else belongs in this file, and
// EVERY comment in it is an ordinary `//` one so that no merge of two landings can leave an
// inner doc comment past the first item (`E0753`, which compiles no suite of this target).
// `all/every_suite_file_is_declared.rs` pin that shape, for this file and for every service
// crate's `tests/all.rs`.
// name — one pair per suite and no other line, so two branches landing at once insert two
// deliberate: an inner doc comment (a `//` followed by a `!`) is legal only before the
// first item, so one that lands past the declarations is `E0753` and compiles no suite of
// this target at all. With none anywhere in the file, no merge of two landings can put one
// past the first item.
// Declare each suite exactly ONCE: this is one crate root, so a second `mod` for the same
// `tests/all.rs` is a test-target CRATE ROOT, where a bare `mod` resolves to a SIBLING
// (`tests/<name>.rs`) that the layout check refuses: the `#[path]` is what points each
// Four suites of this target pin the rules above rather than restating them:
// `all/declaration_roots_are_canonical_and_complete.rs`.
// service crate's own, is stated in that suite's own module doc, where it is read beside
// the test it explains; a note about this file itself belongs in `all/appended_notes.rs`.
// here so a re-appended note is a note rather than a build failure. An inner attribute,
// like an inner doc comment, is legal only before the first item, so it stands here rather
// than in the list below.
//! This file holds its module doc, then one `#[path = "all/<name>.rs"]` / `mod <name>;` pair
//! per suite file under `tests/all/`, sorted by name, and no other line. Put a new suite at
//! `tests/all/<name>.rs` and insert its pair into the sorted list below; what that suite
//! covers, and why it drives the assembled router rather than one service crate, belongs in
//! its OWN module doc, where it is read beside the test it explains, and a note about THIS
//! file belongs in `all/appended_notes.rs`.
//! Each of these fails EVERY suite of this target rather than one, which is why nothing else
//! stands here: a suite no `mod` names compiles nowhere and its tests never run (and a suite
//! left at the top level of `tests/` fails the build outright); a suite declared twice is
//! `E0428`; and an inner doc comment (`//!`) is legal only BEFORE the first item, so THIS
//! BLOCK IS THE ONLY PLACE ONE MAY STAND — past the first declaration it is `E0753`, which is
//! how the tail of this file, grown by concurrent landings into hundreds of re-typed copies of
//! this header, came to compile no suite at all. `all/all_rs_declares_each_suite_once.rs`,
//! `all/all_rs_is_a_canonical_declaration_list.rs`, `all/every_suite_file_is_declared.rs` and
//! `all/declaration_roots_are_canonical_and_complete.rs` pin all of it, for this file and for
//! every service crate's `tests/all.rs`, so check there rather than leaving a note here.
// Put a new suite at `tests/all/<name>.rs` and add its `#[path = "all/<name>.rs"]` /
// `mod <name>;` pair below, in sorted position. What a suite covers, and why it lives in
// this cross-service target rather than in one service crate's own, belongs in that
// suite's own module header, where it is read beside the test it explains.
// deliberate: an inner doc comment (`//!`) is legal only before the first item, so one
// that lands past the declarations is `E0753` and compiles no suite of this target.
// Three suites pin the rules rather than restating them here:
// `all/all_rs_declares_each_suite_once.rs` (a second `mod` for one name is `E0428`, and
// every declaration names a file that exists),
// `all/all_rs_is_a_canonical_declaration_list.rs` (one pair per suite, sorted, no other
// line) — for this file and for every service crate's `tests/all.rs`. The build-time
// layout check fails the build for a suite file no `mod` names, and for a suite left at
// the top level of `tests/`. `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod`
// resolves to a SIBLING of this file: the `#[path]` is what points each declaration into
// `tests/all/`.
// A note a landing appends here wraps without the indentation
// `clippy::doc_lazy_continuation` asks of a doc list item, so the lint is allowed below:
// a re-appended note is then a note rather than a build failure.
// `#[path = "all/<name>.rs"]` / `mod <name>;` pair below, in sorted order — one pair per
// suite and no other line. A suite no `mod` names is compiled nowhere; a second `mod` for
// one name is `E0428`; an inner doc comment past the first item is `E0753`. Each fails every
// suite of this target, so every comment here is an ordinary one and what a suite covers
// belongs in that suite's own module header (a note about THIS file in
// `all/appended_notes.rs`). The `all_rs_*` suites and `all/every_suite_file_is_declared.rs`
// check these rules, for this root and for every service crate's.
// Single integration-test target for the router: the DECLARATION LIST of every
// appended past the declarations is `E0753` and compiles no suite of this target. With no
// inner doc comment anywhere in the file, no merge of two landings can put one past the
// The list below holds one `#[path = "all/<name>.rs"]` / `mod <name>;` pair per suite
// file, sorted by name, and no other line — so two branches landing at once insert two
// lines in two places and have no prose to duplicate, re-word or mistype. This file is a
// test-target CRATE ROOT, so a bare `mod` would resolve to a sibling of it
// declaration into `tests/all/`. Declare each suite exactly ONCE: a second `mod` for one
// the test it explains. A note about this file belongs in `all/appended_notes.rs`, which
// is declared below like any other suite.
// `all/every_suite_file_is_declared.rs` check these rules, for this root and for every
// service crate's `tests/all.rs`, so check there rather than leaving a note here.
// `#[path = "all/<name>.rs"]` / `mod <name>;` pair per suite file under `tests/all/`,
// sorted by name, and NO other line. Put a new suite at `tests/all/<name>.rs` and insert
// its pair in the one place its name sorts to; what a suite covers, and why it belongs to
// this cross-service target rather than to one service crate's own, is stated in that
// suite's own module doc, and a note about THIS file belongs in `all/appended_notes.rs`,
// which is declared below like any other module and holds comments alone.
// level of `tests/` fails the build; a suite declared twice is `E0428`; and an inner doc
// comment (`//!`) is legal only before the first item, so one written past the declarations
// is `E0753`. Each of those fails EVERY suite of this target, which is why
// `all/every_suite_file_is_declared.rs` pin the shape above.
// deliberate: with no `//!` anywhere, no merge of two landings can leave a doc comment past
// the first item.
// `#[path = "all/<name>.rs"]` / `mod <name>;` pair into the sorted list below — one pair per
// suite and no other line, so two branches landing at once insert two lines in two places
// and have no prose to duplicate, re-word or mistype.
// lands past the declarations is `E0753` and compiles no suite of this target. With no `//!`
// anywhere in the file, no merge of two landings can leave one past the first item.
// A suite no `mod` here names compiles nowhere and its tests never run; a suite left at the
// top level of `tests/` fails the build; and a second `mod` for one name is `E0428`. Each
// fails every suite in this target, which is why `all/all_rs_declares_each_suite_once.rs`,
// `all/all_rs_is_a_canonical_declaration_list.rs`, `all/every_suite_file_is_declared.rs` and
// `all/declaration_roots_are_canonical_and_complete.rs` check the shape rather than restate
// it. `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` would resolve to a SIBLING
// test it explains; the notes earlier landings appended here live in `all/appended_notes.rs`.
// Single integration-test target for the router: a canonical DECLARATION LIST.
// This file holds this header and then one `#[path = "all/<name>.rs"]` / `mod <name>;` pair
// per suite file under `tests/all/`, sorted by name, and no other line. Put a new suite at
// `tests/all/<name>.rs` and insert its pair where its name sorts to.
// the declarations is `E0753` and compiles no suite of this target — which is what the
// re-typed copies of this header that concurrent landings kept appending had done. With no
// inner doc comment anywhere in the file, no merge of two landings can leave one past the
// level of `tests/` fails the build; and a suite declared twice is `E0428`. Each of those
// fails every suite in this target. `tests/all.rs` is a test-target CRATE ROOT, so a bare
// `mod` resolves to a SIBLING (`tests/<name>.rs`), which the layout check refuses: the
// `#[path]` above each `mod` is what points it into `tests/all/`.
// module header; a note about this file itself belongs in `all/appended_notes.rs`, declared
// below like any other module.
//! This file is a CANONICAL DECLARATION LIST: this module doc, then one
//! had grown hundreds of re-typed copies of this header, many of them written as inner doc
//! comments past the first item (`E0753`), so no suite of this target compiled at all.
//! `all/every_suite_file_is_declared.rs` pin the rules a landing breaks: a module declared
//! twice is `E0428`, an inner doc comment past the first item is `E0753`, and a suite file
//! no `mod` names compiles nowhere. Each of those fails every suite in this target, so put a
//! new suite at `tests/all/<name>.rs`, insert its pair into the sorted list below, and leave
//! what it covers — and why it drives the assembled router rather than one service crate — to
//! that suite's own module doc, where it is read beside the test it explains.
//! layout check refuses. THIS HEADER IS THE ONLY PLACE A `//!` LINE MAY STAND IN THIS FILE,
//! and what goes below it is a declaration — never a note about one, and never a copy of this
//! header; a note about this file itself belongs in `all/appended_notes.rs`.
// suite, sorted by name, and no other line, so two branches landing at once insert two
// EVERY comment in this file is an ORDINARY `//` one, this header included. An inner doc
// comment is legal only before the first item, so one appended past the declarations is
// `E0753` and compiles no suite of this target — which is what had happened here, where the
// tail had grown hundreds of re-typed copies of this header. With no inner doc comment in
// the file at all, no landing can put one past the first item.
// sibling of this file, which the layout check refuses: the `#[path]` is what points each
// explains; a note about this file itself belongs in `all/appended_notes.rs`. The suites
// `all/all_rs_is_a_canonical_declaration_list.rs` check the rules above rather than
// restating them.
//! Put a new suite at `tests/all/<name>.rs` and add its `#[path]`/`mod` pair below, into the
//! sorted list — one pair per suite and no other line, so two branches landing at once
//! at the top level of `tests/` fails the build; a suite declared twice is `E0428`; and an
//! inner doc comment (`//!`) past the first item is `E0753`. Each of those fails EVERY suite
//! in this target, so this header is the only place an inner doc comment may stand, and what
//! goes below it is a declaration, never a note about one.
//! `all/all_rs_is_a_canonical_declaration_list.rs` check all three rules, for this file and
//! for every service crate's `tests/all.rs`, so a note about them belongs there. What a suite
//! covers, and why it has to live in this target rather than in a service crate's own,
//! belongs in that suite's own module header, where it is read beside the test it explains.
//! `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` resolves to a SIBLING of this
//! file, which the layout check refuses: the `#[path]` is what points each module into
//! `tests/all/`.
// `mod <name>;` pair into the sorted list below. The list holds one pair per suite and no
// other line, so two branches landing at once insert two lines in two places and have no
// prose to duplicate, re-word or mistype.
// EVERY comment in this file is an ordinary `//` one, this header included: an inner doc
// comment is legal only before the first item, so with none anywhere in the file no merge of
// two landings can leave one past the first item. `tests/all.rs` is a test-target CRATE ROOT,
// so a bare `mod` would resolve to a sibling of this file, which the layout check refuses:
// the `#[path]` above each `mod` is what points it into `tests/all/`. Declare each suite
// exactly once: a second `mod` for one name is `E0428` and fails every suite in this target.
// What a suite covers, and why it drives the assembled router rather than one service crate,
// belongs in that suite's own module header; `all/appended_notes.rs` keeps the notes landings
// had appended here. `all/all_rs_declares_each_suite_once.rs`,
// `all/every_suite_file_is_declared.rs` check these rules rather than restating them.
// item, so one that lands past the declarations — which is what concurrent landings
// appending to this file kept producing — is `E0753` and compiles no suite of this target.
// With no inner doc comment anywhere in the file, no merge of two landings can put one past
// the first item. An inner ATTRIBUTE (`#!` followed by `[..]`) is the same story: past the
// first item it is a hard error, so this file carries none of those either.
// Past this header the file is declaration pairs and NOTHING else — no note, no blank line,
// and nothing after the last pair — sorted by suite name: one `#[path = "all/<name>.rs"]` /
// `mod <name>;` pair per file under `tests/all/`, so two branches landing at once insert two
// lines in two places and have no prose to duplicate, re-word or mistype. Declare each suite
// exactly ONCE: this is one crate root, so a second `mod` for the same name is `E0428` and
// `mod` would resolve to a SIBLING (`tests/<name>.rs`), which the layout check refuses; the
// `#[path]` is what points each declaration into `tests/all/`.
// service crate's own, is stated in that suite's own module header, where it is read beside
// the test it explains and cannot collide with a concurrent landing. Four suites check the
// rules above rather than restating them: `all/all_rs_declares_each_suite_once.rs`,
// Put a new suite at `tests/all/<name>.rs` and add its `#[path]`/`mod` pair below, into the
// sorted list — one pair per suite and no other line, so two branches landing at once
// insert two lines in two places and have no prose to duplicate, re-word or mistype.
// A suite that is not declared here compiles nowhere and its tests never run; a suite left
// at the top level of `tests/` fails the build; and a suite declared twice is `E0428`. Each
// of those fails EVERY suite in this target, so what goes below is a declaration, never a
// note about one. `all/all_rs_declares_each_suite_once.rs`,
// `all/every_suite_file_is_declared.rs` check those rules, for this file and for every
// service crate's `tests/all.rs`, so a note about them belongs there. What a suite covers,
// and why it has to live in this target rather than in a service crate's own, belongs in
// that suite's own module header, where it is read beside the test it explains; a note about
// THIS file belongs in `all/appended_notes.rs`.
// EVERY comment in this file is an ORDINARY `//` one, this header included, and no inner
// attribute (`#![…]`) stands anywhere in it: both are legal only before the first item, so
// one a concurrent landing leaves past the declarations compiles no suite of this target at
// all. With none in the file, no merge of two landings can produce one.
// `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` resolves to a SIBLING of this
// file, which the layout check refuses: the `#[path]` is what points each module into
// This file is the license line, this header, and then one `#[path = "all/<name>.rs"]` /
// `mod <name>;` pair per suite file under `tests/all/`, sorted by name, and NO other line —
// so two branches landing at once insert their two lines in one place each and have no prose
// EVERY comment here is an ORDINARY one, this header included, and that is deliberate: an
// inner doc comment (`//!`) is legal only before the first item, so one appended past the
// declarations is `E0753` and compiles no suite of this target. With no `//!` anywhere in the
// file, no merge of two landings can leave one past the first item.
// A suite no `mod` names compiles nowhere and its tests never run (this crate's `build.rs`
// refuses it); a suite left at the top level of `tests/` fails the build; and a second `mod`
// for one name is `E0428`. Each of those fails EVERY suite of this target. `tests/all.rs` is
// a test-target CRATE ROOT, so a bare `mod` would resolve to a SIBLING (`tests/<name>.rs`),
// which the layout check refuses: the `#[path]` is what points each declaration into
// `all/appended_notes.rs`, and four suites check the rules above rather than restating them:
// `#[path = "all/<name>.rs"]` / `mod <name>;` pair per suite file under `tests/all/`, sorted
// by name, and NO other line. Put a new suite at `tests/all/<name>.rs` and insert its pair
// in order: a landing then has one place to insert and no prose to duplicate, re-word or
// mistype.
// lands past the declarations — which is what concurrent landings appending to this file
// kept producing — is `E0753` and compiles no suite of this target. With no `//!` anywhere
// in the file, no merge of two landings can leave one past the first item.
// A suite file no `mod` here names is compiled nowhere and its tests never run; a suite left
// at the top level of `tests/` fails the build; and a second `mod` for one name is `E0428`.
// Each of those fails EVERY suite of this target. `tests/all.rs` is a test-target CRATE
// ROOT, so a bare `mod` would resolve to a SIBLING of this file (`tests/<name>.rs`), which
// the layout check refuses: the `#[path]` is what points each declaration into `tests/all/`.
// test it explains. The notes earlier landings had appended to the tail of this file live in
// pair, keeping the list sorted: one pair per suite and nothing else, so a landing has one
// place to insert and nothing to keep consistent.
// at the top level of `tests/` fails the build; and a suite declared twice is `E0428`,
// which compiles no suite of this target. THIS HEADER IS THE ONLY PLACE AN INNER DOC
// COMMENT (`//!`) MAY STAND IN THIS FILE: past the first item one is `E0753` and compiles
// nothing, so a note below must be an ordinary `//` comment — better still, add a
// declaration rather than a note about one.
// `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of that, for
// this file and for every service crate's `tests/all.rs`.
// Put a new suite at `tests/all/<name>.rs` and declare it below with a
// `#[path]`/`mod` pair, keeping the list sorted: one pair per suite and nothing
// else, so a landing has one place to insert and nothing to keep consistent.
// A suite that is not declared here compiles nowhere and its tests never run. A
// suite left at the top level of `tests/` fails the build. Declare each suite
// exactly ONCE: this is one crate root, so a second `mod` for the same name is
// `E0428` and fails every suite in this target. An inner doc comment (`//!`) is
// legal only BEFORE the first item, so THIS header is the only place one may
// stand — past it, `//!` is `E0753` and compiles nothing, so a note further down
// must be an ordinary `//` comment, or better still belong here instead.
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of
// that, for this file and for every service crate's `tests/all.rs`, so check
// there rather than leaving a note below.
// `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` resolves to a
// SIBLING (`tests/<name>.rs`) — which the layout check refuses. The `#[path]` is
// what points each declaration into `tests/all/`.
// place to insert and nothing to keep consistent. A suite no `mod` names compiles nowhere
// and its tests never run; a suite left at the top level of `tests/` fails the build; and a
// suite declared twice is `E0428`, which fails every suite in this target.
// THIS BLOCK IS THE ONLY PLACE AN INNER DOC COMMENT (`//!`) MAY STAND IN THIS FILE: past
// the first item one is `E0753` and compiles no suite of this target. Below, add a
// declaration — not a note about one. `all/all_rs_declares_each_suite_once.rs` and
// this file and for every service crate's `tests/all.rs`, so check there rather than
// leaving a note here.
// (`tests/<name>.rs`) — which the layout check refuses. The `#[path]` is what points it
// into `tests/all/`.
// Put a new suite at `tests/all/<name>.rs` and declare it below with a `#[path]` / `mod`
// A suite that is not declared here compiles nowhere and its tests never run, and a suite
// left at the top level of `tests/` fails the build. Declare each suite exactly ONCE: this
// is one crate root, so a second `mod` for the same name is `E0428`. THIS HEADER IS THE
// ONLY PLACE AN INNER DOC COMMENT (`//!`) MAY STAND IN THIS FILE: past the first item one
// is `E0753`. Either fails every suite in this target, so below add a declaration — not a
// note about one. `all/all_rs_declares_each_suite_once.rs` and
// An inner doc comment (`//` followed by `!`) is legal only before the first item, so this
// file carries none at all: past the first item one is `E0753` and compiles no suite of
// this target, and the notes concurrent landings appended to the tail of this file had
// grown to thousands of them. Below, add a declaration — not a note about one.
// ONLY PLACE AN INNER DOC COMMENT (`//`) MAY STAND IN THIS FILE: past the first item one
// (`tests/boot_serve.rs`) — which the layout check refuses. The `#[path]` above each `mod`
// pair, keeping the list sorted: one pair per suite and nothing else, so a landing has
// one place to insert and nothing to keep consistent.
// A suite no `mod` names compiles nowhere and its tests never run; a suite left at the
// top level of `tests/` fails the build; and a suite declared twice is `E0428`, which
// deliberate: an inner doc comment is legal only before the first item, so one that
// kept producing — is `E0753` and compiles no suite of this target. With no inner doc
// comment anywhere, no merge of two landings can put one past the first item.
// `all/all_rs_is_a_canonical_declaration_list.rs` check those rules, for this file and
// for every service crate's `tests/all.rs`, so check there rather than leaving a note
// here. What a suite covers, and why it lives in this target rather than in one service
// crate's own, belongs in that suite's own module header.
//! (`tests/boot_serve.rs`) — which the layout check refuses. The `#[path]` above each `mod`
//! is what points it into `tests/all/`.
//! pair, keeping the list sorted: one pair per suite and nothing else, so a landing has
//! one place to insert and nothing to keep consistent.
//! A suite that is not declared here compiles nowhere and its tests never run; a suite
//! left at the top level of `tests/` fails the build; and a suite declared twice is
//! `E0428`, which fails every suite in this target. `tests/all.rs` is a test-target CRATE
//! ROOT, so a bare `mod` resolves to a SIBLING of this file, which the layout check
//! refuses: the `#[path]` is what points the module into `tests/all/`.
//! THIS BLOCK IS THE ONLY PLACE AN INNER DOC COMMENT (`//!`) MAY STAND IN THIS FILE: past
//! declaration — not a note about one. `all/all_rs_declares_each_suite_once.rs` and
//! for this file and for every service crate's `tests/all.rs`.
//! is one crate root, so a second `mod` for the same name is `E0428` and fails every suite
//! in this target. An inner doc comment (`//!`) is legal only before the first item, so
//! THESE lines are the only place one may stand — past the first item `//!` is `E0753` and
//! compiles nothing, so leave no note below, not even an ordinary `//` one: this file is a
//! declaration list, and the rules stand here. A note about a landing belongs in the suite
//! it is about, where it is read with the test it explains (`all/appended_notes.rs` keeps
//! the ones this file had collected).
//! this file and for every service crate's `tests/all.rs`, so check there rather than
//! leaving a note here.
//! module into `tests/all/`.
//! holds those pairs and NOTHING else, so two branches landing at once insert two lines in
//! two places and have no prose to duplicate, re-word or mistype.
//! crate's `tests/all.rs`): a suite no `mod` names compiles nowhere, a suite declared twice
//! is `E0428`, and an inner doc comment past the first item is `E0753` — each of the last two
//! fails every suite in this target. WHY a suite belongs to this cross-service target rather
//! than to one service crate's own belongs in that suite's module doc, beside the test it
//! describes; `all/appended_notes.rs` keeps the notes this file had collected.
// line, so two branches landing at once insert two lines in two places and have no prose to
// duplicate, re-word or mistype.
// comment (`//!`) is legal only before the first item, so one appended past the declarations
// is `E0753` and compiles no suite of this target. Declare each suite exactly ONCE — a
// second `mod` for one name is `E0428`, which fails every suite here too. `tests/all.rs` is
// a test-target CRATE ROOT, so a bare `mod <name>;` would resolve to a sibling of this file,
// service crate's own, is stated in that suite's own module header; a note about this file
// belongs in `all/appended_notes.rs`. The suites `all/all_rs_declares_each_suite_once.rs`,
// `all/declaration_roots_are_canonical_and_complete.rs` check the rules above rather than
// name — one pair per suite and NO other line, so two branches landing at once insert two
// lines in two places and have no prose to duplicate, re-word or mistype, and nothing is
// appended past the list.
// deliberate: an inner doc comment and an inner attribute are legal only before the first
// item, so one that lands past the declarations compiles NO suite of this target. With
// none anywhere in the file, no merge of two landings can put one past the first item.
// name is `E0428`. A suite under `tests/all/` that no `mod` here names is compiled nowhere
// (this crate's `build.rs` refuses it), and a suite left at the top level of `tests/` is a
// test target of its own, which fails the build. `tests/all.rs` is a test-target CRATE
// ROOT, so a bare `mod` would resolve to a SIBLING (`tests/<name>.rs`), which the layout
// check refuses: the `#[path]` is what points each declaration into `tests/all/`.
// Four suites of this target check the rules above rather than restating them:
// the test it explains and cannot collide with a concurrent landing. A note about this
// file itself belongs in `all/appended_notes.rs`, declared below like any other module.
// Single integration-test target for the router: the canonical DECLARATION LIST.
// per suite file under `tests/all/`, sorted strictly by name, and no other line. A landing
// therefore inserts two lines in one place and has no prose here to duplicate, re-word or
// deliberate: an inner doc comment (`//!`) is legal only before the first item, so a copy of
// this header appended past the declarations is `E0753` and compiles no suite of this target.
// With no `//!` anywhere, no merge of two landings can leave one past the first item.
// A suite file no `mod` here names compiles nowhere and its tests never run (the crate's
// build-time layout check fails the build for it); a suite left at the top level of `tests/`
// fails the build; and a second `mod` for one name is `E0428`, which compiles neither copy.
// this file: the `#[path]` is what points each declaration into `tests/all/`.
// the test it explains; `all/appended_notes.rs` keeps the notes landings had appended here.
// Three suites of this target pin the shape above rather than restating it:
// lands past the declarations is `E0753` and compiles no suite of this target. With none in
// the file, no merge of two landings can leave one there — which is how this file last
// broke, after landings appended hundreds of re-typed copies of its header.
// suite, sorted by name, and NO other line, so two branches landing at once insert two
// lines in two places and have no prose to duplicate, re-word or mistype. The `#[path]` is
// what points each declaration into `tests/all/`: this file is a test-target crate root,
// where a bare `mod` would resolve to a sibling of it, which the layout check refuses.
// What a suite covers, and why it drives the assembled router rather than one service
// crate, belongs in that suite's own module header; a note about THIS file belongs in
// `all/appended_notes.rs`. Four suites pin the rules rather than restating them here:
// `all/all_rs_declares_each_suite_once.rs` (a second `mod` for one name is `E0428`),
// `all/all_rs_is_a_canonical_declaration_list.rs` (one sorted pair per suite, nothing else)
// and `all/every_suite_file_is_declared.rs`.
//! had grown re-typed copies of this header, some of them written as inner doc comments past
//! the first item (`E0753`), so no suite of this target compiled at all.
//! covers to that suite's own module doc. The `#[path]` is what points each declaration into
//! `tests/all/`: this file is a test-target CRATE ROOT, where a bare `mod` would resolve to a
//! sibling of it, which the layout check refuses. The notes earlier landings appended to this
//! file's tail live in `all/appended_notes.rs`, declared below like any other suite.
// suite file, sorted by name, and no other line — so two branches landing at once insert
// two lines in two places and have no prose to duplicate, re-word or mistype.
// past the declarations is `E0753` and compiles no suite of this target. With no `//!`
// SIBLING of this file, which the layout check refuses: the `#[path]` above each `mod` is
// what points it into `tests/all/`. Declare each suite exactly ONCE — a second `mod` for
// one name is `E0428` and fails every suite in this target.
// explains; a note about THIS file belongs in `all/appended_notes.rs`. The suites
// this header appended past the declarations is `E0753` and compiles no suite of this
// target — which is what had happened here, hundreds of times over. With no `//!` anywhere,
// name is `E0428` and fails every suite in this target. A suite file no `mod` here names is
// compiled nowhere and its tests never run, and a suite left at the top level of `tests/`
// fails the build. `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` would resolve
// to a SIBLING of this file, which the layout check refuses: the `#[path]` above each `mod`
// The suites that pin those rules rather than restating them:
// `all/every_suite_file_is_declared.rs` and
// `all/declaration_roots_are_canonical_and_complete.rs`. What a suite covers, and why it
// drives the assembled router rather than one service crate, belongs in that suite's own
// module header; a note about THIS file belongs in `all/appended_notes.rs`.
// Single integration-test target for the router: the canonical DECLARATION LIST of every
// cross-service suite, and nothing else.
// suite file under `tests/all/`, sorted by name, and no other line. Two branches landing at
// once then insert two lines in two places and have no prose to duplicate, re-word or
// mistype — which is what had happened here: the tail of this file had grown hundreds of
// re-typed copies of its own header, many of them written as inner doc comments past the
// first item, which is `E0753` and compiles no suite of this target at all.
// EVERY comment in this file is an ORDINARY one, this header included: with no inner doc
// comment anywhere, no merge of two landings can leave one past the first item.
// this file, which the layout check refuses: the `#[path]` is what points each declaration
// into `tests/all/`. Declare each suite exactly ONCE — a second `mod` for one name is
// `E0428` — and declare EVERY entry under `tests/all/` the layout check counts: each
// `<name>.rs` file, and each `<name>/` directory that carries a `mod.rs`. One no declaration
// names is compiled nowhere. Some of those files are shared harnesses rather than suites
// (`query_wire`, `eks_wire`, `cloudformation_stack_clock`) and some are probes; each is
// declared like any other.
// service crate's own, is stated in that suite's OWN module header, where it is read beside
// the test it explains. A note about THIS file belongs in `all/appended_notes.rs`.
// `all/declaration_roots_are_canonical_and_complete.rs`, beside
// `#[path = "all/<name>.rs"]` / `mod <name>;` pair into the sorted list below — one pair
// per suite and no other line, so two branches landing at once insert two lines in two
// places and have no prose to duplicate, re-word or mistype.
// past the declarations is `E0753` and compiles no suite of this target — which is what
// repeated landings appending to the tail of this file had produced. With no inner doc
// level of `tests/` fails the build; and a suite declared twice is `E0428`, which fails
// every suite in this target. `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod`
// would resolve to a SIBLING of this file, which the layout check refuses: the `#[path]`
// above each `mod` is what points it into `tests/all/`.
// `all/every_suite_file_is_declared.rs` check those rules rather than restating them.
// explains; a note about THIS file belongs in `all/appended_notes.rs`.
// Single integration-test target for the router: this header, then one
// sorted by name, and no other line.
// lands past the declarations is `E0753` and compiles no suite of this target. With none
// Put a new suite at `tests/all/<name>.rs` and insert its pair into the sorted list below.
// resolves to a SIBLING of this file, which the layout check refuses: the `#[path]` is what
// explains; a note about THIS file belongs in `all/appended_notes.rs`, declared below like
// any other module. `all/all_rs_declares_each_suite_once.rs`,
// there rather than leaving a note here.
// Single integration-test target for the router: the suites that drive MORE THAN ONE service
// over the assembled emulator.
// by name, and no other line. Two branches landing at once then insert two lines in two
// places and have no prose to duplicate, re-word or mistype — which is what had gone wrong
// here before: the tail of this file had grown hundreds of re-typed copies of its header,
// many of them written as inner doc comments past the first item (`E0753`), so no suite of
// this target compiled at all.
// EVERY comment in this file is an ORDINARY `//` one, this block included, and that is
// Single integration-test target for the router: the canonical declaration list of its
// suite file and no other line, so two branches landing at once insert two lines in two
// the declarations compiles no suite of this target at all, and with none anywhere no merge
// of two landings can leave one there.
// A suite file no `mod` here names is compiled nowhere and its tests never run (the layout
// check fails the build for it), a suite left at the top level of `tests/` fails the build,
// and a suite declared twice is `E0428`, which fails every suite in this target.
// this file: the `#[path]` above each `mod` is what points the declaration into `tests/all/`.
// Three suites of this target check those rules rather than restating them here:
// `all/all_rs_is_a_canonical_declaration_list.rs`, `all/all_rs_declares_each_suite_once.rs`
// and `all/all_rs_inner_doc_comments_precede_the_first_item.rs`, for this file and for every
// service crate's `tests/all.rs`. What a suite covers, and why it drives the assembled
// router rather than one service crate, belongs in that suite's own module header; the notes
// earlier landings had appended to this file live in `all/appended_notes.rs`.
// Single integration-test target for the router: the CANONICAL DECLARATION LIST of every
// at the top level of `tests/` fails the build; and a suite declared twice is `E0428`, which
// `mod <name>;` would resolve to a SIBLING of this file: the `#[path]` above each `mod` is
// what points the declaration into `tests/all/`.
// Three suites check those rules rather than restating them here:
// `all/all_rs_is_a_canonical_declaration_list.rs`. What a suite covers, and why it belongs
// to this cross-service target rather than to one service crate's own, is stated in that
// suite's OWN module header, beside the test it explains; a note about THIS file belongs in
// The lint allowed below is for the suite files this root compiles: a wrapped paragraph in a
// module header is a note rather than a build failure. An inner attribute, like an inner doc
// comment, is legal only before the first item, so it stands here rather than in the list.
//! Put a new suite at `tests/all/<name>.rs` and insert its `#[path = "all/<name>.rs"]` /
//! `mod <name>;` pair into the sorted list below. This file holds this header and those
//! pairs, and no other line — so two branches landing at once insert two lines in two
//! places and have no prose to duplicate, re-word or mistype.
//! THIS HEADER IS THE ONLY PLACE A `//!` LINE MAY STAND IN THIS FILE: an inner doc comment
//! is legal only before the first item, so one written past the first declaration is
//! `E0753` and compiles no suite of this target. A suite declared twice is `E0428`, and a
//! suite file no `mod` names is compiled nowhere.
//! `all/all_rs_is_a_canonical_declaration_list.rs`,
//! service crate's `tests/all.rs`, so check there rather than leaving a note below.
//! The `#[path]` attribute is what points each declaration into `tests/all/`: this file is
//! a test-target CRATE ROOT, where a bare `mod` would resolve to a sibling of it, which the
//! layout check refuses. What a suite covers, and why it drives the assembled router rather
//! than one service crate, belongs in that suite's own module doc; a note about this file
//! itself belongs in `all/appended_notes.rs`.
// `mod <name>;` pair into the sorted list below. Below this header the file holds those
// pairs and NO other line, so two branches landing at once insert two lines in two places
// deliberate: an inner doc comment (`//!`) is legal only before the first item, so with
// none anywhere, no merge of two landings can leave one past the first item, where it is
// `E0753` and compiles no suite of this target at all.
// `all/declaration_roots_are_canonical_and_complete.rs` check that shape, for this file and
// for every service crate's `tests/all.rs`, so a note about those rules belongs there
// rather than here. What a suite covers, and why it drives the assembled router rather than
// one service crate, belongs in that suite's own module header; the notes earlier landings
// appended to the end of this file live in `all/appended_notes.rs`.
// explains; a note about this file itself belongs in `all/appended_notes.rs`.
// lines in two places and have no prose to duplicate, re-word or mistype. Put a new suite
// at `tests/all/<name>.rs` and insert its pair in order; what that suite covers, and why it
// drives the assembled router rather than one service crate, belongs in its own module
// header, beside the test it explains.
// EVERY comment in this file is an ORDINARY `//` one, this header included: an inner doc
// comment (`//!`) is legal only before the first item, so one appended past the list is
// `E0753` and compiles no suite of this target. A second `mod` for one name is `E0428`, and
// a suite no `mod` names compiles nowhere and its tests never run. Each of those fails every
// suite in this target, and `all/all_rs_declares_each_suite_once.rs`,
// declaration is what points it into `tests/all/`.
// This file holds the license line, this header, and then one
// name, and no other line. Put a new suite at `tests/all/<name>.rs` and insert its pair in
// sorted position; what the suite covers belongs in that suite's own module header, where it
// is read beside the test it explains.
// Every comment in this file is an ordinary `//` one, this header included: an inner doc
// is `E0753`, which compiles no suite of this target. With none in the file, a landing that
// appends to the tail cannot leave one behind the first declaration.
// A suite no `mod` names compiles nowhere and its tests never run; a suite declared twice is
// `E0428`; and `tests/all.rs` is a test-target crate root, so a bare `mod` resolves to a
// sibling of this file, which the layout check refuses — the `#[path]` is what points each
// declaration into `tests/all/`. `all/all_rs_declares_each_suite_once.rs`,
// `all/every_suite_file_is_declared.rs` check that shape.
// name, and NO other line. Put a new suite at `tests/all/<name>.rs` and insert its pair into
// the sorted list; what a suite covers, and why it belongs to this cross-service target
// rather than to one service crate's own, is stated in that suite's own module doc, and a
// note about THIS file belongs in `all/appended_notes.rs`, which is declared below like any
// other module.
// comment (`//!`) past the first item is `E0753`. Each of those fails EVERY suite of this
// pin the shape above. `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` resolves
// to a SIBLING of this file, which the layout check refuses: the `#[path]` is what points
// deliberate: with no `//!` anywhere, no merge of two landings can leave one past the first
// item.
//! This file is a CANONICAL DECLARATION LIST: this header, then one
//! `#[path = "all/<name>.rs"]` / `mod <name>;` pair per suite under `tests/all/`, sorted by
//! name, and no other line. Two branches landing at once then insert two lines in two places
//! and have no prose to duplicate, re-word or mistype.
//! Put a new suite at `tests/all/<name>.rs` and insert its pair in sorted position. What that
//! suite covers — and why it drives the assembled router rather than one service crate —
//! belongs in that suite's OWN module header, where it is read beside the test it explains;
//! a note about this file belongs in `all/appended_notes.rs`.
//! Three rules fail every suite of this target rather than one, which is why nothing else
//! stands here: a suite no `mod` names compiles nowhere and its tests never run
//! (`all/every_suite_file_is_declared.rs`), a suite declared twice is `E0428`
//! (`all/all_rs_declares_each_suite_once.rs`), and an inner doc comment is legal only BEFORE
//! the first item, so one appended past the declarations is `E0753`
//! (`all/all_rs_inner_doc_comments_precede_the_first_item.rs`).
//! `all/all_rs_is_a_canonical_declaration_list.rs` pins the shape above, for this file and for
// Every comment in this file is an ordinary one, this block included: a doc comment of the
// enclosing module is legal only before the first item, so one appended past the
// declarations compiles no suite of this target at all, and with none anywhere no merge of
// two landings can leave one there.
// A suite no `mod` here names is compiled nowhere and its tests never run; a suite left at
// the top level of `tests/` fails the build; and one name declared twice compiles neither
// copy. `all/all_rs_declares_each_suite_once.rs`,
// service crate's own `tests/all.rs`. What a suite covers, and why it drives the assembled
// router rather than one service crate, belongs in that suite's own module header;
// `all/appended_notes.rs` keeps the notes earlier landings had appended here.
// `tests/all.rs` is a test-target crate root, so a bare `mod <name>;` would resolve to a
// sibling of this file, which the layout check refuses: the `#[path]` above each declaration
// `mod <name>;` pair into the sorted list below. One pair per suite and no other line, so two
// re-word or mistype. NOTHING follows the last pair.
// A suite no `mod` here names is compiled nowhere and its tests never run (this crate's build
// script refuses it); a suite left at the top level of `tests/` fails the build; and a suite
// declared twice is `E0428`. Each of those takes EVERY suite of this target down with it.
// the declarations is `E0753` and compiles no suite of this target — and with no `//!`
// anywhere here, no merge of two landings can leave one there.
// Three suites check those rules rather than restating them:
// `all/all_rs_is_a_canonical_declaration_list.rs`, for this file and for every service
// crate's own `tests/all.rs`. What a suite covers, and why it lives in this cross-service
// target rather than in one service crate's own, belongs in that suite's own module header;
// a note about THIS file belongs in `all/appended_notes.rs`.
// `mod <name>;` pair per suite under `tests/all/`, sorted by name, and NO other line. Put a
// new suite at `tests/all/<name>.rs` and insert its pair in order; what a suite covers, and
// why it belongs to this cross-service target rather than to one service crate's own, is
// stated in that suite's own module doc, and a note about THIS file belongs in
// `all/appended_notes.rs`, which is declared below like any other module and holds comments
// alone.
// `E0428`; a line between two pairs is a report from the canonical-list suite; and an inner
// doc comment (`//!`) past the first item is `E0753`. Each of those fails EVERY suite of this
// cross-service suite under `tests/all/`, and nothing else. Below this header the file holds
// one `#[path = "all/<name>.rs"]` / `mod <name>;` pair per suite file, sorted by name, and no
// `E0753` and compiles no suite of this target — which is what repeated landings kept
// producing here. With no inner doc comment in the file at all, no merge can leave one past
// order; what a suite covers, and why it belongs to this cross-service target rather than to
// one service crate's own, is stated in that suite's own module doc, and a note about THIS
// file belongs in `all/appended_notes.rs`, which is declared below like any other module and
// holds comments alone.
// comment is legal only before the first item, so one written past the declarations is
// `E0753`. Each of those fails EVERY suite of this target, which is why
// `all/every_suite_file_is_declared.rs` pin the shape above. EVERY comment in this file is an
// ordinary one, this header included, and that is deliberate: with none of the other form
// anywhere, no merge of two landings can leave one past the first item.
// (`tests/<name>.rs`), which the layout check refuses: the `#[path]` is what points each
// Below this block the file holds one `#[path = "all/<name>.rs"]` / `mod <name>;` pair per
// suite file under `tests/all/`, sorted by name, and no other line — no prose between the
// pairs, no blank line, no second declaration of a name. Two branches landing at once then
// insert two lines in two places and have nothing to duplicate, re-word or mistype.
// the declarations fails every suite of this target. With none anywhere, no merge of two
// landings can leave one past the first item.
// A suite no `mod` here names is compiled nowhere and its tests never run, and a second `mod`
// for one name compiles neither copy.
// Put a new suite at `tests/all/<name>.rs` and insert its pair in sorted position. What a
// suite covers, and why it drives the assembled router rather than one service crate, belongs
// in that suite's own module header; `all/appended_notes.rs` keeps the notes this file had
// collected.
// is `E0753` and compiles no suite of this target — which is what had happened here, where
// the tail had grown hundreds of re-typed copies of this header. With no inner doc comment in
// the file at all, there is none to copy.
// name is `E0428`, which fails every suite of this target too — and declare every suite file,
// since one no `mod` names compiles nowhere and the build-time layout check refuses it.
// `all/all_rs_is_a_canonical_declaration_list.rs` check the rules above rather than restating
// them.
// tail had grown hundreds of re-typed copies of this header. With no inner doc comment in the
// file at all, no merge of two landings can put one past the first item.
// declaration into `tests/all/`. Declare each suite exactly ONCE — a second `mod` for one name
// is `E0428` and fails every suite in this target too.
//! Single integration-test target for the router: the declaration list of its cross-service
//! suites, and nothing else.
//! One `#[path = "all/<name>.rs"]` / `mod <name>;` pair per file under `tests/all/`, sorted by
//! name, with no line between the pairs and none after the last one — so two branches landing
//! at once insert two lines in two places and have no prose to duplicate, re-word or mistype.
//! An inner doc comment is legal only before the first item, so this header is the only place
//! one may stand: past the first item it is `E0753` and compiles no suite of this target. A
//! second pair for one name is `E0428`, and a suite file no pair names compiles nowhere.
//! What a suite covers, and why it drives the assembled router rather than one service crate,
//! belongs in that suite's own module header; the notes earlier landings had appended to the
//! tail of this file live in `all/appended_notes.rs`.
// The note paragraphs concurrent landings had appended to this file wrapped without the
// indentation `clippy::doc_lazy_continuation` asks of a doc list item; the lint is allowed
// here so a re-appended note is a note rather than a build failure.
// cross-service suites.
// so two branches landing at once insert two lines in two places and have no prose to
// duplicate, re-word or mistype. Put a new suite at `tests/all/<name>.rs` and insert its
// pair in sorted position.
// to a SIBLING of this file, which the layout check refuses: the `#[path]` above each `mod` is
// what points it into `tests/all/`.
// test it explains; the notes earlier landings appended to this file live in
// the declarations is `E0753` and compiles no suite of this target. With no inner doc comment
// in the file at all, no merge of two landings can leave one past the first item.
// Single integration-test target for the router: the DECLARATION LIST, and nothing
// else.
// deliberate: an inner doc comment (`//` followed by `!`) is legal only before the
// first item, so one that lands past the declarations is `E0753` and compiles no
// suite of this target. With none anywhere in the file, no merge of two landings can
// put one past the first item.
// The list below is DECLARATIONS ONLY: one `#[path = "all/<name>.rs"]` / `mod
// <name>;` pair per suite file under `tests/all/`, sorted by name, and no other line
// — and NOTHING after the last pair — so two branches landing at once insert two
// `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` would resolve to a
// SIBLING of this file, which the layout check refuses: the `#[path]` is what points
// each declaration into `tests/all/`. Declare each suite exactly ONCE: a second `mod`
// for one name is `E0428` and fails every suite here.
// WHAT a suite covers, and WHY it belongs to this cross-service target rather than to
// one service crate's own, is stated in that suite's own module doc, where it is read
// beside the test it explains. A note about this file itself belongs in
// `all/appended_notes.rs`. Four suites pin the rules above rather than restating
// name. One pair per suite and no other line, so two branches landing at once insert two
// the file, no merge of two landings can leave one past the first item.
// Five suites pin the rules rather than restating them here:
// `all/all_rs_carries_no_inner_doc_comment.rs`,
// `all/all_rs_is_a_canonical_declaration_list.rs` (pairs, sorted, no other line) and
// `all/every_suite_file_is_declared.rs` (a suite file no `mod` names compiles nowhere).
// service crate's own, is stated in that suite's OWN module doc, beside the test it
// explains; `all/appended_notes.rs` keeps the notes earlier landings had appended here.
#[path = "all/cloudformation_cloudfront_published_edge_address.rs"]
mod cloudformation_cloudfront_published_edge_address;
// `#[path = "all/<name>.rs"]` / `mod <name>;` pair per file under `tests/all/`, sorted by
// name, and no other line. Add a suite by putting it at `tests/all/<name>.rs` and inserting
// its pair into the sorted list; what that suite covers, and why it drives the assembled
// router rather than one service crate, belongs in its OWN module header, where it is read
// beside the test it explains, and a note about THIS file belongs in
// This is the one file in the target whose breakage compiles no suite at all, so sibling
// suites pin the shape above: `all/all_rs_is_a_canonical_declaration_list.rs` (one pair per
// suite, sorted, no other line), `all/all_rs_declares_each_suite_once.rs` (a second `mod`
// for one name is `E0428`), `all/all_rs_inner_doc_comments_precede_the_first_item.rs` (an
// inner doc comment past the first item is `E0753`),
// `all/declaration_roots_are_canonical_and_complete.rs` (a file or module directory no
// declaration names is compiled nowhere). EVERY comment in this file is an ordinary `//`
// one, this header included, so no merge of two landings can leave a doc comment past the
// harnesses or notes rather than suites; all are declared, because a file no declaration
// Single integration-test target for the router: the declaration list of every cross-service
// suite under `tests/all/`, and no other line.
// Put a new suite at `tests/all/<name>.rs` and insert one `#[path = "all/<name>.rs"]` /
// `mod <name>;` pair below, in the place its name sorts to. What that suite covers belongs in
// its own module doc, beside the test it explains; a note about THIS file belongs in
// EVERY comment here is an ordinary `//` one, this header included, so no appended, re-typed
// or merged copy of a line of it can be an inner doc comment past the first item. The rules
// this root is held to are checked by `all/all_rs_carries_no_inner_doc_comment.rs`,
// `all/every_suite_file_is_declared.rs` rather than restated here.
// Single integration-test target for the router: the CANONICAL DECLARATION LIST of its
// cross-service suites, and nothing else.
//
// `tests/all/<name>.rs` and insert its pair into the sorted list — nothing beside it, so two
// branches landing at once insert two lines in two places and have no prose to duplicate,
// re-word or mistype.
// EVERY comment in this file is an ORDINARY `//` one, this header included, and that is
// deliberate: an inner doc comment (`//!`) is legal only before the first item, so one that
// lands past the declarations — which is what concurrent landings appending notes to this
// file keep producing — is `E0753` and compiles no suite of this target. With no `//!`
// anywhere, no merge of two landings can put one past the first item.
// `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod <name>;` would resolve to a
// SIBLING of this file (`tests/<name>.rs`), which the layout check refuses: the `#[path]`
// above each `mod` is what points the declaration into `tests/all/`. A suite no `mod` names
// compiles nowhere and its tests never run, and a suite declared twice is `E0428` — each
// fails every suite in this target, which is why five suites pin the shape above rather than
// restating it here: `all/all_rs_carries_no_inner_doc_comment.rs`,
// `all/all_rs_declares_each_suite_once.rs`,
// `all/all_rs_is_a_canonical_declaration_list.rs` and `all/every_suite_file_is_declared.rs`.
// WHAT a suite covers, and WHY it belongs to this cross-service target rather than to one
// service crate's own, is stated in that suite's own module doc, where it is read beside the
// test it explains; `all/appended_notes.rs` and `all/appended_notes_tail.rs` keep the notes
// earlier landings had appended here.
// Single integration-test target for the router: the canonical declaration list of every
// cross-service suite under `tests/all/`, and nothing else.
// suite, sorted by name, and no other line, so two branches landing at once insert two lines
// in two places and have no prose to duplicate, re-word or mistype.
// deliberate: an inner doc comment is legal only before the first item, so one appended past
// the declarations is `E0753` and compiles no suite of this target. With none in the file at
// all, no landing can leave one past the first item.
// declaration into `tests/all/`. Declare each suite exactly ONCE — a second `mod` for one
// name is `E0428`, and a suite file no `mod` here names compiles nowhere.
// The suites under `tests/all/` whose names begin `all_rs_`, plus
// `all/declaration_roots_are_canonical_and_complete.rs`, pin those rules rather than
// restating them here.
// What a suite covers, and why it belongs to this cross-service target rather than to one
// service crate's own, belongs in that suite's own module header, where it is read beside the
// test it explains; a note about THIS file belongs in `all/appended_notes.rs`.
#![allow(clippy::doc_lazy_continuation)]
// Single integration-test target for the router: the DECLARATION LIST, and nothing else.
// EVERY comment in this file is an ORDINARY one, this header included, and that is
// keep producing — is `E0753` and compiles no suite of this target. With no inner doc
// comment anywhere in the file, no merge of two landings can put one past the first item,
// because there is no such line to copy; `all/all_rs_carries_no_inner_doc_comment.rs` pins
// exactly that.
// The list below holds one `#[path = "all/<name>.rs"]` / `mod <name>;` pair per suite file
// under `tests/all/`, sorted by name, and no other line: a landing inserts its two lines in
// one place and has no prose to duplicate, re-word or mistype. `tests/all.rs` is a
// test-target CRATE ROOT, so a bare `mod` would resolve to a SIBLING (`tests/<name>.rs`),
// which the layout check refuses; the `#[path]` is what points each declaration into
// `tests/all/`. Declare each suite exactly ONCE: a second `mod` for one name is `E0428` and
// fails every suite in this target.
// test it explains. The notes earlier landings appended to this file live in
// `all/appended_notes.rs` and `all/appended_notes_tail.rs`, and these suites check the rules
// above rather than restating them: `all/all_rs_carries_no_inner_doc_comment.rs`,
// `all/every_suite_file_is_declared.rs`.
// A suite's own module doc may wrap a list item without the indentation
// `clippy::doc_lazy_continuation` asks for, so the lint is allowed for this whole test
// target here. An inner ATTRIBUTE, like an inner doc comment, is legal only before the first
// item, which is why it stands in this header rather than in the list below.
