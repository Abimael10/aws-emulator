// SPDX-License-Identifier: CC0-1.0

//! What the OWNING service reports a provisioned resource's configuration to be now: the
//! per-type read arm drift detection compares a stack's expected configuration against.
//!
//! One concern of [`super`], split out of that file so the dispatch enum and the create,
//! update and delete arms stay inside the line bound this project holds a file to.

use super::*;

impl Provisioner {
    /// What the service that OWNS a resource reports the resource's configuration to be
    /// NOW: the document its own read answers with. This is the ACTUAL configuration
    /// [`super::drift`] compares a resource's expected template configuration against, so
    /// drift is read off the owning service rather than off CloudFormation's own record of
    /// what it once applied.
    ///
    /// `Ok(None)` is that service answering that it no longer holds the resource — judged
    /// on its own not-found CODE, never on a message, exactly as [`Self::delete`] does —
    /// which is the `DELETED` drift status. `Err(refusal)` is any other refusal: the check
    /// did not run, and the detection reports that rather than a resource as checked.
    pub(crate) async fn actual_configuration(
        self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        resource: &StoredResource,
    ) -> Result<Option<Value>, Refusal> {
        let physical_resource_id = &resource.physical_resource_id;
        match self {
            Self::FirehoseDeliveryStream => {
                describe_delivery_stream(ctx, region, physical_resource_id).await
            }
            Self::KinesisStream => {
                describe_kinesis_stream(ctx, region, physical_resource_id).await
            }
            Self::CloudWatchMetricStream => {
                describe_metric_stream(ctx, region, physical_resource_id).await
            }
            Self::CloudWatchDashboard => {
                describe_dashboard(ctx, region, physical_resource_id).await
            }
            // CloudWatch's own read of the alarm the stack created, by the name it was
            // created under.
            Self::CloudWatchAlarm => {
                describe_metric_alarm(ctx, region, physical_resource_id).await
            }
            // CloudWatch's own read of the composite alarm the stack created, by the name it
            // was created under.
            Self::CloudWatchCompositeAlarm => {
                describe_composite_alarm(ctx, region, physical_resource_id).await
            }
            // Amazon ECS's own read of the service the stack created, by its name inside the
            // cluster it runs on.
            Self::EcsService => Box::pin(describe_ecs_service(ctx, region, resource)).await,
            Self::EcrPublicRepository => {
                describe_public_repository(ctx, region, physical_resource_id).await
            }
            Self::EcrRepository => describe_repository(ctx, region, physical_resource_id).await,
            Self::Ec2Vpc => describe_ec2_vpc(ctx, region, physical_resource_id).await,
            Self::Ec2Subnet => describe_ec2_subnet(ctx, region, physical_resource_id).await,
            Self::Ec2SecurityGroup => {
                describe_ec2_security_group(ctx, region, physical_resource_id).await
            }
            Self::Ec2Volume => describe_ec2_volume(ctx, region, physical_resource_id).await,
            Self::Ec2LaunchTemplate => {
                describe_ec2_launch_template(ctx, region, physical_resource_id).await
            }
            Self::AutoScalingGroup => {
                describe_auto_scaling_group(ctx, region, physical_resource_id).await
            }
            // Read back by the group and the name the policy was created under, which the
            // resource's own record carries.
            Self::AutoScalingScalingPolicy => {
                describe_scaling_policy(
                    ctx,
                    region,
                    &resource.properties,
                    &declared_name(resource),
                )
                .await
            }
            Self::S3Bucket => describe_bucket(ctx, region, physical_resource_id).await,
            // A policy resource's configuration is the document the named bucket carries
            // now, read back from Amazon S3.
            Self::S3BucketPolicy => describe_bucket_policy(ctx, region, resource).await,
            Self::S3TablesTableBucket => {
                describe_table_bucket(ctx, region, physical_resource_id).await
            }
            // Read back by the same pair the delete addresses the namespace by.
            Self::S3TablesNamespace => describe_namespace(ctx, region, resource).await,
            Self::S3VectorsVectorBucket => {
                describe_vector_bucket(ctx, region, physical_resource_id).await
            }
            Self::S3VectorsIndex => describe_index(ctx, region, physical_resource_id).await,
            Self::EmrCluster => describe_job_flow(ctx, region, physical_resource_id).await,
            Self::IamRole => describe_role(ctx, region, physical_resource_id).await,
            Self::IamInstanceProfile => {
                describe_instance_profile(ctx, region, physical_resource_id).await
            }
            Self::IamUser => describe_iam_user(ctx, region, physical_resource_id).await,
            // Read back from the user's own list of keys, IAM publishing no read of ONE
            // access key.
            Self::IamAccessKey => describe_iam_access_key(ctx, region, resource).await,
            Self::IamManagedPolicy => {
                describe_managed_policy(ctx, region, physical_resource_id).await
            }
            Self::CodePipelinePipeline => {
                describe_pipeline(ctx, region, physical_resource_id).await
            }
            // Read back by the same triple the delete addresses the registration by.
            Self::CodePipelineCustomActionType => {
                describe_custom_action_type(ctx, region, resource).await
            }
            // Read back out of the account's own webhook listing, CodePipeline publishing no
            // read of ONE webhook.
            Self::CodePipelineWebhook => {
                describe_webhook(ctx, region, physical_resource_id).await
            }
            Self::EcsCluster => describe_cluster(ctx, region, physical_resource_id).await,
            // Amazon ECS's own read of the capacity provider the stack created, by the name
            // it was created under.
            Self::EcsCapacityProvider => {
                describe_ecs_capacity_provider(ctx, region, physical_resource_id).await
            }
            Self::SqsQueue => describe_queue(ctx, region, physical_resource_id).await,
            // A policy resource's configuration is the `Policy` attribute of the queues it
            // names, which is read back from Amazon SQS as the document those queues carry
            // now.
            Self::SqsQueuePolicy => describe_queue_policy(ctx, region, resource).await,
            Self::OpenSearchServerlessCollection => {
                describe_collection(ctx, region, physical_resource_id).await
            }
            Self::OpenSearchServerlessSecurityPolicy => {
                describe_security_policy(ctx, region, resource).await
            }
            // The endpoint as that service reports it, read by the id it minted and
            // reported under the property names the type spells its members with.
            Self::OpenSearchServerlessVpcEndpoint => {
                describe_vpc_endpoint(ctx, region, physical_resource_id).await
            }
            Self::OpenSearchServiceDomain => {
                describe_opensearch_domain(ctx, region, physical_resource_id).await
            }
            Self::SageMakerModelPackageGroup => {
                describe_model_package_group(ctx, region, physical_resource_id).await
            }
            // SageMaker's own reads of the three resources, each by the name it was created
            // under, reported under the property names the type spells its members with.
            Self::SageMakerModel => {
                describe_sagemaker_model(ctx, region, physical_resource_id).await
            }
            Self::SageMakerEndpointConfig => {
                describe_sagemaker_endpoint_config(ctx, region, physical_resource_id).await
            }
            Self::SageMakerEndpoint => {
                describe_sagemaker_endpoint(ctx, region, physical_resource_id).await
            }
            Self::SageMakerNotebookInstance => {
                describe_sagemaker_notebook_instance(ctx, region, physical_resource_id).await
            }
            Self::SageMakerNotebookInstanceLifecycleConfig => {
                describe_sagemaker_notebook_instance_lifecycle_config(
                    ctx,
                    region,
                    physical_resource_id,
                )
                .await
            }
            Self::SnsTopic => describe_topic(ctx, region, physical_resource_id).await,
            // A policy resource's configuration is the `Policy` attribute of the topics it
            // names, which is read back from Amazon SNS as the document those topics carry
            // now.
            Self::SnsTopicPolicy => describe_topic_policy(ctx, region, resource).await,
            Self::SnsSubscription => describe_subscription(ctx, region, resource).await,
            Self::FisExperimentTemplate => {
                describe_experiment_template(ctx, region, physical_resource_id).await
            }
            Self::WafRegionalIpSet => describe_ip_set(ctx, region, physical_resource_id).await,
            // Read back by the same triple the delete addresses the entity by.
            Self::Wafv2WebAcl => {
                describe_wafv2_entity(ctx, region, resource, WAFV2_WEB_ACL_CALLS).await
            }
            Self::Wafv2RuleGroup => {
                describe_wafv2_entity(ctx, region, resource, WAFV2_RULE_GROUP_CALLS).await
            }
            Self::Wafv2IpSet => {
                describe_wafv2_entity(ctx, region, resource, WAFV2_IP_SET_CALLS).await
            }
            Self::Wafv2RegexPatternSet => {
                describe_wafv2_entity(ctx, region, resource, WAFV2_REGEX_PATTERN_SET_CALLS).await
            }
            // Read back as the web ACL AWS WAF reports in front of the protected resource
            // now, which is what the declared pair is compared against.
            Self::Wafv2WebAclAssociation => {
                describe_web_acl_association(ctx, region, resource).await
            }
            Self::GlueJob => describe_glue_job(ctx, region, physical_resource_id).await,
            // Read back by the same handles the deletes address the two catalog entities by,
            // and reported as the type's own properties: a database's definition is the
            // `DatabaseInput` property, a table's the `TableInput` one.
            Self::GlueDatabase => describe_glue_database(ctx, region, resource).await,
            Self::GlueTable => describe_glue_table(ctx, region, resource).await,
            Self::GlueCrawler => describe_glue_crawler(ctx, region, physical_resource_id).await,
            Self::GlueTableOptimizer => {
                describe_glue_table_optimizer(ctx, region, resource).await
            }
            Self::GrafanaWorkspace => {
                describe_workspace(ctx, region, physical_resource_id).await
            }
            Self::GuardDutyDetector => describe_detector(ctx, region, physical_resource_id).await,
            // Read back by the same pair the delete addresses the list by.
            Self::GuardDutyIpSet => describe_guardduty_ip_set(ctx, region, resource).await,
            Self::KmsKey => describe_kms_key(ctx, region, physical_resource_id).await,
            Self::KmsAlias => describe_kms_alias(ctx, region, physical_resource_id).await,
            // One `DescribeClusterV2` reads a cluster of either kind back.
            Self::MskCluster | Self::MskServerlessCluster => {
                describe_msk_cluster(ctx, region, physical_resource_id).await
            }
            Self::MqBroker => describe_mq_broker(ctx, region, physical_resource_id).await,
            Self::RdsDbInstance => {
                describe_db_instance(ctx, region, physical_resource_id).await
            }
            // The cluster is read back by the identifier it was created under, which is this
            // resource's physical id.
            Self::RdsDbCluster => {
                Box::pin(read_db_cluster(ctx, region, physical_resource_id)).await
            }
            Self::RdsDbSubnetGroup => {
                describe_db_subnet_group(ctx, region, physical_resource_id).await
            }
            Self::RdsDbParameterGroup => {
                describe_parameter_group(
                    ctx,
                    region,
                    physical_resource_id,
                    DB_PARAMETER_GROUP_CALLS,
                )
                .await
            }
            Self::RdsDbClusterParameterGroup => {
                describe_parameter_group(
                    ctx,
                    region,
                    physical_resource_id,
                    DB_CLUSTER_PARAMETER_GROUP_CALLS,
                )
                .await
            }
            // Amazon RDS's own read of the proxy, by the name it was created under.
            Self::RdsDbProxy => read_db_proxy(ctx, region, physical_resource_id).await,
            // Read back by the same proxy-and-group pair the delete addresses the
            // registrations by, which only the resource's own declaration carries.
            Self::RdsDbProxyTargetGroup => {
                describe_db_proxy_target_group(ctx, region, resource).await
            }
            // The read is addressed by the rule's name AND the bus it is bound to, which
            // only the resource's own record carries.
            Self::EventsRule => describe_rule(ctx, region, resource).await,
            // EventBridge's own read of the bus the stack created, by the name it was created
            // under.
            Self::EventsEventBus => {
                event_buses::describe_event_bus(ctx, region, physical_resource_id).await
            }
            // A grant is read out of the POLICY the bus it was installed on carries now, by
            // the statement id that identifies it.
            Self::EventsEventBusPolicy => {
                event_buses::describe_event_bus_policy(ctx, region, resource).await
            }
            Self::SsmParameter => describe_parameter(ctx, region, physical_resource_id).await,
            // Systems Manager's own read of the association the stack created, by the id it
            // minted for it.
            Self::SsmAssociation => {
                describe_ssm_association(ctx, region, physical_resource_id).await
            }
            // Systems Manager's own read of the window the stack created, by the id it minted
            // for it — and of the target, out of that window's own listing, which is the one
            // read that service publishes for a target.
            Self::SsmMaintenanceWindow => {
                describe_ssm_maintenance_window(ctx, region, physical_resource_id).await
            }
            Self::SsmMaintenanceWindowTarget => {
                describe_ssm_maintenance_window_target(ctx, region, resource).await
            }
            // Systems Manager's own read of the document the stack created, by the name it was
            // created under.
            Self::SsmDocument => {
                describe_ssm_document(ctx, region, physical_resource_id).await
            }
            Self::StepFunctionsActivity => {
                describe_activity(ctx, region, physical_resource_id).await
            }
            Self::SchedulerScheduleGroup => {
                describe_schedule_group(ctx, region, physical_resource_id).await
            }
            // Resource Groups' own read of the group, by the name it was created under.
            Self::ResourceGroupsGroup => {
                describe_resource_group(ctx, region, physical_resource_id).await
            }
            // The read is addressed by the schedule's name AND its group, which only the
            // resource's own record carries.
            Self::SchedulerSchedule => describe_schedule(ctx, region, resource).await,
            Self::PipesPipe => {
                event_pipes::describe_pipe(ctx, region, physical_resource_id).await
            }
            // The read is addressed by the group's name AND the pool that holds it, which
            // only the resource's own record carries.
            Self::CognitoUserPoolGroup => describe_group(ctx, region, resource).await,
            Self::AcmAcmeEndpoint => {
                describe_acme_endpoint(ctx, region, physical_resource_id).await
            }
            Self::AcmCertificate => describe_certificate(ctx, region, physical_resource_id).await,
            Self::AcmPcaCertificateAuthority => {
                describe_certificate_authority(ctx, region, physical_resource_id).await
            }
            // None of the properties an `AWS::ACMPCA::Certificate` declares — the
            // certification request it had signed, the template, the validity — is reported
            // by any read Amazon Web Services Private CA publishes for an issued
            // certificate, so what the check can establish about one is whether that
            // service still holds it.
            Self::AcmPcaCertificate => {
                issued_certificate_configuration(ctx, region, physical_resource_id).await
            }
            // An activation's one declared property that is also READ back is the status it
            // left the CA in, which `DescribeCertificateAuthority` reports.
            Self::AcmPcaCertificateAuthorityActivation => {
                activation_configuration(ctx, region, physical_resource_id).await
            }
            // A grant is read out of the CA's own listing of its permissions, by the
            // principal this record granted to.
            Self::AcmPcaPermission => describe_ca_permission(ctx, region, resource).await,
            Self::Route53ResolverEndpoint => {
                describe_resolver_endpoint(ctx, region, physical_resource_id).await
            }
            Self::Route53ResolverRule => {
                describe_resolver_rule(ctx, region, physical_resource_id).await
            }
            Self::Route53ResolverRuleAssociation => {
                describe_resolver_rule_association(ctx, region, physical_resource_id).await
            }
            Self::Route53ResolverFirewallDomainList => {
                describe_firewall_domain_list(ctx, region, physical_resource_id).await
            }
            Self::Route53ResolverFirewallRuleGroup => {
                describe_firewall_rule_group(ctx, region, physical_resource_id).await
            }
            Self::Route53ResolverFirewallRuleGroupAssociation => {
                describe_firewall_rule_group_association(ctx, region, physical_resource_id).await
            }
            Self::Route53HostedZone => {
                describe_hosted_zone(ctx, region, physical_resource_id).await
            }
            // The read is addressed by the record's own name, type and set identifier in the
            // zone its declaration names, which only the resource's own record carries.
            Self::Route53RecordSet => describe_record_set(ctx, region, resource).await,
            // Each record set the group declares, read back by its own name and type in the
            // zone the group names.
            Self::Route53RecordSetGroup => {
                describe_record_set_group(ctx, region, resource).await
            }
            // Read back by the id Route 53 minted, which is this resource's physical id.
            Self::Route53HealthCheck => describe_health_check(ctx, region, resource).await,
            // Read back by the identifier Route 53 minted for each, which is that
            // resource's physical id.
            Self::Route53CidrCollection => {
                describe_cidr_collection(ctx, region, physical_resource_id).await
            }
            Self::Route53DelegationSet => {
                describe_delegation_set(ctx, region, physical_resource_id).await
            }
            Self::Route53TrafficPolicy => {
                describe_traffic_policy(ctx, region, physical_resource_id).await
            }
            Self::ElasticBeanstalkApplication => {
                describe_eb_application(ctx, region, physical_resource_id).await
            }
            // The read is addressed by the version's label AND the application that
            // holds it, which only the resource's own record carries.
            Self::ElasticBeanstalkApplicationVersion => {
                describe_eb_application_version(ctx, region, resource).await
            }
            Self::ElasticBeanstalkEnvironment => {
                describe_eb_environment(ctx, region, physical_resource_id).await
            }
            // Read back by the assigned `ServerId`, like the delete.
            Self::TransferServer => describe_transfer_server(ctx, region, resource).await,
            Self::CodeDeployApplication => {
                describe_codedeploy_application(ctx, region, physical_resource_id).await
            }
            // The read is addressed by the group's name AND the application that holds it,
            // which only the resource's own record carries.
            Self::CodeDeployDeploymentGroup => {
                describe_codedeploy_deployment_group(ctx, region, resource).await
            }
            Self::CodeDeployDeploymentConfig => {
                describe_codedeploy_deployment_config(ctx, region, physical_resource_id).await
            }
            // Batch's readers take a LIST and answer the records they found, so "no longer
            // held" is an empty list rather than a modelled not-found code
            // ([`batch_described`]).
            Self::BatchComputeEnvironment => {
                batch_described(
                    ctx,
                    region,
                    BATCH_DESCRIBE_COMPUTE_ENVIRONMENTS_URI,
                    "computeEnvironments",
                    physical_resource_id,
                )
                .await
            }
            Self::BatchJobQueue => {
                batch_described(
                    ctx,
                    region,
                    BATCH_DESCRIBE_JOB_QUEUES_URI,
                    "jobQueues",
                    physical_resource_id,
                )
                .await
            }
            Self::BatchJobDefinition => {
                describe_batch_job_definition(ctx, region, physical_resource_id).await
            }
            Self::BatchServiceEnvironment => {
                batch_described(
                    ctx,
                    region,
                    BATCH_DESCRIBE_SERVICE_ENVIRONMENTS_URI,
                    "serviceEnvironments",
                    physical_resource_id,
                )
                .await
            }
            // Read back by the same cluster-and-principal pair the delete uses.
            Self::EksAccessEntry => describe_access_entry(ctx, region, resource).await,
            Self::EksCluster => describe_eks_cluster(ctx, region, physical_resource_id).await,
            // Read back by the same cluster-and-name pair the delete uses.
            Self::EksAddon => describe_eks_addon(ctx, region, resource).await,
            Self::EksNodegroup => describe_eks_nodegroup(ctx, region, resource).await,
            Self::EksPodIdentityAssociation => {
                describe_pod_identity_association(ctx, region, resource).await
            }
            Self::ApiGatewayDomainName => {
                describe_apigateway_domain_name(ctx, region, physical_resource_id).await
            }
            // Read back by the same domain-and-base-path pair the delete uses.
            Self::ApiGatewayBasePathMapping => {
                describe_apigateway_base_path_mapping(ctx, region, resource).await
            }
            Self::ApiGatewayV2Api => {
                describe_apigatewayv2_api(ctx, region, physical_resource_id).await
            }
            Self::ApiGatewayV2DomainName => {
                describe_apigatewayv2_domain_name(ctx, region, physical_resource_id).await
            }
            // Read back by the same domain-and-id pair the delete uses.
            Self::ApiGatewayV2ApiMapping => {
                describe_apigatewayv2_api_mapping(ctx, region, resource).await
            }
            // Read back by the same api-and-identifier pair each delete uses.
            Self::ApiGatewayV2Integration => {
                describe_apigatewayv2_child(ctx, region, resource, &APIGATEWAYV2_INTEGRATION_CHILD)
                    .await
            }
            Self::ApiGatewayV2Route => {
                describe_apigatewayv2_child(ctx, region, resource, &APIGATEWAYV2_ROUTE_CHILD).await
            }
            Self::ApiGatewayV2Stage => {
                describe_apigatewayv2_child(ctx, region, resource, &APIGATEWAYV2_STAGE_CHILD).await
            }
            Self::ApiGatewayV2Deployment => {
                describe_apigatewayv2_child(ctx, region, resource, &APIGATEWAYV2_DEPLOYMENT_CHILD)
                    .await
            }
            Self::ApiGatewayV2Authorizer => {
                describe_apigatewayv2_child(ctx, region, resource, &APIGATEWAYV2_AUTHORIZER_CHILD)
                    .await
            }
            Self::CodeCommitRepository => {
                describe_codecommit_repository(ctx, region, physical_resource_id).await
            }
            Self::CodeArtifactDomain => describe_codeartifact_domain(ctx, region, resource).await,
            Self::CodeArtifactRepository => {
                describe_codeartifact_repository(ctx, region, resource).await
            }
            // Read back by the same ARN the delete addresses the group by.
            Self::CodeBuildReportGroup => {
                describe_codebuild_report_group(ctx, region, resource).await
            }
            // Read back out of the batch read of projects, by the name the project was
            // created under.
            Self::CodeBuildProject => {
                describe_codebuild_project(ctx, region, physical_resource_id).await
            }
            Self::SesConfigurationSet => {
                describe_ses_configuration_set(ctx, region, physical_resource_id).await
            }
            // Read back by the same text the delete addresses the identity by.
            Self::SesEmailIdentity => {
                describe_ses_email_identity(ctx, region, physical_resource_id).await
            }
            Self::SesReceiptRuleSet => {
                describe_ses_receipt_rule_set(ctx, region, physical_resource_id).await
            }
            // Read back by the same set-and-rule pair the delete addresses the rule by.
            Self::SesReceiptRule => describe_ses_receipt_rule(ctx, region, resource).await,
            Self::BackupVault => {
                describe_backup_vault(ctx, region, physical_resource_id).await
            }
            Self::BackupPlan => describe_backup_plan(ctx, region, physical_resource_id).await,
            // Read back by the same plan-and-selection pair the delete uses.
            Self::BackupSelection => describe_backup_selection(ctx, region, resource).await,
            // Amazon Redshift's own read of the cluster the stack created, by the
            // identifier it was created under.
            Self::RedshiftCluster => {
                describe_redshift_cluster(ctx, region, physical_resource_id).await
            }
            // Amazon Bedrock's own read of the guardrail the stack created, by the
            // identifier it was created under.
            Self::BedrockGuardrail => {
                describe_bedrock_guardrail(ctx, region, physical_resource_id).await
            }
            // Amazon Bedrock Agent's own read of the knowledge base the stack created, by
            // the id it minted for it.
            Self::BedrockKnowledgeBase => {
                describe_bedrock_knowledge_base(ctx, region, physical_resource_id).await
            }
            // Amazon Bedrock Agent's own read of the data source the stack created, by the
            // knowledge base and the id it minted for it.
            Self::BedrockDataSource => {
                describe_bedrock_data_source(ctx, region, resource).await
            }
            // Amazon Bedrock Agent's own read of the flow the stack created, by the ARN it
            // minted for it.
            Self::BedrockFlow => describe_bedrock_flow(ctx, region, physical_resource_id).await,
            // Amazon Redshift's own read of the parameter group the stack created, by the
            // name it was created under.
            Self::RedshiftClusterParameterGroup => {
                describe_cluster_parameter_group(ctx, region, physical_resource_id).await
            }
            // Amazon Redshift's own read of the subnet group the stack created, by the name
            // it was created under.
            Self::RedshiftClusterSubnetGroup => {
                describe_cluster_subnet_group(ctx, region, physical_resource_id).await
            }
            // Amazon Redshift's own read of the snapshot copy grant the stack created, by the
            // name it was created under.
            Self::RedshiftSnapshotCopyGrant => {
                describe_snapshot_copy_grant(ctx, region, physical_resource_id).await
            }
            // The AgentCore control plane's own read of the identity, by the name it was
            // created under.
            Self::BedrockAgentCoreWorkloadIdentity => {
                describe_workload_identity(ctx, region, physical_resource_id).await
            }
            // Amazon Braket publishes no read of ONE spending limit, so the limit is read
            // out of the account's own listing, by the ARN that service minted for it.
            Self::BraketSpendingLimit => {
                describe_braket_spending_limit(ctx, region, physical_resource_id).await
            }
            // Clean Rooms ML's own read of the dataset, by the ARN it minted.
            Self::CleanRoomsMlTrainingDataset => {
                describe_training_dataset(ctx, region, physical_resource_id).await
            }
            // Amplify UI Builder's own read of the entity, by the id it minted, inside the app
            // environment the declaration names.
            Self::AmplifyUiBuilderComponent => {
                describe_ui_entity(ctx, region, UiEntity::Component, resource).await
            }
            Self::AmplifyUiBuilderForm => {
                describe_ui_entity(ctx, region, UiEntity::Form, resource).await
            }
            Self::AmplifyUiBuilderTheme => {
                describe_ui_entity(ctx, region, UiEntity::Theme, resource).await
            }
            // AWS Amplify's own read of the app, by the id it minted for it.
            Self::AmplifyApp => describe_amplify_app(ctx, region, resource).await,
            // AWS Amplify's own read of the branch, by the name it was created under inside the
            // app it belongs to.
            Self::AmplifyBranch => describe_amplify_branch(ctx, region, resource).await,
            // Amazon MWAA's own read of the environment, by the name it was created under.
            Self::MwaaEnvironment => {
                describe_mwaa_environment(ctx, region, physical_resource_id).await
            }
            // X-Ray publishes no read of ONE sampling rule, so the rule is read out of the
            // account's own listing, by the identity that service minted for it.
            Self::XraySamplingRule => describe_sampling_rule(ctx, region, resource).await,
            // X-Ray's own read of ONE group, by the identity that service minted for it.
            Self::XrayGroup => describe_xray_group(ctx, region, resource).await,
            // AWS Config's own reads of the three resources, each by the name it was
            // created under.
            Self::ConfigConfigurationRecorder => {
                describe_configuration_recorder(ctx, region, physical_resource_id).await
            }
            Self::ConfigDeliveryChannel => {
                describe_delivery_channel(ctx, region, physical_resource_id).await
            }
            Self::ConfigConfigRule => {
                describe_config_rule(ctx, region, physical_resource_id).await
            }
            // Amazon ElastiCache's own read of the cluster, by the identifier it was created
            // under. The cluster is reported with its SCALAR configuration — the engine, the
            // node type and count, the windows and the group names, each the member of the
            // same name the type declares — plus its configuration endpoint; a property that
            // is a LIST on the wire (the security groups, the log delivery configurations)
            // is left out, which is the position every arm whose property is not a scalar is
            // in and what [`super::drift`] documents.
            Self::ElastiCacheCacheCluster => {
                Box::pin(read_cache_cluster(ctx, region, physical_resource_id)).await
            }
            // Amazon MemoryDB's own read of the group, by the name it was created under.
            Self::MemoryDbGroup(group) => {
                describe_memorydb_group(ctx, region, group, physical_resource_id).await
            }
            // Amazon Connect Customer Profiles' own read of the domain, by the name it was
            // created under.
            Self::CustomerProfilesDomain => {
                describe_customer_profiles_domain(ctx, region, physical_resource_id).await
            }
            // Recycle Bin's own read of the rule, by the identifier it minted for it.
            Self::RbinRule => describe_rbin_rule(ctx, region, physical_resource_id).await,
            // Neptune Analytics' own reads of the graph and of the snapshot, each by the id it
            // minted for the resource.
            Self::NeptuneGraphGraph => {
                describe_neptune_graph(ctx, region, physical_resource_id).await
            }
            Self::NeptuneGraphGraphSnapshot => {
                describe_graph_snapshot(ctx, region, physical_resource_id).await
            }
            // Backup Gateway's own read of the hypervisor, by the ARN it minted for it.
            Self::BackupGatewayHypervisor => {
                describe_backup_gateway_hypervisor(ctx, region, physical_resource_id).await
            }
            // MediaConvert publishes no read of its certificate associations, so the trust is
            // read from the other side: the resources Certificate Manager reports as using
            // the certificate that is this resource's physical id.
            Self::MediaConvertCertificateAssociation => {
                describe_mediaconvert_certificate_association(ctx, region, physical_resource_id)
                    .await
            }
        }
    }
}
