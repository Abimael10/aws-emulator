// SPDX-License-Identifier: CC0-1.0

use super::*;

/// The SDK service NAME the diagnostic below names a called service by, per SIGNING name.
///
/// A resource provider calls the service that owns the type through an AWS SDK, and what it
/// reports when that call is refused is the SDK's exception message: the service's own
/// message followed by the SDK's own diagnostic of the call ([`sdk_diagnostic`]). The
/// service in that diagnostic is the SDK's name for it, which is NOT the signing name the
/// request was signed with — Amazon OpenSearch Service signs `es` and its SDK names it
/// `OpenSearch`, that model's own `sdkId`.
///
/// One entry per service whose reported wording this emulator pins; a service with no entry
/// has its message reported exactly as it worded it, with no diagnostic invented for it.
/// Each entry also carries the SHAPE that service's SDK words the diagnostic in
/// ([`SdkDiagnostic`]), which is the SDK's and cannot be derived from the service.
pub(super) const SDK_SERVICE_NAMES: &[(&str, &str, SdkDiagnostic)] = &[
    // Amazon Web Services Private CA's SDK names it `AWSACMPCA` — the name that service's own
    // SDK client carries, which is neither its signing name (`acm-pca`) nor its `sdkId` (`ACM
    // PCA`, `smithy_query(query="service_info")` on that service's model). The shape is the
    // PROXIED one, naming the error code and closing with the proxy the call was made through,
    // as the recorded `ResourceStatusReason` of an `AWS::ACMPCA::Permission` whose declared
    // principal that service does not grant to carries it (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, check
    // `acm-pca__cloudformation.deploy-cfn-acmpca-permission-invalid-principal-rejected`, whose
    // reason quotes `<the service's sentence> (Service: AWSACMPCA; Status Code: 400; Error
    // Code: ValidationException; Request ID: …; Proxy: null)`). The four resource types this
    // emulator provisions over this signing name are all that service's
    // ([`crate::state::provisioner::create_ca_permission`] and the arms beside it).
    ("acm-pca", "AWSACMPCA", SdkDiagnostic::Proxied),
    // Amazon API Gateway's SDK names it `ApiGateway` — that model's own
    // `awsService.cloudFormationName`, which is also the name its SDK client carries
    // (`smithy_query(query="service_info")` on that service's model: `sdkId: "API Gateway"`,
    // `cloudFormationName: ApiGateway`). The shape is the bare ENVELOPE, with no attempt
    // count after it, as the recorded `ResourceStatusReason` of an
    // `AWS::ApiGateway::Deployment` created for a REST API that has no methods carries it
    // (`"<the service's sentence> (Service: ApiGateway, Status Code: 400, Request ID: …)"`,
    // where Amazon Route 53's below carries `(SDK Attempt Count: …)` past the closing
    // parenthesis). This signing name is SHARED with API Gateway v2, whose own surface is
    // bound under a URI prefix of its own and is therefore named by its own SDK rather than
    // by this entry ([`SDK_SHARED_NAME_PREFIXES`]); this one answers for the REST control
    // plane's types ([`provisioning::apigateway_rest_api`]). The provider of the api ITSELF
    // words the ATTEMPT-counting shape, which is recorded per provider
    // ([`SDK_PROVIDER_SHAPES`]).
    (
        crate::state::provisioner::API_GATEWAY_SIGNING_NAME,
        "ApiGateway",
        SdkDiagnostic::Envelope,
    ),
    // AWS AppConfig's SDK names it by that model's own `sdkId`, which is also the
    // `cloudFormationName` the type names of its resources carry
    // (`smithy_query(query="service_info")` on that service's model: `sdkId: "AppConfig"`,
    // `cloudFormationName: AppConfig`). The shape is the bare ENVELOPE, with no attempt count
    // after it, as the recorded `ResourceStatusReason` of an `AWS::AppConfig::Application`
    // whose declared name is longer than that service's maximum carries it (`"<the service's
    // sentence> (Service: AppConfig, Status Code: 400, Request ID: …)"`, where Amazon Route 53's
    // below carries `(SDK Attempt Count: …)` past the closing parenthesis). The four resource
    // types this emulator provisions over this signing name are all that service's
    // ([`provisioning::appconfig_resources`]).
    ("appconfig", "AppConfig", SdkDiagnostic::Envelope),
    // Application Auto Scaling's SDK names it by its `sdkId` without the spaces, which is
    // also the `cloudFormationName` the type names of its resources carry
    // (`smithy_query(query="service_info")`).
    (
        "application-autoscaling",
        "ApplicationAutoScaling",
        SdkDiagnostic::Attempts,
    ),
    // CloudWatch Application Signals' SDK names it by that model's own `sdkId` VERBATIM, the
    // space included (`Application Signals`, `smithy_query(query="service_info")` on that
    // service's model), and words its diagnostic with the refused OPERATION and that service's
    // error code in it, as the recorded `ResourceStatusReason` of a failed
    // `AWS::ApplicationSignals::InstrumentationConfig` carries it (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, check `application-signals__cloudformation.deploy-\
    // application-signals-instrumentation-config-missing-code-unit-rejected`, whose reason
    // reads `<the service's sentence> (Service: Application Signals; Operation:
    // CreateInstrumentationConfiguration; Status Code: 400; …)`). The three resource types this
    // emulator provisions over this signing name are all that service's
    // ([`provisioner::application_signals`]).
    (
        "application-signals",
        "Application Signals",
        SdkDiagnostic::Operation,
    ),
    // Amazon CloudWatch Application Insights' SDK names it `ApplicationInsights` — that
    // model's own `awsService.cloudFormationName`, which is its `sdkId` (`Application
    // Insights`, `smithy_query(query="service_info")` on that service's model) with the space
    // closed up, as the name its SDK client carries. The shape is the ATTEMPT-counting one, as
    // the recorded `ResourceStatusReason` of an `AWS::ApplicationInsights::Application` over a
    // resource group the account does not hold carries it (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, check `application-insights__cloudformation__resource-\
    // groups.deploy-applicationinsights-application-missing-resource-group-rejected`, whose
    // reason reads `<the service's sentence> (Service: ApplicationInsights, Status Code: 400,
    // Request ID: …) (SDK Attempt Count: …)`). The one resource type this emulator provisions
    // over this signing name is that service's ([`provisioner::application_insights`]).
    (
        "applicationinsights",
        "ApplicationInsights",
        SdkDiagnostic::Attempts,
    ),
    // Amazon EC2 Auto Scaling's SDK names it `AutoScaling` — that model's own
    // `awsService.cloudFormationName`, which is its `sdkId` (`Auto Scaling`,
    // `smithy_query(query="service_info")` on that service's model) with the space closed up,
    // exactly as Application Auto Scaling's above is. The shape is the ATTEMPT-counting one, as
    // the recorded `ResourceStatusReason` of an `AWS::AutoScaling::AutoScalingGroup` whose
    // declared `DeletionProtection` that service does not accept, and of one whose deletion
    // protection blocks the delete, each carry it (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, checks
    // `autoscaling__cloudformation.deploy-cfn-asg-invalid-deletion-protection-value-rejected`
    // and
    // `autoscaling__cloudformation.deploy-cfn-asg-deletion-protection-blocks-delete-until-cleared`,
    // whose reasons read `Resource handler returned message: "<the service's sentence>
    // (Service: AutoScaling, Status Code: 400, Request ID: …) (SDK Attempt Count: …)"
    // (RequestToken: …, HandlerErrorCode: …)`). Every type this emulator provisions over this
    // signing name is that service's (the group and its scaling policy,
    // [`provisioner::compute_fleet`] and [`provisioner::auto_scaling_policies`]).
    ("autoscaling", "AutoScaling", SdkDiagnostic::Attempts),
    // Amazon Braket's SDK names it by that model's own `sdkId` (`Braket`), and words its
    // diagnostic with the refused OPERATION and that service's error code in it, as the
    // recorded `ResourceStatusReason` of a failed `AWS::Braket::SpendingLimit` carries it (in
    // the behavioral corpus `simparity-behavioral-aws-v1`, check
    // `braket__cloudformation.deploy-braket-spendinglimit-simulator-device-rejected`, whose
    // reason reads `Resource handler returned message: "<the service's sentence> (Service:
    // Braket; Operation: CreateSpendingLimit; Status Code: 400; Error Code:
    // ValidationException; Request ID: …)" (RequestToken: …, HandlerErrorCode:
    // InvalidRequest)`).
    ("braket", "Braket", SdkDiagnostic::Operation),
    // Clean Rooms ML's SDK names it `CleanRoomsMl` — that model's own `sdkId` (`CleanRoomsML`,
    // `smithy_query(query="service_info")` on that service's model) written with a lower-case
    // final letter, which is the client name its SDK carries. The shape is the ATTEMPT-counting
    // one, as the recorded `ResourceStatusReason` of an `AWS::CleanRoomsML::TrainingDataset`
    // whose Glue table's Amazon S3 location holds nothing carries it (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, check
    // `cleanroomsml__cloudformation.deploy-cfn-cleanroomsml-training-dataset-empty-s3-location-rejected`,
    // whose reason reads `<the service's sentence> (Service: CleanRoomsMl, Status Code: 400,
    // Request ID: …) (SDK Attempt Count: …)` with `HandlerErrorCode: InvalidRequest`).
    ("cleanrooms-ml", "CleanRoomsMl", SdkDiagnostic::Attempts),
    // AWS CodePipeline's SDK names it by that model's own `sdkId` (`CodePipeline`,
    // `smithy_query(query="service_info")` on that service's model), which is also the
    // `cloudFormationName` the type names of its resources carry. The shape is the
    // ATTEMPT-counting one, as the recorded `ResourceStatusReason` of an
    // `AWS::CodePipeline::Pipeline` whose declared service role that service cannot assume
    // carries it (in the behavioral corpus `simparity-behavioral-aws-v1`, check
    // `cloudformation__codepipeline.deploy-codepipeline-pipeline-unassumable-role-rolled-back`,
    // whose reason reads `Resource handler returned message: "<the service's sentence>
    // (Service: CodePipeline, Status Code: 400, Request ID: …) (SDK Attempt Count: …)"
    // (RequestToken: …, HandlerErrorCode: GeneralServiceException)`). Every type this
    // emulator provisions over this signing name is that service's
    // (`crate::state::provisioner`'s pipeline, custom-action-type and webhook arms).
    ("codepipeline", "CodePipeline", SdkDiagnostic::Attempts),
    // Amazon Data Lifecycle Manager's SDK names it `Dlm` — that model's own `sdkId` (`DLM`) with
    // only the initial capitalised, exactly as Amazon RDS's and Amazon EC2's below are — and
    // words the ATTEMPT-counting shape, as the recorded `ResourceStatusReason` of an
    // `AWS::DLM::LifecyclePolicy` whose declared execution role names a principal in another
    // account carries it (in the behavioral corpus `simparity-behavioral-aws-v1`, check
    // `cloudformation__dlm.deploy-cfn-dlm-lifecyclepolicy-cross-account-role-rejected`, whose
    // reason reads `Resource handler returned message: "<the service's sentence> (Service: Dlm,
    // Status Code: 403, Request ID: …) (SDK Attempt Count: …)" (RequestToken: …,
    // HandlerErrorCode: AccessDenied)`). The one resource type this emulator provisions over
    // this signing name is that service's ([`provisioner::dlm_lifecycle_policies`]).
    ("dlm", "Dlm", SdkDiagnostic::Attempts),
    // Amazon EC2's SDK names it by that model's own `sdkId` (`EC2`,
    // `smithy_query(query="service_info")` on that service's model) written as one word with a
    // capital initial, exactly as Amazon RDS's below is, and words the diagnostic with the
    // refused call's error CODE and the PROXY it was made through — as the recorded
    // `ResourceStatusReason` of an `AWS::EC2::LaunchTemplate` whose declared name Amazon EC2
    // already holds a template under carries it (in the behavioral corpus, check
    // `cloudformation__ec2.deploy-ec2-launch-template-duplicate-name-rejected`, whose reason
    // reads `Resource handler returned message: "<the service's sentence> (Service: Ec2;
    // Status Code: 400; Error Code: InvalidLaunchTemplateName.AlreadyExistsException; Request
    // ID: …; Proxy: null)" (RequestToken: …, HandlerErrorCode: AlreadyExists)`). Every type
    // this emulator provisions over this signing name is that service's.
    ("ec2", "Ec2", SdkDiagnostic::Proxied),
    // Elastic Load Balancing v2's SDK names it `ElasticLoadBalancingV2` — that model's own
    // `awsService.cloudFormationName`, which is also the name its SDK client carries
    // (`smithy_query(query="service_info")`: `sdkId: "Elastic Load Balancing v2"`,
    // `cloudFormationName: ElasticLoadBalancingV2`). The four resource types this emulator
    // provisions over this signing name are all that service's
    // ([`provisioning::elastic_load_balancing`]), and the shape is the bare ENVELOPE, with no
    // attempt count after it, as the recorded `ResourceStatusReason` of an
    // `AWS::ElasticLoadBalancingV2::ListenerRule` whose priority the listener already
    // evaluates a rule at carries it (`"<the service's sentence> (Service:
    // ElasticLoadBalancingV2, Status Code: 400, Request ID: …)"`, where Amazon Route 53's
    // below carries `(SDK Attempt Count: …)` past the closing parenthesis).
    (
        "elasticloadbalancing",
        "ElasticLoadBalancingV2",
        SdkDiagnostic::Envelope,
    ),
    // Amazon EMR's SDK names it `Emr` — that model's own `sdkId` (`EMR`,
    // `smithy_query(query="service_info")` on that service's model) with only the initial
    // capitalised, exactly as Amazon RDS's and Amazon EC2's below and above are — and words the
    // ATTEMPT-counting shape, as the recorded `ResourceStatusReason` of an
    // `AWS::EMR::SecurityConfiguration` whose declared document that service will not store
    // carries it (in the behavioral corpus `simparity-behavioral-aws-v1`, check
    // `cloudformation__emr.deploy-emr-security-configuration-malformed-json-rejected`, whose
    // reason reads `Resource handler returned message: "<the service's sentence> (Service: Emr,
    // Status Code: 400, Request ID: …) (SDK Attempt Count: …)" (RequestToken: …,
    // HandlerErrorCode: InvalidRequest)`). Every type this emulator provisions over this
    // signing name is that service's (the cluster, the security configuration, the Studio and a
    // Studio's session mapping).
    ("elasticmapreduce", "Emr", SdkDiagnostic::Attempts),
    ("es", "OpenSearch", SdkDiagnostic::Attempts),
    ("iam", "Iam", SdkDiagnostic::Attempts),
    // Amazon MSK's SDK names it `Kafka` — that model's own `sdkId` (`Kafka`,
    // `smithy_query(query="service_info")` on that service's model), which is the signing name
    // with its initial capitalised rather than the `MSK` its resource types are spelled with.
    // The shape is the ATTEMPT-counting one, as the recorded `ResourceStatusReason` of an
    // `AWS::MSK::ServerlessCluster` declared without SASL/IAM authentication and of an
    // `AWS::MSK::Configuration` naming an unsupported Apache Kafka version each carry it (in
    // the behavioral corpus `simparity-behavioral-aws-v1`, checks
    // `cloudformation__kafka.deploy-cfn-msk-serverless-cluster-rejects-iam-auth-disabled` and
    // `cloudformation__kafka.deploy-msk-configuration-invalid-kafka-version-rejected-and-\
    // rolled-back`, whose reasons read `Resource handler returned message: "[ClientRequestToken:
    // …] <the service's sentence> (Service: Kafka, Status Code: 400, Request ID: …) (SDK Attempt
    // Count: …)…" (RequestToken: …, HandlerErrorCode: InvalidRequest)`). Every type this
    // emulator provisions over this signing name is that service's (the two cluster types and
    // the configuration).
    ("kafka", "Kafka", SdkDiagnostic::Attempts),
    ("lambda", "Lambda", SdkDiagnostic::Attempts),
    // Amazon CloudWatch Logs's SDK names it by that model's own `sdkId` (`CloudWatch Logs`,
    // `smithy_query(query="service_info")` on that service's model) written as one word,
    // exactly as Application Auto Scaling's above is. Its diagnostic is worded for a refusal
    // of the REQUEST and for no other class ([`SdkDiagnostic::EnvelopeOnRefusedRequest`]),
    // which is what the recorded `ResourceStatusReason` of each class of failure of its types
    // reports: an `AWS::Logs::MetricFilter` whose declared pattern that service cannot read,
    // and an `AWS::Logs::SubscriptionFilter` whose declared Lambda destination may not be
    // invoked, each carry `"… (Service: CloudWatchLogs, Status Code: 400, Request ID: …)"`
    // (in the behavioral corpus `simparity-behavioral-aws-v1`, checks
    // `cloudformation__lambda__logs.cfn-logs-subscription-filter-missing-permission-rejected-at-create`
    // among them), where a log group whose name is already taken and a filter whose declared
    // group is not there carry that service's own sentence alone. The four resource types
    // this emulator provisions over this signing name are all that service's
    // ([`provisioning::logs_resources`]).
    (
        "logs",
        "CloudWatchLogs",
        SdkDiagnostic::EnvelopeOnRefusedRequest,
    ),
    // AWS Elemental MediaConvert's SDK names it by that model's own `sdkId`
    // (`MediaConvert`, `smithy_query(query="service_info")` on that service's model), which
    // is also the `cloudFormationName` the type names of its resources carry, and words its
    // diagnostic with the refused OPERATION and that service's error code in it, as the
    // recorded `ResourceStatusReason` of an `AWS::MediaConvert::CertificateAssociation` whose
    // declared certificate Certificate Manager never issued carries it (in the behavioral
    // corpus `simparity-behavioral-aws-v1`, check
    // `acm__cloudformation__mediaconvert.deploy-mediaconvert-certificateassociation-unknown-certificate-rejected`,
    // whose reason reads `Resource handler returned message: "<the service's sentence>
    // (Service: MediaConvert; Operation: AssociateCertificate; Status Code: 404; Error Code:
    // NotFoundException; Request ID: …)" (RequestToken: …, HandlerErrorCode: NotFound)`). Its
    // three TRANSCODING types are provisioned by a provider that words another shape, which is
    // recorded per provider ([`SDK_PROVIDER_SHAPES`]).
    ("mediaconvert", "MediaConvert", SdkDiagnostic::Operation),
    // Neptune Analytics' SDK names it `NeptuneGraph` — that model's own `sdkId`
    // (`Neptune Graph`) written as one word, exactly as Application Auto Scaling's above is —
    // and words the ATTEMPT-counting shape, as the recorded `ResourceStatusReason` of an
    // `AWS::NeptuneGraph::Graph` whose declared provisioned memory is below that service's
    // minimum carries it (in the behavioral corpus `simparity-behavioral-aws-v1`, check
    // `cloudformation__ec2__neptune-graph.deploy-neptune-graph-provisioned-memory-out-of-range-rejected`,
    // whose reason reads `Resource handler returned message: "<the service's sentence>
    // (Service: NeptuneGraph, Status Code: 400, Request ID: …) (SDK Attempt Count: …)"
    // (RequestToken: …, HandlerErrorCode: InvalidRequest)`).
    ("neptune-graph", "NeptuneGraph", SdkDiagnostic::Attempts),
    // Amazon RDS's SDK names it by that model's own `sdkId` (`RDS`) written as one word with
    // a capital initial, and words the bare ENVELOPE — the service, the status and the call's
    // id, with nothing after it — as the recorded `ResourceStatusReason` of an
    // `AWS::RDS::DBSubnetGroup` whose subnets cover too few Availability Zones carries it (in
    // the behavioral corpus, check
    // `cloudformation__ec2__rds.cfn-rds-db-subnet-group-single-az-rejected-negative`, whose
    // reason reads `Resource handler returned message: "<the service's sentence> (Service:
    // Rds, Status Code: 400, Request ID: …)" (RequestToken: …, HandlerErrorCode:
    // InvalidRequest)`).
    ("rds", "Rds", SdkDiagnostic::Envelope),
    // Recycle Bin's SDK names it `Rbin` — that model's own `sdkId` (`rbin`) with the initial
    // capitalised, exactly as Amazon RDS's above is — and words the ATTEMPT-counting shape, as
    // the recorded `ResourceStatusReason` of an `AWS::Rbin::Rule` whose retention period is
    // longer than the resource type it retains supports carries it (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, check
    // `cloudformation__rbin.deploy-rbin-rule-retention-period-over-one-year-rejected`, whose
    // reason reads `Resource handler returned message: "<the service's sentence> (Service:
    // Rbin, Status Code: 400, Request ID: …) (SDK Attempt Count: …)" (RequestToken: …,
    // HandlerErrorCode: InvalidRequest)`).
    ("rbin", "Rbin", SdkDiagnostic::Attempts),
    // Amazon Route 53's SDK names it by its `sdkId` without the space, exactly as
    // Application Auto Scaling's does (`smithy_query(query="service_info")`: `sdkId:
    // "Route 53"`). The shape is the ATTEMPT-counting one, as the recorded
    // `ResourceStatusReason` of an `AWS::Route53::RecordSet` whose declared zone Route 53
    // does not hold carries it (`(Service: Route53, Status Code: 404, Request ID: …) (SDK
    // Attempt Count: …)`).
    ("route53", "Route53", SdkDiagnostic::Attempts),
    ("s3", "S3", SdkDiagnostic::Attempts),
    // AWS Secrets Manager's SDK names it `SecretsManager` — that model's own `sdkId` (`Secrets
    // Manager`, `smithy_query(query="service_info")` on that service's model) written as one
    // word, which is also the `cloudFormationName` the type names of its resources carry. The
    // shape is the bare ENVELOPE, with no attempt count after it, as the recorded
    // `ResourceStatusReason` of an `AWS::SecretsManager::ResourcePolicy` whose declared document
    // grants public access while the declaration asks for those to be blocked carries it (in the
    // behavioral corpus `simparity-behavioral-aws-v1`, check `secretsmanager.deploy-cfn-public-\
    // resource-policy-rejected-by-block-public-policy-and-rolled-back`, whose reason reads
    // `Resource handler returned message: "<the service's sentence> (Service: SecretsManager,
    // Status Code: 400, Request ID: …)" (RequestToken: …, HandlerErrorCode: InvalidRequest)`).
    // The three resource types this emulator provisions over this signing name are all that
    // service's ([`provisioning::secretsmanager_secret`],
    // [`provisioning::secretsmanager_resource_policy`] and
    // [`provisioning::secretsmanager_rotation_schedule`]).
    (
        SECRETSMANAGER_SIGNING_NAME,
        "SecretsManager",
        SdkDiagnostic::Envelope,
    ),
    // AWS Cloud Map's SDK names it by that model's own `sdkId` (`ServiceDiscovery`,
    // `smithy_query(query="service_info")` on that service's model), which is also the
    // `cloudFormationName` the type names of its resources carry. The shape is the
    // ATTEMPT-counting one, as Amazon Route 53's above is — the two services' resource handlers
    // are the same generation — and the five resource types this emulator provisions over this
    // signing name are all that service's ([`provisioning::service_discovery`]).
    (
        "servicediscovery",
        "ServiceDiscovery",
        SdkDiagnostic::Attempts,
    ),
    // AWS Step Functions' SDK names it `Sfn` — that model's own `sdkId` (`SFN`,
    // `smithy_query(query="service_info")` on that service's model) written as its SDK client
    // is, which is NOT the signing name the request was signed with (`states`) nor the service
    // shape's own name. The shape is the ATTEMPT-counting one, as the recorded
    // `ResourceStatusReason` of an `AWS::StepFunctions::StateMachine` whose declared Amazon
    // States Language definition that service refuses carries it (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, checks
    // `cloudformation__stepfunctions.deploy-cdk-choice-default-unknown-state-rejected` and
    // `cloudformation__stepfunctions.deploy-pass-state-with-next-and-end-together-rejected-\
    // at-stack-create`, whose reasons read `Resource handler returned message: "<the service's
    // sentence> (Service: Sfn, Status Code: 400, Request ID: …) (SDK Attempt Count: …)"
    // (RequestToken: …, HandlerErrorCode: InvalidRequest)`), which is the shape that service's
    // refusals carry wherever they are relayed. The three resource types this emulator
    // provisions over this signing name are all that service's
    // ([`provisioning::step_functions_state_machine`] and
    // [`provisioning::step_functions_versions_and_aliases`]).
    ("states", "Sfn", SdkDiagnostic::Attempts),
    // AWS X-Ray's SDK names it by that model's own `sdkId` (`XRay`,
    // `smithy_query(query="service_info")` on that service's model), which is also the
    // `cloudFormationName` the type names of its resources carry. The shape is the bare
    // ENVELOPE, with no attempt count after it, as the recorded `ResourceStatusReason` of an
    // `AWS::XRay::Group` whose declared filter expression that service refuses carries it (in
    // the behavioral corpus `simparity-behavioral-aws-v1`, check
    // `xray.deploy-group-via-cloudformation-invalid-filter-expression-rejected`, whose reason
    // reads `Resource handler returned message: "<the service's sentence> (Service: XRay,
    // Status Code: 400, Request ID: …)" (RequestToken: …, HandlerErrorCode: InvalidRequest)`).
    // The two resource types this emulator provisions over this signing name are both that
    // service's ([`provisioner::xray_groups`] and the sampling-rule arms of
    // [`provisioner::platform_services`]).
    ("xray", "XRay", SdkDiagnostic::Envelope),
];

/// The SDK service NAME and diagnostic shape for a call on a signing name SEVERAL services
/// share, keyed by the OPERATION the provider's refused call named.
///
/// [`SDK_SERVICE_NAMES`] above is keyed by SIGNING NAME, which identifies the service for
/// every signing name one service owns. The Bedrock family's does not: Amazon Bedrock's own
/// control plane, the Bedrock Agent build-time plane and the Bedrock Agent RUNTIME plane all
/// sign `bedrock` while each serves its own endpoint and carries its own `sdkId`, so which
/// name the SDK of the refused call states is decided by the OPERATION that was called rather
/// than by the signing name. A row here answers for that call and nothing else, so no other
/// claimant of the shared name gains a diagnostic from it.
///
/// Read only where the caller NAMED the operation
/// ([`in_process_calls::call_rest_json_named`]), which is what says which of the services
/// sharing the name was called.
const SDK_SHARED_NAME_SERVICES: &[(&str, &str, &str, SdkDiagnostic)] = &[
    // The Bedrock Agent RUNTIME plane, named by that model's own `sdkId` VERBATIM, the spaces
    // included (`Bedrock Agent Runtime`, `smithy_query(query="service_info")` on that
    // service's model), wording its diagnostic with the refused OPERATION and that service's
    // error code in it — read from the recorded `ResourceStatusReason` of an
    // `AWS::Bedrock::Session` whose declared encryption key names no key (in the behavioral
    // corpus `simparity-behavioral-aws-v1`, check `bedrock-agent-runtime__cloudformation.\
    // deploy-bedrock-agent-runtime-session-nonexistent-kms-key-rejected-and-rolled-back`,
    // whose handler reason reads `<the service's sentence> (Service: Bedrock Agent Runtime;
    // Operation: CreateSession; Status Code: 403; Error Code: AccessDeniedException; Request
    // ID: …)`). The rows beside the create are the same provider's other calls on a session
    // ([`provisioner::bedrock_sessions`]), which word the shape it words — a recording of one
    // of them is what would correct them.
    (
        "bedrock",
        "CreateSession",
        BEDROCK_AGENT_RUNTIME_SDK_NAME,
        SdkDiagnostic::Operation,
    ),
    (
        "bedrock",
        "GetSession",
        BEDROCK_AGENT_RUNTIME_SDK_NAME,
        SdkDiagnostic::Operation,
    ),
    (
        "bedrock",
        "UpdateSession",
        BEDROCK_AGENT_RUNTIME_SDK_NAME,
        SdkDiagnostic::Operation,
    ),
    (
        "bedrock",
        "EndSession",
        BEDROCK_AGENT_RUNTIME_SDK_NAME,
        SdkDiagnostic::Operation,
    ),
    (
        "bedrock",
        "DeleteSession",
        BEDROCK_AGENT_RUNTIME_SDK_NAME,
        SdkDiagnostic::Operation,
    ),
];

/// The name the SDK of the Bedrock Agent runtime states for it, written once so the rows
/// above cannot spell it differently.
const BEDROCK_AGENT_RUNTIME_SDK_NAME: &str = "Bedrock Agent Runtime";

/// The service name and shape a call of `operation` on a SHARED signing name takes
/// ([`SDK_SHARED_NAME_SERVICES`]).
fn shared_name_service(
    signing_name: &str,
    operation: Option<&str>,
) -> Option<(&'static str, SdkDiagnostic)> {
    let operation = operation?;
    SDK_SHARED_NAME_SERVICES
        .iter()
        .find(|(name, called, _, _)| *name == signing_name && *called == operation)
        .map(|(_, _, service, shape)| (*service, *shape))
}

/// The SDK service NAME and diagnostic shape for a call on a signing name SEVERAL services
/// share whose SURFACES are bound under prefixes of their own, keyed by the prefix the
/// refused call's URI is below.
///
/// [`SDK_SHARED_NAME_SERVICES`] tells the claimants of a shared name apart by the OPERATION
/// that was called, which answers only where their operation names differ. The two API
/// Gateway control planes are not such a pair: the REST plane and the v2 plane each publish a
/// `CreateStage`, a `CreateDeployment`, a `CreateAuthorizer` and a `CreateModel`, so the name
/// of the call says nothing about which of them served it. The URI does: every operation of
/// the v2 model is bound below that model's own prefix
/// ([`crate::state::provisioner::APIGATEWAYV2_URI_PREFIX`]) and no operation of the REST
/// plane is, so a refused call made under it was that service's and the exception its SDK
/// raised names that service.
///
/// A row here is read BEFORE the signing-name entry, since it describes one particular
/// surface of a shared name where that entry describes the service the name resolves to
/// otherwise.
const SDK_SHARED_NAME_PREFIXES: &[(&str, &str, &str, SdkDiagnostic)] = &[
    // Amazon API Gateway v2's SDK names it `ApiGatewayV2` — that model's own `sdkId`
    // (`smithy_query(query="service_info")` on that service's model), which is also the name
    // the `AWS::ApiGatewayV2::*` type names carry — and words the ATTEMPT-counting shape,
    // where the REST plane's entry words the bare envelope: read from the recorded
    // `ResourceStatusReason` of an `AWS::ApiGatewayV2::Integration` whose `SQS-SendMessage`
    // subtype mapping leaves out a parameter that action requires (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, check `apigatewayv2__cloudformation__iam__sqs.deploy-\
    // cfn-apigatewayv2-integration-sqssendmessage-missing-messagebody-rejected`, whose reason
    // reads `Resource handler returned message: "<the service's sentence> (Service:
    // ApiGatewayV2, Status Code: 400, Request ID: …) (SDK Attempt Count: …)" (RequestToken: …,
    // HandlerErrorCode: InvalidRequest)`).
    (
        crate::state::provisioner::API_GATEWAY_SIGNING_NAME,
        crate::state::provisioner::APIGATEWAYV2_URI_PREFIX,
        "ApiGatewayV2",
        SdkDiagnostic::Attempts,
    ),
];

/// The service name and shape a call made on `uri` over a SHARED signing name takes
/// ([`SDK_SHARED_NAME_PREFIXES`]), or `None` for a call on no such surface and for a caller
/// that states no URI — an RPC wire binds every operation of a service to one path, so the
/// path of such a call tells its claimants apart from nothing.
fn shared_name_service_at(
    signing_name: &str,
    uri: Option<&str>,
) -> Option<(&'static str, SdkDiagnostic)> {
    let uri = uri?;
    SDK_SHARED_NAME_PREFIXES
        .iter()
        .find(|(name, prefix, _, _)| *name == signing_name && uri_is_under(uri, prefix))
        .map(|(_, _, service, shape)| (*service, *shape))
}

/// Whether `uri` is the path `prefix` names or one BELOW it.
///
/// Matched at a segment boundary, so a surface bound under `/v2` claims `/v2/apis` and
/// `/v2/apis?x=1` and not a sharing service's path that merely opens with those characters: a
/// URI prefix names a subtree of paths, not a string.
fn uri_is_under(uri: &str, prefix: &str) -> bool {
    uri.strip_prefix(prefix).is_some_and(|rest| {
        rest.is_empty() || rest.starts_with('/') || rest.starts_with('?')
    })
}

/// The PROVIDERS whose diagnostic shape differs from the one their service's entry above
/// carries, keyed by the OPERATION the provider's refused call named.
///
/// A resource provider is a per-TYPE artifact, built against whichever generation of its
/// service's SDK it was published with, so two providers of ONE service can word two shapes —
/// and the shape belongs to the provider, not to the service. Where a service's providers
/// disagree, its entry above carries one of them and the operations the other provider calls
/// are recorded here, each citing the recorded reason the shape was read from.
///
/// Read only where the caller NAMED the operation ([`in_process_calls::call_rest_json_named`]),
/// since that is what says which provider made the call; a call that names none takes its
/// service's own shape.
///
/// A row is per OPERATION rather than per recorded call, because the shape is the PROVIDER's:
/// one recorded reason fixes the shape for every call the provider that answered it makes, and
/// the rows below say which reason that was, so a later recording of another of those calls can
/// correct them.
const SDK_PROVIDER_SHAPES: &[(&str, &str, SdkDiagnostic)] = &[
    // The `AWS::ApiGateway::RestApi` type's provider — the api ITSELF
    // ([`provisioning::apigateway_rest_api`]) — words the ATTEMPT-counting shape, where that
    // service's entry above carries the bare envelope its DEPLOYMENT provider words. Read from
    // the recorded `ResourceStatusReason` of an `AWS::ApiGateway::RestApi` whose declared
    // `ApiKeySourceType` is outside that service's value set (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, check `apigateway__cloudformation.deploy-cfn-apigateway-\
    // restapi-invalid-apikeysourcetype-rejected`, whose reason reads `Resource handler returned
    // message: "<the service's sentence> (Service: ApiGateway, Status Code: 400, Request ID: …)
    // (SDK Attempt Count: …)" (RequestToken: …, HandlerErrorCode: GeneralServiceException)`).
    // The two rows are the calls that provider makes whose operation this crate states; a call
    // of its that names none takes its service's own shape.
    (
        crate::state::provisioner::API_GATEWAY_SIGNING_NAME,
        "CreateRestApi",
        SdkDiagnostic::Attempts,
    ),
    (
        crate::state::provisioner::API_GATEWAY_SIGNING_NAME,
        "UpdateRestApi",
        SdkDiagnostic::Attempts,
    ),
    // AWS Elemental MediaConvert's three TRANSCODING types — the queue a stack's jobs run in,
    // the job template they are created from and the preset an output is written with
    // ([`provisioning::mediaconvert_resources`]) — are provisioned by a provider that words the
    // ATTEMPT-counting shape, where that service's entry above carries the semicolon-separated
    // one its certificate association's provider words. Read from the recorded
    // `ResourceStatusReason` of an `AWS::MediaConvert::Queue` declared `RESERVED` — a type that
    // publishes no reservation-plan property, so the create reaches that service without the
    // settings a reserved queue requires (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, check
    // `cloudformation__mediaconvert.deploy-mediaconvert-queue-reserved-pricing-plan-rejected`,
    // whose reason reads `Resource handler returned message: "<the service's sentence>
    // (Service: MediaConvert, Status Code: 400, Request ID: …) (SDK Attempt Count: …)"
    // (RequestToken: …, HandlerErrorCode: InvalidRequest)`). That is the one reason RECORDED for
    // these providers; the other eight rows are the same provider's other calls, which word the
    // shape it words — a recording of one of them is what would correct them.
    ("mediaconvert", "CreateQueue", SdkDiagnostic::Attempts),
    ("mediaconvert", "UpdateQueue", SdkDiagnostic::Attempts),
    ("mediaconvert", "DeleteQueue", SdkDiagnostic::Attempts),
    ("mediaconvert", "CreateJobTemplate", SdkDiagnostic::Attempts),
    ("mediaconvert", "UpdateJobTemplate", SdkDiagnostic::Attempts),
    ("mediaconvert", "DeleteJobTemplate", SdkDiagnostic::Attempts),
    ("mediaconvert", "CreatePreset", SdkDiagnostic::Attempts),
    ("mediaconvert", "UpdatePreset", SdkDiagnostic::Attempts),
    ("mediaconvert", "DeletePreset", SdkDiagnostic::Attempts),
    // The AWS Client VPN types of Amazon EC2 — the endpoint and an ingress authorization rule
    // of one ([`crate::state::provisioner::client_vpn`]) — are provisioned by a provider that
    // words the ATTEMPT-counting shape, where that service's entry above carries the
    // semicolon-separated one its launch template's provider words. Read from the recorded
    // `ResourceStatusReason` of an `AWS::EC2::ClientVpnEndpoint` whose declared client address
    // range is smaller than the published minimum (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, check
    // `acm__cloudformation__ec2.deploy-ec2-client-vpn-endpoint-invalid-client-cidr-rejected`,
    // whose reason reads `Resource handler returned message: "<the service's sentence>
    // (Service: Ec2, Status Code: 400, Request ID: …)" …`). That is the one reason RECORDED
    // for these providers; the other rows are the same providers' other calls, which word the
    // shape they word — a recording of one of them is what would correct them.
    ("ec2", "CreateClientVpnEndpoint", SdkDiagnostic::Attempts),
    ("ec2", "DeleteClientVpnEndpoint", SdkDiagnostic::Attempts),
    (
        "ec2",
        "DescribeClientVpnEndpoints",
        SdkDiagnostic::Attempts,
    ),
    ("ec2", "AuthorizeClientVpnIngress", SdkDiagnostic::Attempts),
    ("ec2", "RevokeClientVpnIngress", SdkDiagnostic::Attempts),
    (
        "ec2",
        "DescribeClientVpnAuthorizationRules",
        SdkDiagnostic::Attempts,
    ),
    // The calls that act on a listener's CERTIFICATE LIST are CloudFormation's own, not a
    // registry provider's: an `AWS::ElasticLoadBalancingV2::ListenerCertificate` is deployed by
    // a handler CloudFormation ships ([`provisioning::elbv2_listener_certificate`]), built
    // against a generation of that service's SDK that words the PROXIED shape — where the four
    // types that ARE resources of that service have providers wording its entry's bare envelope.
    // Read from the recorded `ResourceStatusReason` of a `ListenerCertificate` pointed at a
    // well-formed certificate ARN that service resolves to nothing (in the behavioral corpus
    // `simparity-behavioral-aws-v1`, check `acm__cloudformation__ec2__elbv2.cfn-\
    // listenercertificate-nonexistent-certificate-rejected`, whose reason reads `<the service's
    // sentence> (Service: AmazonElasticLoadBalancing; Status Code: 400; Error Code:
    // CertificateNotFound; Request ID: …; Proxy: null)`, with no provider envelope quoted around
    // it at all).
    (
        "elasticloadbalancing",
        "AddListenerCertificates",
        SdkDiagnostic::Proxied,
    ),
    (
        "elasticloadbalancing",
        "RemoveListenerCertificates",
        SdkDiagnostic::Proxied,
    ),
    (
        "elasticloadbalancing",
        "DescribeListenerCertificates",
        SdkDiagnostic::Proxied,
    ),
];

/// The SDK NAME a provider reports the service it called under, where that differs from the name
/// its service's own entry carries ([`SDK_SERVICE_NAMES`]), keyed by the OPERATION the refused
/// call named exactly as [`SDK_PROVIDER_SHAPES`] is.
///
/// The name in the diagnostic is the name of the SDK CLIENT the caller used, so it travels with
/// the same thing the shape does: a handler built against another generation of a service's SDK
/// names that generation's client, where a registry provider of the same service names the one
/// it was generated with.
const SDK_PROVIDER_SERVICE_NAMES: &[(&str, &str, &str)] = &[
    // The certificate-list calls name the client of the generation that words the proxied shape
    // beside them, whose client for Elastic Load Balancing v2 is `AmazonElasticLoadBalancing` —
    // read from the same recorded reason those rows cite.
    (
        "elasticloadbalancing",
        "AddListenerCertificates",
        LEGACY_ELB_SDK_NAME,
    ),
    (
        "elasticloadbalancing",
        "RemoveListenerCertificates",
        LEGACY_ELB_SDK_NAME,
    ),
    (
        "elasticloadbalancing",
        "DescribeListenerCertificates",
        LEGACY_ELB_SDK_NAME,
    ),
];

/// See [`SDK_PROVIDER_SERVICE_NAMES`].
const LEGACY_ELB_SDK_NAME: &str = "AmazonElasticLoadBalancing";

/// The SDK name the provider that called `operation` of `signing_name` reports that service
/// under, where that differs from its service's own ([`SDK_PROVIDER_SERVICE_NAMES`]).
fn provider_service_name(signing_name: &str, operation: Option<&str>) -> Option<&'static str> {
    let operation = operation?;
    SDK_PROVIDER_SERVICE_NAMES
        .iter()
        .find(|(name, called, _)| *name == signing_name && *called == operation)
        .map(|(_, _, service)| *service)
}

/// The shape the provider that called `operation` of `signing_name` words, where that differs
/// from its service's own ([`SDK_PROVIDER_SHAPES`]).
fn provider_shape(signing_name: &str, operation: Option<&str>) -> Option<SdkDiagnostic> {
    let operation = operation?;
    SDK_PROVIDER_SHAPES
        .iter()
        .find(|(name, called, _)| *name == signing_name && *called == operation)
        .map(|(_, _, shape)| *shape)
}

/// The SHAPE of the parenthesis an SDK appends to the message of the exception it raises for
/// a refused call.
///
/// Each names the service and the HTTP status; what differs is what else they carry and how
/// the parts are separated. Which of them a called service's SDK words is that SDK's, and it
/// is not derivable from the service, so it is RECORDED per service in
/// [`SDK_SERVICE_NAMES`] — each entry citing where that shape was read from, with
/// [`SDK_PROVIDER_SHAPES`] recording the providers of a service that word another one — and a
/// service with no entry has no diagnostic invented for it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SdkDiagnostic {
    /// `(Service: <service>, Status Code: <status>, Request ID: <id>) (SDK Attempt Count:
    /// <n>)` — comma-separated, with the call's id (or ids) and how many attempts the SDK
    /// made at it.
    Attempts,
    /// `(Service: <service>; Operation: <operation>; Status Code: <status>; Error Code:
    /// <code>; Request ID: <id>)` — semicolon-separated, naming the operation that was
    /// refused and the error code it was refused with in place of the attempt count.
    Operation,
    /// `(Service: <service>, Status Code: <status>, Request ID: <id>)` — the same
    /// comma-separated envelope [`SdkDiagnostic::Attempts`] opens with, and NOTHING after it:
    /// the SDK of such a service words no attempt count at all.
    Envelope,
    /// [`SdkDiagnostic::Envelope`] for a refusal of the REQUEST, and NOTHING AT ALL for one of
    /// any other class ([`handler_error_code`]): the providers of such a service's types pass
    /// the SDK's own exception on for the invalid-request class — where the diagnostic is part
    /// of the message that exception carries — and report every other class as the owning
    /// service's own sentence alone.
    EnvelopeOnRefusedRequest,
    /// `(Service: <service>; Status Code: <status>; Error Code: <code>; Request ID: <id>;
    /// Proxy: null)` — semicolon-separated like [`SdkDiagnostic::Operation`], naming the error
    /// code the call was refused with but not the call, and closing with the PROXY the SDK made
    /// the request through: `null` for the direct call this emulator's in-process dispatch is.
    Proxied,
}

/// The services whose answers carry an EXTENDED request id beside the request id, which
/// their SDK states in the diagnostic too.
///
/// Amazon S3 is the service that answers one: every S3 response carries `x-amz-id-2` beside
/// `x-amz-request-id`, and the exception an S3 SDK raises states both ("Request ID: ...,
/// Extended Request ID: ..."). A service with no entry answers one id, and its diagnostic
/// states that one.
pub(super) const SDK_EXTENDED_REQUEST_ID_SERVICES: &[&str] = &["s3"];

/// How many characters the request id of a service that answers an extended one carries, and
/// the case it is written in: Amazon S3's `x-amz-request-id` is a short opaque token in
/// upper case, where the request id of the services above is a uuid
/// ([`crate::state::new_request_id`]).
pub(super) const SDK_SHORT_REQUEST_ID_CHARACTERS: usize = 16;

/// How many characters the EXTENDED request id carries: a long opaque token, which is what
/// `x-amz-id-2` is.
pub(super) const SDK_EXTENDED_REQUEST_ID_CHARACTERS: usize = 64;

/// The request id — and the extended request id where the service answers one — that the
/// diagnostic states for a call to `signing_name`.
///
/// Both are this call's own, minted per call as every identifier this service hands out is:
/// the in-process answer carries none of its own, and they identify the CALL rather than
/// anything about the resource.
pub(super) fn sdk_request_ids(signing_name: &str) -> String {
    if !SDK_EXTENDED_REQUEST_ID_SERVICES.contains(&signing_name) {
        return format!("Request ID: {}", crate::state::new_request_id());
    }
    format!(
        "Request ID: {}, Extended Request ID: {}",
        crate::state::new_identifier_digits("sdk-request", SDK_SHORT_REQUEST_ID_CHARACTERS)
            .to_uppercase(),
        crate::state::new_identifier_digits(
            "sdk-extended-request",
            SDK_EXTENDED_REQUEST_ID_CHARACTERS
        ),
    )
}

/// The diagnostic an AWS SDK appends to the message of the exception it raises for a refused
/// call: which service refused it, the HTTP status it answered with, the id (or ids) of the
/// call and how many attempts the SDK made at it.
///
/// `None` for a service [`SDK_SERVICE_NAMES`] does not name, which leaves the message as
/// that service worded it — for a diagnostic that NAMES the operation when the caller
/// named none, since an operation is not something to invent, and for a service whose
/// providers report one only on a refusal of the REQUEST
/// ([`SdkDiagnostic::EnvelopeOnRefusedRequest`]) when the refusal falls in another class.
///
/// `operation` is the called operation as its own model names it, which only a caller that
/// knows which operation it is making can state ([`call_rest_json_named`]); `uri` is the path
/// the call was made on, which is what says which of the services sharing one signing name
/// served it ([`SDK_SHARED_NAME_PREFIXES`]).
pub(super) fn sdk_diagnostic(
    signing_name: &str,
    operation: Option<&str>,
    uri: Option<&str>,
    status: u16,
    code: &str,
) -> Option<String> {
    // The SURFACE of a shared signing name the call was made on, which names its own service
    // whatever the name resolves to otherwise.
    let surface = shared_name_service_at(signing_name, uri);
    let (service, shape) = surface
        .or_else(|| {
            SDK_SERVICE_NAMES
                .iter()
                .find(|(name, _, _)| *name == signing_name)
                .map(|(_, service, shape)| (*service, *shape))
        })
        // A signing name several services share is identified by the OPERATION that was
        // called, not by the name ([`SDK_SHARED_NAME_SERVICES`]).
        .or_else(|| shared_name_service(signing_name, operation))?;

    // The shape the PROVIDER that made this call words, where that service's providers do not
    // all word the same one ([`SDK_PROVIDER_SHAPES`]). Keyed by the operation's name, which
    // the claimants of a shared signing name can share — so it is not asked for a call a URI
    // prefix already resolved to one surface, whose own row states what that surface's
    // provider words.
    let shape = match surface {
        Some(_) => shape,
        None => provider_shape(signing_name, operation).unwrap_or(shape),
    };
    // And the name that provider's own SDK client reports the service under, where that differs
    // from its service's ([`SDK_PROVIDER_SERVICE_NAMES`]) — keyed the same way, and so not asked
    // for a call a URI prefix already resolved to one surface, for the same reason.
    let service = match surface {
        Some(_) => service,
        None => provider_service_name(signing_name, operation).unwrap_or(service),
    };
    // The envelope the two comma-separated shapes open with, written once so the two cannot
    // state the service, the status and the call's id differently.
    let envelope = || {
        format!(
            "(Service: {service}, Status Code: {status}, {})",
            sdk_request_ids(signing_name),
        )
    };
    match shape {
        SdkDiagnostic::Attempts => Some(format!(
            "{} (SDK Attempt Count: {})",
            envelope(),
            sdk_attempt_count(code),
        )),
        SdkDiagnostic::Operation => operation.map(|operation| {
            format!(
                "(Service: {service}; Operation: {operation}; Status Code: {status}; \
                 Error Code: {code}; {})",
                sdk_request_ids(signing_name),
            )
        }),
        SdkDiagnostic::Envelope => Some(envelope()),
        SdkDiagnostic::EnvelopeOnRefusedRequest => {
            (handler_error_code(code) == HANDLER_ERROR_INVALID_REQUEST).then(envelope)
        }
        SdkDiagnostic::Proxied => Some(format!(
            "(Service: {service}; Status Code: {status}; Error Code: {code}; {}; \
             Proxy: {SDK_DIRECT_CALL_PROXY})",
            sdk_request_ids(signing_name),
        )),
    }
}

/// The PROXY an SDK that words one states for a call it made DIRECTLY, with no proxy
/// configured — which is every call this emulator's in-process dispatch makes
/// ([`in_process_calls::dispatch`]).
pub(super) const SDK_DIRECT_CALL_PROXY: &str = "null";

/// How many attempts an AWS SDK makes at a call whose refusal it classifies as worth
/// retrying: its default maximum, reached because every attempt was refused the same way.
pub(super) const SDK_RETRIED_ATTEMPTS: u32 = 3;

/// How many it makes at a refusal it does not retry: the one call it was asked for.
pub(super) const SDK_SINGLE_ATTEMPT: u32 = 1;

/// The classes of refusal an SDK retries, as [`HANDLER_ERROR_CLASSES`] names them: a
/// throttled call, an exhausted service limit and a service that failed are conditions that
/// can clear on their own, where a refusal of the REQUEST cannot and is raised on the first
/// attempt.
pub(super) const SDK_RETRIED_CLASSES: [&str; 3] =
    ["Throttling", "ServiceLimitExceeded", "GeneralServiceException"];

/// The attempt count the diagnostic reports for a refusal carrying `code`, from the class
/// that code falls in ([`handler_error_code`]) — never from the message, which a request can
/// influence.
pub(super) fn sdk_attempt_count(code: &str) -> u32 {
    if SDK_RETRIED_CLASSES.contains(&handler_error_code(code)) {
        SDK_RETRIED_ATTEMPTS
    } else {
        SDK_SINGLE_ATTEMPT
    }
}

/// One refused answer's message as the SDK of the called service raises it: the message that
/// service worded, then that SDK's diagnostic of the call where this crate words one
/// ([`sdk_diagnostic`]).
pub(crate) fn sdk_message(signing_name: &str, status: u16, code: &str, message: &str) -> String {
    sdk_message_of(signing_name, None, None, status, code, message)
}

/// The member an error DOCUMENT attributes its refusal to, as the wire spells that member:
/// the value a service that reports which input parameter was at fault carries beside its
/// message (Amazon MSK's `BadRequestException$InvalidParameter`, `jsonName:
/// invalidParameter`).
const ATTRIBUTED_PARAMETER_MEMBER: &str = "invalidParameter";

/// The CALLS whose provider states the attributed member after the SDK's own diagnostic,
/// keyed by the OPERATION the refused call named.
///
/// A service that reports WHICH input parameter it refused reports it as a member of its error
/// shape, and most providers report the message alone. One that states it quotes it at the END
/// of what it reports, past the SDK's diagnostic — and whether a provider does is the
/// PROVIDER's, not the service's: Amazon MSK's serverless-cluster provider states it where
/// that service's configuration provider does not, which is why this is per operation, exactly
/// as [`SDK_PROVIDER_SHAPES`] is.
///
/// Read from the recorded `ResourceStatusReason` of an `AWS::MSK::ServerlessCluster` refused
/// for its client authentication (in the behavioral corpus `simparity-behavioral-aws-v1`,
/// check `cloudformation__kafka.deploy-cfn-msk-serverless-cluster-rejects-iam-auth-disabled`,
/// whose reason closes the quoted message `… (SDK Attempt Count: …) 'clientAuthentication'`),
/// against the `AWS::MSK::Configuration` reason beside it, which closes at the diagnostic
/// although that service attributes its refusal to `kafkaVersions` just the same.
const SDK_ATTRIBUTED_PARAMETER_CALLS: &[(&str, &str)] = &[("kafka", "CreateClusterV2")];

/// What such a provider adds to the message for a refusal `document` attributes to an input
/// member: that member, quoted, or `None` for a call whose provider words none and for an
/// answer that attributed its refusal to nothing.
pub(super) fn attributed_parameter(
    signing_name: &str,
    operation: Option<&str>,
    document: &Value,
) -> Option<String> {
    let operation = operation?;
    if !SDK_ATTRIBUTED_PARAMETER_CALLS
        .iter()
        .any(|(name, called)| *name == signing_name && *called == operation)
    {
        return None;
    }
    let parameter = document
        .get(ATTRIBUTED_PARAMETER_MEMBER)
        .and_then(Value::as_str)
        .filter(|parameter| !parameter.is_empty())?;
    Some(format!("'{parameter}'"))
}

/// [`sdk_message`] for a caller that knows WHICH operation of that service it called, which
/// is part of the diagnostic some SDKs word ([`SdkDiagnostic::Operation`]), and on WHICH URI,
/// which is what says which of the services sharing one signing name answered it
/// ([`SDK_SHARED_NAME_PREFIXES`]).
pub(crate) fn sdk_message_of(
    signing_name: &str,
    operation: Option<&str>,
    uri: Option<&str>,
    status: u16,
    code: &str,
    message: &str,
) -> String {
    match sdk_diagnostic(signing_name, operation, uri, status, code) {
        Some(diagnostic) if message.is_empty() => diagnostic,
        Some(diagnostic) => format!("{message} {diagnostic}"),
        None => message.to_string(),
    }
}

/// Each `HandlerErrorCode` a provider's refusal is reported under, with the fragments an
/// owning service's own error CODE carries when the refusal falls in that class.
///
/// The classes are the model's own `HandlerErrorCode` values
/// ([`crate::ops::types::HANDLER_ERROR_CODE_VALUES`], which the tests below hold every
/// class here to), and that is what a client reads to decide whether a failed resource is
/// worth retrying — a throttled create is, a malformed declaration never is. The class is
/// decided by the error CODE the owning service answered, the value a client of that
/// service branches on, and never by message text a request can influence.
///
/// Walked in order, so a code carrying more than one fragment takes the first: the narrower
/// class comes before the wider one it reads like (`RequestLimitExceeded` is throttling, not
/// an exhausted quota).
pub(super) const HANDLER_ERROR_CLASSES: &[(&str, &[&str])] = &[
    (
        "AccessDenied",
        &["AccessDenied", "Forbidden", "Unauthorized", "AuthorizationError"],
    ),
    (
        "InvalidCredentials",
        &["InvalidClientTokenId", "SignatureDoesNotMatch", "ExpiredToken"],
    ),
    (
        "Throttling",
        &["Throttl", "TooManyRequests", "RequestLimitExceeded", "SlowDown"],
    ),
    (
        "ServiceLimitExceeded",
        &["LimitExceeded", "QuotaExceeded", "TooManyEntries"],
    ),
    (
        HANDLER_ERROR_ALREADY_EXISTS,
        &["AlreadyExists", "AlreadyOwnedBy", "Duplicate"],
    ),
    ("NotFound", &["NotFound", "NoSuch", "DoesNotExist"]),
    (
        "ResourceConflict",
        &["Conflict", "InUse", "InvalidState", "Concurrent"],
    ),
    (
        "InternalFailure",
        &["InternalFailure", "SerializationException"],
    ),
    (
        "GeneralServiceException",
        &[
            "InternalError",
            "InternalServerError",
            "ServiceUnavailable",
            "ServiceFailure",
            // A call the owning service would not carry out because the resource it manages
            // still HOLDS content (Amazon S3's `BucketNotEmpty` for a bucket that still has
            // objects in it): the declaration is well formed and the provider asked for
            // exactly what the stack said, so what it passes on is the called service's own
            // exception rather than a fault it found in the request — and, like every other
            // class here, a condition that clears on its own once that content is gone,
            // which is why the SDK retried the call ([`SDK_RETRIED_CLASSES`]).
            "NotEmpty",
        ],
    ),
];

/// The class a refusal falls in when nothing above matches: the owning service refused the
/// request the declaration asked for, which is what every validation refusal is.
///
/// Read by [`crate::state::provisioner::Provisioner::handler_message`] too: it is the class whose
/// own wording some providers carry inside the message.
pub(crate) const HANDLER_ERROR_INVALID_REQUEST: &str = "InvalidRequest";

/// The class a refusal that the resource's NAME is already in use falls in — the one class
/// CloudFormation's own record of the account and Region can say more about than the service
/// that answered it ([`already_managed_reason`]).
pub(super) const HANDLER_ERROR_ALREADY_EXISTS: &str = "AlreadyExists";

/// The lowest HTTP status that is the owning SERVICE failing rather than the request being
/// wrong, for an answer that carried no error code at all ([`UNNAMED_CODE_PREFIX`]).
pub(super) const SERVER_ERROR_STATUS: u16 = 500;

/// The class an answer that named no error at all falls in when its status says the owning
/// service failed rather than that the request was wrong.
///
/// Read by [`crate::state::provisioner::Provisioner::handler_error_class`] too: it is the
/// class a provider that maps none of the owning service's own error codes reports a refusal
/// under, whatever the code itself reads like.
pub(crate) const HANDLER_ERROR_SERVICE_FAILED: &str = "GeneralServiceException";

/// The `HandlerErrorCode` an owning service's refusal is reported under — see
/// [`HANDLER_ERROR_CLASSES`].
///
/// Read by the per-type arms too, for a provider whose own wording of a refusal is the wording
/// of ONE class's exception: the class decides whether that wording applies, so the arm asks
/// the same question of the same code this does
/// ([`crate::state::provisioner::call_firewall_association`]).
pub(in crate::state) fn handler_error_code(code: &str) -> &'static str {
    if let Some(status) = code.strip_prefix(UNNAMED_CODE_PREFIX) {
        // An answer that named no error at all: only the status it came back with says
        // whether the service failed or the request was refused.
        return match status.parse::<u16>() {
            Ok(status) if status >= SERVER_ERROR_STATUS => HANDLER_ERROR_SERVICE_FAILED,
            _ => HANDLER_ERROR_INVALID_REQUEST,
        };
    }
    HANDLER_ERROR_CLASSES
        .iter()
        .find(|(_, fragments)| fragments.iter().any(|fragment| code.contains(fragment)))
        .map_or(HANDLER_ERROR_INVALID_REQUEST, |(class, _)| *class)
}

/// The `ResourceStatusReason` the event for a resource its provider refused to act on
/// carries: "Success/failure message associated with the resource".
///
/// CloudFormation does not report its own words for such a failure — it reports the
/// provider's, quoted, beside the token of the resource request it answered and the
/// `HandlerErrorCode` class that refusal falls in ([`handler_error_code`]), so a client
/// reads WHY the resource failed and whether the failure is of a kind worth retrying.
///
/// The token is this resource request's own, minted per request as every identifier this
/// service hands out is.
///
/// `owner` is the provisioner of the type whose resource failed, because the message inside
/// the quotes is the type's HANDLER's
/// ([`crate::state::provisioner::Provisioner::handler_message`]): for a type whose provider words a
/// refusal through the class it put it in, that wording is part of what CloudFormation
/// quotes. A type this emulator has no provisioner for reports the refusal as it came.
///
/// `identifier` is the resource the refusal is about, as its owning service knows it: a
/// provider that names the resource in its own report of a refusal — a bucket whose name is
/// already taken ([`s3_bucket::handler_message`]) — states it from here, so the event says
/// WHICH resource the refusal was about.
///
/// A type CloudFormation provisions ITSELF has no provider answer to quote, and its failure is
/// therefore reported in CloudFormation's own words with no envelope around them
/// ([`own_failure_reason`]).
pub(in crate::state) fn handler_failure_reason(owner: Option<Owner>, refusal: &Refusal, identifier: &str) -> String {
    // A type CloudFormation provisions itself: no provider answered for the resource, so there
    // is nothing to quote and the refusal stands as it reads.
    if owner.is_some_and(|owner| !owner.reported_by_a_resource_provider()) {
        return own_failure_reason(refusal);
    }
    let message = stated_message(refusal);
    let class = match owner {
        Some(owner) => owner.handler_error_class(&refusal.code, handler_error_code(&refusal.code)),
        None => handler_error_code(&refusal.code),
    };
    // The token of the resource request this refusal answered, minted here because this is
    // where that request ends: it is reported once as the `RequestToken` of the event and
    // again inside the message of a provider that words it there
    // ([`crate::state::provisioner::Provisioner::handler_message`]), and the two are the same
    // request, so they are the same token.
    let token = crate::state::new_request_id();
    let message = match owner {
        // The bucket provider's own wording, which lives beside this algorithm for the
        // reason [`s3_bucket`] records.
        Some(Owner::Builtin(Provisioner::S3Bucket)) => {
            s3_bucket::handler_message(class, identifier, message)
        }
        Some(provisioner) => provisioner.handler_message(class, message, &token),
        None => message.to_string(),
    };
    quoted_provider_message(&message, &token, class)
}

/// The `ResourceStatusReason` the event for a resource whose own PROVIDER reported that it is
/// not going to reach the state the deployment waited for carries — a load balancer the owning
/// service settled in a state it serves no traffic in, a link that service settled `FAILED`.
///
/// It is the same answer [`handler_failure_reason`] reports: a provider's, which CloudFormation
/// does not restate in its own words but QUOTES, beside the token of the resource request it
/// answered and the `HandlerErrorCode` class it falls in. What differs is only where the
/// provider's sentence came from — a read of the resource rather than a refused call — so the
/// envelope is built from the same parts, and a client classifying the failure reads one shape
/// however the provider arrived at it.
///
/// The class is [`HANDLER_ERROR_SERVICE_FAILED`]: nothing about the request was wrong — the
/// owning service ACCEPTED the call and then could not bring the resource up — which is what
/// that class says of a provider whose own service failed it.
///
/// A type CloudFormation provisions ITSELF, and one backed by a provider of the client's own
/// (a custom resource, whose answer carries no provider CloudFormation stands behind), has no
/// resource provider to quote, so its report stands as it reads ([`own_failure_reason`]).
///
/// That is where this parts company with [`handler_failure_reason`], which is asked only where
/// a provider call was made and therefore quotes an owner it does not recognise as well: an
/// answer reported for a type with NO owner at all reaches this one — a custom resource's own
/// FAILED response travels as a readiness answer — and such a report is nobody's provider's, so
/// it is left unquoted rather than attributed to one.
pub(in crate::state) fn handler_reported_failure_reason(
    owner: Option<Owner>,
    reported: &str,
) -> String {
    if !owner.is_some_and(Owner::reported_by_a_resource_provider) {
        return reported.to_string();
    }
    // Minted here for the reason [`handler_failure_reason`] records: this is where the resource
    // request ends, and the token reported is that request's own.
    let token = crate::state::new_request_id();
    quoted_provider_message(reported, &token, HANDLER_ERROR_SERVICE_FAILED)
}

/// One provider's answer in the envelope CloudFormation reports a resource provider's answer
/// in: the message quoted, the token of the resource request it answered, and the class that
/// answer falls in.
///
/// Stated once, because the two reports that carry it — a refused call
/// ([`handler_failure_reason`]) and a resource that will not come up
/// ([`handler_reported_failure_reason`]) — are the same envelope around a different sentence,
/// and a client parses the one shape.
fn quoted_provider_message(message: &str, token: &str, class: &str) -> String {
    format!(
        "Resource handler returned message: \"{message}\" (RequestToken: {token}, \
         HandlerErrorCode: {class})",
    )
}

/// What a refusal STATES: the message it worded, or — for a service that worded none at all
/// but still named the error — the code it named, which is then all there is to report.
fn stated_message(refusal: &Refusal) -> &str {
    if refusal.message.is_empty() {
        refusal.code.as_str()
    } else {
        refusal.message.as_str()
    }
}

/// The `ResourceStatusReason` the event for a failed resource of a type CLOUDFORMATION
/// PROVISIONS ITSELF carries: the refusal exactly as it stands.
///
/// The envelope [`handler_failure_reason`] builds is CloudFormation QUOTING a resource
/// provider — the registry extension of the service that owns the type, whose answer
/// CloudFormation reports beside the token of the resource request it answered and the class
/// that answer falls in. A type CloudFormation deploys itself has no such provider: a nested
/// stack is created by CloudFormation's own `CreateStack` ([`nested_stack`]), so what fails
/// that resource is CloudFormation's own account of why the stack was not created and is
/// reported as it reads — with no quoting, no request token and no handler class, none of
/// which any provider supplied.
///
/// Only [`handler_failure_reason`] decides this, from the type whose resource failed
/// ([`Owner::reported_by_a_resource_provider`]), so the question is asked once wherever a
/// refusal is recorded.
fn own_failure_reason(refusal: &Refusal) -> String {
    stated_message(refusal).to_string()
}

/// CLOUDFORMATION's own reason for a create the owning service refused because the name is
/// already in use AND a live stack of this Region is what manages the resource holding it, or
/// `None` where the refusal is not that.
///
/// The service that owns a type arbitrates its own namespace, and a name already in use is
/// what it answers the `AlreadyExists` class for ([`handler_error_code`]) — but CloudFormation
/// knows something that service cannot: WHICH STACK manages the resource that holds the name.
/// Where the holder is one of this Region's stacks, the resource's event therefore carries
/// CloudFormation's own sentence naming that stack
/// ([`crate::state::physical_name::already_exists_in_stack`]) instead of the owning service's report
/// about its own namespace, which is the actionable half of the failure. A name no stack of
/// the Region manages is nothing CloudFormation can add to — a name another account holds, or
/// one a client took outside any stack — so that service's own message stands.
///
/// Asked at the refusal rather than ahead of the create for the reason
/// [`crate::state::physical_name`] records: a create the owning service ACCEPTS under a name one of
/// this Region's stacks holds is a deployment that got the resource it declared, and nothing
/// of CloudFormation's turns it into a failure.
pub(super) fn already_managed_reason(
    resource: &StoredResource,
    stack: &StackProvenance,
    refusal: &Refusal,
) -> Option<String> {
    if handler_error_code(&refusal.code) != HANDLER_ERROR_ALREADY_EXISTS {
        return None;
    }
    crate::state::physical_name::taken_by_another_stack(
        resource,
        &stack.managed_names,
        &stack.stack_id,
    )
        .map(|held| {
            crate::state::physical_name::already_exists_in_stack(
                &held.physical_resource_id,
                &held.stack_id,
            )
        })
}

/// The refusal the stack's SERVICE ROLE answers a call of `operation` on `signing_name`
/// with, or `None` where nothing refuses it — so a call the role's policies deny is never
/// made ([`crate::state::service_role`]).
///
/// The refusal is CLOUDFORMATION's, not the owning service's: the call was not made, so
/// there is no answer of that service's to report. Its code is the one an unauthorized call
/// is classified under ([`handler_error_code`]'s `AccessDenied` class, which is what tells a
/// client whether the failed resource is worth retrying) rather than the particular spelling
/// the called service would have used, and its message is the sentence AWS words such a
/// refusal with, naming the principal, the action and the resource. `operation` is `None` for
/// a wire this cannot read an operation name off, which is judged by nothing.
pub(super) fn denied_by_the_service_role(signing_name: &str, operation: Option<&str>) -> Option<Refusal> {
    let message = crate::state::service_role::refusal(signing_name, operation?)?;
    Some(Refusal {
        code: crate::state::service_role::ACCESS_DENIED.to_string(),
        message,
    })
}
