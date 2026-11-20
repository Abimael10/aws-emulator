// SPDX-License-Identifier: CC0-1.0

use super::*;

/// Whether a REPLACEMENT of this type must delete the resource it replaces BEFORE creating the
/// one that replaces it.
///
/// It is for a type whose owning service cannot hold two of the resource at once: "each subnet
/// in your VPC must be associated with a route table" and with exactly one, so Amazon EC2
/// refuses a second association for a subnet that already has one — on AWS the create-then-
/// delete order is not merely unusual there, it is impossible. CloudFormation therefore deletes
/// such a resource first, whatever its `UpdateReplacePolicy` states: there is no window in
/// which both exist, so the old physical resource cannot be kept while the new one is created.
///
/// Every other type's replacement is create-before-delete, which is the order that keeps the
/// resource it replaces serving its clients until the new one is in force — and the order the
/// deployment's cleanup phase takes the old one down in
/// ([`crate::state::provisioning::reconcile`]).
pub fn replacement_deletes_first(resource_type: &str) -> bool {
    resource_type == EC2_SUBNET_ROUTE_TABLE_ASSOCIATION
        // An Application Signals instrumentation configuration is the same case: one code
        // location holds at most one configuration — a create at a location that already has
        // one is refused as a conflict ("If a configuration already exists for the same
        // service, environment, signal type, and location, this operation returns a conflict
        // instead of overwriting it") — so a replacement that keeps the location cannot create
        // the new record while the old one is there.
        || resource_type == APPLICATION_SIGNALS_INSTRUMENTATION_CONFIG
}

/// The properties of this type a resource cannot take a NEW VALUE for while it goes on
/// existing — the ones the resource and property types reference publishes with "Update
/// requires: `Replacement`" — BESIDE the name, which is in that position for every type
/// whose owning service cannot rename the resource ([`name_is_create_only`]) and is read
/// from the declaration rather than catalogued.
///
/// A resource is created with such a property and nothing changes it afterwards: an IAM
/// access key belongs to the user `CreateAccessKey` minted it for and carries the `Serial`
/// the template rotates it with ("incrementing this value notifies CloudFormation that you
/// want to rotate your access key… CloudFormation will replace the AccessKey resource with
/// a new access key"), so a declaration that states either differently asks for a DIFFERENT
/// resource — which CloudFormation replaces rather than updates.
///
/// A type with no entry has none beyond its name, so the name alone decides whether a
/// re-deploy means the same resource ([`super::declared_name`]).
///
/// The parts of a COMPOSITE primary identifier are in that position by construction
/// ([`composite_identity_properties`]): the resource IS the binding those values name, so
/// stating one of them differently asks for a different resource.
pub fn replacement_properties(resource_type: &str) -> &'static [&'static str] {
    match resource_type {
        // An Application Insights application IS the monitoring of one resource group: that
        // group is what the application is keyed by, every call on one takes it, and
        // `UpdateApplication` carries no member for it — so a declaration naming another group
        // asks for a DIFFERENT application, which CloudFormation replaces. (The type declares no
        // name of its own, so [`name_is_create_only`] has nothing to catch here.)
        APPLICATION_INSIGHTS_APPLICATION => &["ResourceGroupName"],
        IAM_ACCESS_KEY => &["Serial", "UserName"],
        // A published state machine VERSION keeps everything it was published with: it is the
        // snapshot `PublishStateMachineVersion` took of the machine, and Step Functions
        // publishes no call that edits one — so a declaration restating any property asks for a
        // DIFFERENT version, which CloudFormation replaces by publishing one
        // ([`crate::state::provisioning::step_functions_versions_and_aliases`]). (An ALIAS
        // needs no entry: its description and its routing configuration are
        // `UpdateStateMachineAlias`' own members, so a change to either reaches the alias in
        // place, and its NAME is in this position through [`name_is_create_only`].)
        crate::state::provisioning::step_functions_versions_and_aliases::STEP_FUNCTIONS_STATE_MACHINE_VERSION => {
            &["Description", "StateMachineArn", "StateMachineRevisionId"]
        }
        // An Amazon Athena SAVED QUERY keeps everything it was saved with: `CreateNamedQuery`
        // takes the database, the SQL and the workgroup, and the one call that changes a query
        // afterwards (`UpdateNamedQuery`) takes the query's id with a new name, description and
        // query string and NO database or workgroup — Athena publishes no call that moves a
        // saved query to another database or workgroup. So a declaration restating any of them
        // asks for a DIFFERENT query, which CloudFormation replaces; the resource and property
        // types reference publishes every property of this type with "Update requires:
        // Replacement", and a replacement is how this emulator applies a change to one
        // ([`crate::state::provisioning::athena_resources`]). (Its NAME is in that position
        // through [`name_is_create_only`].)
        crate::state::provisioning::athena_resources::ATHENA_NAMED_QUERY => {
            &["Database", "Description", "QueryString", "WorkGroup"]
        }
        // A MemoryDB parameter group's engine FAMILY and its description are fixed when
        // Amazon MemoryDB makes it: both are members of `CreateParameterGroup`, and the one
        // call that changes a group afterwards (`UpdateParameterGroup`) takes nothing but
        // the group's parameter values — so a declaration restating either asks for a
        // different group, which CloudFormation replaces. (A subnet group needs no entry:
        // both of its properties beside the name are `UpdateSubnetGroup`'s own members.)
        MEMORYDB_PARAMETER_GROUP => &["Description", "Family"],
        // A MediaConvert queue keeps the PRICING PLAN it was created with: `pricingPlan` is a
        // member of `CreateQueue` and of no member of `UpdateQueue` — which takes a queue's
        // description, status, concurrency and reservation plan and no pricing plan at all
        // (`smithy_query(query="operation",operation="CreateQueue,UpdateQueue")`) — so a
        // declaration restating it asks for a different queue, which CloudFormation replaces.
        // (Its NAME is in that position through [`name_is_create_only`], as a job template's
        // and a preset's are; every other property of the three is a member of that type's own
        // update.)
        MEDIACONVERT_QUEUE => &["PricingPlan"],
        // A session is encrypted under the key it was created with: `encryptionKeyArn` is a
        // member of `CreateSession` and of no member of `UpdateSession` (which takes a
        // session's identifier and its metadata alone), so a declaration naming another key
        // asks for a different session.
        BEDROCK_SESSION => &["EncryptionKeyArn"],
        // A spending limit is a cost control ABOUT one device: `UpdateSpendingLimit` takes
        // the limit's amount and its period and no device at all, so a declaration naming
        // another device asks for a different limit.
        BRAKET_SPENDING_LIMIT => &["DeviceArn"],
        // An Application Signals instrumentation configuration keeps everything it was created
        // with: "configurations are immutable after creation"
        // (`CreateInstrumentationConfiguration`), and that service publishes no call that
        // changes one at all — so a declaration restating any property asks for a DIFFERENT
        // configuration, which CloudFormation replaces. Its own four identity dimensions (the
        // service, the environment, the signal type and the location) are among them for the
        // same reason every other property is: the record the create wrote is the whole of the
        // resource. (A GROUPING CONFIGURATION needs no entry: `PutGroupingConfiguration` is
        // documented "Creates or updates the grouping configuration for this account", so a
        // changed declaration reaches the configuration in place. An SLO needs none either:
        // every property of it beside its name is a member of `UpdateServiceLevelObjective`.)
        APPLICATION_SIGNALS_INSTRUMENTATION_CONFIG => &[
            "AttributeFilters",
            "CaptureConfiguration",
            "Description",
            "Environment",
            "ExpiresAt",
            "InstrumentationType",
            "Location",
            "Service",
            "SignalType",
            "Tags",
        ],
        // Clean Rooms ML publishes no call that changes a training dataset at all: the
        // dataset's description, the role it reads its data with and the datasets themselves
        // are all fixed when `CreateTrainingDataset` records it, so a declaration restating one
        // of them asks for a different dataset. (Its NAME is in the same position through
        // [`name_is_create_only`].) Its `Tags` are here for the same reason rather than as an
        // exception: that service does change a dataset's tags in place, through its own
        // `TagResource`/`UntagResource`, but no arm of [`crate::state::provisioner`] calls
        // them for this type yet — so a changed tag set is applied by replacing the dataset,
        // whose create carries the tags, rather than reported as applied by nothing. A tag arm
        // retires this entry.
        CLEANROOMSML_TRAINING_DATASET => &["Description", "RoleArn", "Tags", "TrainingData"],
        // A hypervisor keeps the encryption key it was imported with: `KmsKeyArn` is a member
        // of `ImportHypervisorConfiguration` and of no member of `UpdateHypervisor` (which
        // takes the host, the credentials, the name and the log group), so a declaration
        // restating it asks for a different hypervisor.
        BACKUP_GATEWAY_HYPERVISOR => &["KmsKeyArn"],
        // A retention rule retains the ONE resource type it was created for: `ResourceType` is
        // a member of `CreateRule`, and `UpdateRule`'s member of that name is documented
        // "currently not supported. You can't update a retention rule's resource type after
        // creation" — so a declaration naming another type asks for a different rule.
        RBIN_RULE => &["ResourceType"],
        // A Neptune Analytics graph keeps the replicas, the encryption key and the vector
        // index it was made with: each is a member of `CreateGraph` and of no member of
        // `UpdateGraph` (which takes the provisioned memory and the two flags), so a
        // declaration restating one asks for a different graph. (Its NAME is in the same
        // position through [`name_is_create_only`]: no call of that service renames a graph.)
        NEPTUNE_GRAPH_GRAPH => &[
            "KmsKeyIdentifier",
            "ReplicaCount",
            "VectorSearchConfiguration",
        ],
        // A snapshot is the capture of ONE graph: `graphIdentifier` is a member of
        // `CreateGraphSnapshot`, and that service publishes no call that changes a snapshot at
        // all, so a declaration naming another graph asks for a different snapshot.
        NEPTUNE_GRAPH_GRAPH_SNAPSHOT => &["GraphIdentifier"],
        // A Cloud Map SERVICE lives in the namespace its `NamespaceId` names, is of the kind
        // its `Type` states and has the custom health check its create configured: each is a
        // member of `CreateService` and of no member of `ServiceChange` (`UpdateService` takes
        // the description, the DNS records and the Route 53 health check), and that service
        // publishes no call that moves a service to another namespace — so a declaration
        // restating one of them asks for a DIFFERENT service, which CloudFormation replaces.
        crate::state::template::SERVICEDISCOVERY_SERVICE => {
            &["HealthCheckCustomConfig", "NamespaceId", "Type"]
        }
        // An INSTANCE is registered IN one service: `RegisterInstance$ServiceId` addresses the
        // instance rather than being a value of it, and Cloud Map publishes no call that moves
        // an instance to another service, so a declaration naming another service asks for a
        // different instance. (Its `InstanceId` is in the same position through
        // [`name_is_create_only`]: that id is what identifies the instance.)
        crate::state::template::SERVICEDISCOVERY_INSTANCE => {
            &[crate::state::template::SERVICEDISCOVERY_SERVICE_ID_PROPERTY]
        }
        // A PRIVATE DNS namespace's hosted zone is associated with the VPC its create named:
        // `Vpc` is a member of `CreatePrivateDnsNamespace` and of no member of
        // `PrivateDnsNamespaceChange` (that update takes the description and the SOA record), so
        // a declaration naming another VPC asks for a different namespace.
        crate::state::template::SERVICEDISCOVERY_PRIVATE_DNS_NAMESPACE => &["Vpc"],
        // A maintenance window TARGET belongs to the window it was registered with and is of
        // the kind the register stated: `WindowId` addresses the target rather than being a
        // value of it, and `ResourceType` is a member of the register and of no update
        // (`UpdateMaintenanceWindowTarget` takes the targets, the owner information, the name
        // and the description), so a declaration restating either asks for a different target.
        crate::state::provisioner::ssm_maintenance_windows::SSM_MAINTENANCE_WINDOW_TARGET => {
            &["ResourceType", "WindowId"]
        }
        // An Amplify UI Builder component, form or theme belongs to the app environment its
        // `AppId` and `EnvironmentName` name: both are URI labels of every call on one — the
        // create included — and that service publishes no call that moves an entity to another
        // app or environment, so a declaration restating either asks for a different entity.
        AMPLIFYUIBUILDER_COMPONENT | AMPLIFYUIBUILDER_FORM | AMPLIFYUIBUILDER_THEME => {
            &["AppId", "EnvironmentName"]
        }
        // An AWS Amplify BRANCH belongs to the app its `AppId` names: that id is the first URI
        // label of every call on a branch — the create included — and that service publishes no
        // call that moves a branch to another app, so a declaration restating it asks for a
        // different branch. (Its NAME is in the same position through [`name_is_create_only`]:
        // `UpdateBranch` addresses a branch BY its name and takes no new one.)
        AMPLIFY_BRANCH => &["AppId"],
        // An environment's encryption key and who manages its VPC endpoints are fixed when
        // Amazon MWAA makes it: both are members of `CreateEnvironment` and of no member of
        // `UpdateEnvironment`, so a declaration restating either asks for a different
        // environment.
        MWAA_ENVIRONMENT => &["EndpointManagement", "KmsKey"],
        // An api's PROTOCOL is fixed when API Gateway v2 makes it: `ProtocolType` is a member
        // of `CreateApi` and of no member of `UpdateApi` — and no other call of that service
        // changes an api's protocol — so a declaration stating another protocol asks for a
        // DIFFERENT api, which is what that property's "Update requires: Replacement" says.
        // Every other property of the type is one that patch takes, so stating one
        // differently is a change to the api the stack already has
        // (`crate::state::provisioner::update_apigatewayv2_api`).
        APIGATEWAYV2_API => &["ProtocolType"],
        // An integration, a route, a stage, a deployment and an authorizer each exist INSIDE
        // the api their `ApiId` names, and API Gateway v2 publishes no call that moves one to
        // another api — every call on one takes the api beside the resource's own identifier —
        // so a declaration naming another api asks for a different resource, which is what
        // that property's "Update requires: Replacement" says. A DEPLOYMENT is a snapshot
        // released to one stage besides: `CreateDeployment` takes that stage and
        // `UpdateDeployment` takes the description alone
        // (`crate::state::provisioner::ApiGatewayV2Child::create_only`).
        APIGATEWAYV2_INTEGRATION
        | APIGATEWAYV2_ROUTE
        | APIGATEWAYV2_STAGE
        | APIGATEWAYV2_AUTHORIZER => &["ApiId"],
        APIGATEWAYV2_DEPLOYMENT => &["ApiId", "StageName"],
        // An AppSync custom domain is served with the certificate it was created with:
        // `certificateArn` is a member of `CreateDomainName`, `UpdateDomainName` takes the
        // description alone, and no other call of that service re-certifies a domain — so a
        // declaration naming another certificate asks for a different domain, which the
        // deployment replaces. (The MAPPING of an API onto a domain has none beyond its own
        // identity: its `ApiId` is what `AssociateApi` changes in place.)
        crate::state::provisioning::appsync_domain_names::APPSYNC_DOMAIN_NAME => {
            &["CertificateArn"]
        }
        // Every child of an AppSync GraphQL API lives UNDER the API its `ApiId` names: that id
        // is the first URI label of every call on the child — the create included — and that
        // service publishes no call that moves one to another API, so a declaration restating
        // it asks for a DIFFERENT resource. A RESOLVER is attached to one field of one type,
        // which `UpdateResolver` addresses it by and takes no new value for, so restating
        // either asks for a different resolver too. (A data source's NAME is in that same
        // position through [`name_is_create_only`]: `UpdateDataSource` addresses a data source
        // BY its name and takes no new one.)
        APPSYNC_GRAPHQL_SCHEMA
        | APPSYNC_DATA_SOURCE
        | APPSYNC_FUNCTION_CONFIGURATION
        | APPSYNC_API_KEY => &[APPSYNC_API_ID_PROPERTY],
        APPSYNC_RESOLVER => &[
            APPSYNC_API_ID_PROPERTY,
            APPSYNC_TYPE_NAME_PROPERTY,
            APPSYNC_FIELD_NAME_PROPERTY,
        ],
        // A GraphQL API is of the KIND it was created as and is reachable from where its
        // create said: `apiType` and `visibility` are members of `CreateGraphqlApi` and of no
        // member of `UpdateGraphqlApi`, so a declaration restating either asks for a different
        // API. (Its NAME is not one of them: that update takes a new one — see
        // [`name_is_create_only`].)
        APPSYNC_GRAPHQL_API => &["ApiType", "Visibility"],
        // An AppConfig environment and configuration profile belong to the application their
        // `ApplicationId` names, and a profile reads its configuration from the source its
        // `LocationUri` and `Type` fix: each is a member of that type's create and of no
        // member of its update (`UpdateConfigurationProfile` takes neither `LocationUri` nor
        // `Type`), so a declaration restating one asks for a different resource. A deployment
        // strategy's `ReplicateTo` is the same case against `UpdateDeploymentStrategy`.
        APPCONFIG_ENVIRONMENT => &["ApplicationId"],
        APPCONFIG_CONFIGURATION_PROFILE => &["ApplicationId", "LocationUri", "Type"],
        APPCONFIG_DEPLOYMENT_STRATEGY => &["ReplicateTo"],
        // A hosted configuration VERSION is its content under the number AppConfig minted, and a
        // DEPLOYMENT is one rollout of one version into one environment: that service publishes
        // no call that edits either — `CreateHostedConfigurationVersion` and `StartDeployment`
        // are the only calls that make one, and the only other call on an existing deployment
        // stops it — so every property of both types is create-only and a declaration restating
        // any of them asks for a different resource, which the deployment replaces. The parts of
        // each type's composite identity are in that position for the second reason as well.
        APPCONFIG_HOSTED_CONFIGURATION_VERSION => &[
            "ApplicationId",
            "ConfigurationProfileId",
            "Content",
            "ContentType",
            "Description",
            "LatestVersionNumber",
            "VersionLabel",
        ],
        APPCONFIG_DEPLOYMENT => &[
            "ApplicationId",
            "ConfigurationProfileId",
            "ConfigurationVersion",
            "DeploymentStrategyId",
            "Description",
            "DynamicExtensionParameters",
            "EnvironmentId",
            "KmsKeyIdentifier",
        ],
        // A grant on a private CA is made WHOLE and cannot be edited: Amazon Web Services
        // Private CA publishes no call that changes one — `CreatePermission` refuses a
        // request that re-asserts the same CA, principal and account for another action set,
        // and the only way to a different grant is to revoke and grant again — so every
        // property of the type is create-only and a declaration restating any of them asks
        // for a different resource, which the deployment replaces. The two that are also the
        // parts of its composite identity are in that position for the second reason as well.
        ACMPCA_PERMISSION => &["Actions", "CertificateAuthorityArn", "Principal", "SourceAccount"],
        // A State Manager association may be paired with ONE node by the deprecated
        // `InstanceId`, which `CreateAssociation` takes and `UpdateAssociation` does not: the
        // node an association was created for cannot be changed on it, so a declaration
        // naming another node asks for a different association.
        SSM_ASSOCIATION => &["InstanceId"],
        // The operating system a PATCH BASELINE applies to is fixed when Systems Manager makes
        // it: `OperatingSystem` is a member of `CreatePatchBaseline` and of no member of
        // `UpdatePatchBaseline`, so a declaration restating it asks for a different baseline —
        // which CloudFormation replaces rather than updates.
        crate::state::provisioning::ssm_patch_baseline::SSM_PATCH_BASELINE => {
            &["OperatingSystem"]
        }
        // An event bus is created FOR a partner source or for nothing, and EventBridge
        // publishes no call that re-points one: `EventSourceName` is a member of
        // `CreateEventBus` and of no member of `UpdateEventBus`, so a declaration restating it
        // asks for a different bus.
        EVENTS_EVENT_BUS => &["EventSourceName"],
        // An event bus's resource policy statement is installed ON one bus under one
        // statement id: `PutPermission` takes both and EventBridge publishes no call that
        // moves a statement to another bus or renames one, so a declaration restating either
        // asks for a different grant — which CloudFormation replaces rather than updates.
        EVENTS_EVENT_BUS_POLICY => &["EventBusName", "StatementId"],
        // A document's TYPE is fixed when Systems Manager makes it: `DocumentType` is a member
        // of `CreateDocument` and of no member of `UpdateDocument` — no call of that service
        // turns a command document into a runbook — and so is `Requires`, so a declaration
        // restating either asks for a different document. Every other property is a member of
        // that update, so stating one differently is a change to the document the stack
        // already has (`crate::state::provisioner::update_ssm_document`).
        crate::state::provisioner::ssm_documents::SSM_DOCUMENT => {
            &["DocumentType", "Requires"]
        }
        // An Amazon Cognito user pool's sign-in aliases and its username attributes and their
        // case sensitivity are fixed when the pool is made: each is a member of
        // `CreateUserPool` and of no member of `UpdateUserPool`, so a declaration restating
        // one asks for a different pool. (Its `Schema` is NOT one of them: the schema is
        // append-only on the pool that exists, and `AddCustomAttributes` is the call that adds
        // to it — `crate::state::provisioning::cognito_user_pool`.) An app client's secret
        // generation is the same case against `UpdateUserPoolClient`, and an identity
        // provider's type against `UpdateIdentityProvider`.
        //
        // The POOL each of those three belongs to is not among them, even though no update
        // call moves a resource between pools: a declaration naming another pool is reported
        // as a change that was NOT APPLIED, by the arm that would have made the call
        // (`cognito_user_pool`'s own `create_only_properties`), which is the same reading an
        // identity-pool role attachment moved to another pool gets. A resource server's
        // identifier and an identity provider's name are those resources' identities, which
        // `name_is_create_only` already covers.
        COGNITO_USER_POOL => &["AliasAttributes", "UsernameAttributes", "UsernameConfiguration"],
        COGNITO_USER_POOL_CLIENT => &["GenerateSecret"],
        COGNITO_USER_POOL_IDENTITY_PROVIDER => &["ProviderType"],
        // A rotation schedule IS the rotation configured on the secret its `SecretId` names:
        // `RotateSecret` takes that secret's id and there is no call that moves a rotation to
        // another secret, so a declaration naming another secret asks for a different
        // resource — which is what that property's "Update requires: Replacement" says.
        SECRETSMANAGER_ROTATION_SCHEDULE => &["SecretId"],
        // A resource POLICY is the same case: `PutResourcePolicy` takes the secret's id and
        // there is no call that moves a policy to another secret, so a declaration naming
        // another secret asks for a different resource.
        SECRETSMANAGER_RESOURCE_POLICY => &["SecretId"],
        // A log stream, a metric filter and a subscription filter each exist INSIDE the log
        // group their `LogGroupName` names, and Amazon CloudWatch Logs publishes no call that
        // moves one to another group — every call on one takes the group beside the child's
        // own name — so a declaration naming another group asks for a different resource,
        // which is what that property's "Update requires: Replacement" says.
        crate::state::provisioning::logs_resources::LOGS_LOG_STREAM
        | crate::state::provisioning::logs_resources::LOGS_METRIC_FILTER
        | crate::state::provisioning::logs_resources::LOGS_SUBSCRIPTION_FILTER => {
            &["LogGroupName"]
        }
        // An Amazon ECS task-definition REVISION is immutable: that service publishes no call
        // that edits one — `RegisterTaskDefinition` is the only way to state a configuration,
        // and it makes a NEW revision — so every property of the type carries "Update
        // requires: Replacement", and a declaration that states any of them differently asks
        // for a new revision rather than a change to the one the stack has.
        ECS_TASK_DEFINITION => &[
            "ContainerDefinitions",
            "Cpu",
            "EnableFaultInjection",
            "EphemeralStorage",
            "ExecutionRoleArn",
            "Family",
            "InferenceAccelerators",
            "IpcMode",
            "Memory",
            "NetworkMode",
            "PidMode",
            "PlacementConstraints",
            "ProxyConfiguration",
            "RequiresCompatibilities",
            "RuntimePlatform",
            "TaskRoleArn",
            "Volumes",
        ],
        // An AWS Batch job-definition REVISION is immutable in the same way: that service
        // publishes no call that edits one — `RegisterJobDefinition` is the only way to state
        // a configuration, and it makes a NEW revision under the name — so a declaration that
        // states any of these differently asks for a new revision, with its own ARN, rather
        // than a change to the one the stack has.
        BATCH_JOB_DEFINITION => &[
            "ConsumableResourceProperties",
            "ContainerProperties",
            "EcsProperties",
            "EksProperties",
            "NodeProperties",
            "Parameters",
            "PlatformCapabilities",
            "PropagateTags",
            "RetryStrategy",
            "SchedulingPriority",
            "Timeout",
            "Type",
        ],
        // An Amazon RDS DB INSTANCE is created with the database it hosts, the character sets
        // it stores text in, the master user it is reached as, whether its storage is
        // encrypted and under which key, the time zone its engine runs in, the system
        // identifier an Oracle instance carries, the cluster it joined and what it was created
        // FROM: each is a member of `CreateDBInstance` (or of the restore and read-replica
        // creates) and of no member of `ModifyDBInstance` — Amazon RDS publishes no call that
        // changes any of them on the instance that exists, which is why the update walk sends
        // none of them (`DB_INSTANCE_UPDATE_PROPERTIES_NOT_SENT`) — so a declaration restating
        // one asks for a DIFFERENT DB instance, which is what each property's "Update
        // requires: Replacement" says. Every other property of the type is one that
        // modification takes, so stating it differently is a change to the instance the stack
        // already has; `AvailabilityZone` is left out deliberately, since the resource
        // reference publishes it as an update with "some interruptions" rather than as a
        // replacement.
        RDS_DB_INSTANCE_TYPE => &[
            "CharacterSetName",
            "DBClusterIdentifier",
            "DBName",
            "DBSnapshotIdentifier",
            "DBSystemId",
            "KmsKeyId",
            "MasterUsername",
            "NcharCharacterSetName",
            "SourceDBInstanceIdentifier",
            "SourceDbiResourceId",
            "SourceRegion",
            "StorageEncrypted",
            "Timezone",
        ],
        // ---------------------------------------------------------------------------
        // Amazon EC2
        // ---------------------------------------------------------------------------
        //
        // Source for every row below: AWS's CloudFormation resource reference for the type —
        // the properties it publishes with "Update requires: `Replacement`" — read against the
        // calls Amazon EC2 publishes for changing a resource that exists
        // (`crate::state::provisioner::ec2_in_place`,
        // `crate::state::provisioning::ec2_vpc_network`). Each is a value fixed when that
        // service makes the resource: no call of it changes the block a VPC or a subnet is
        // addressed on, the zone a subnet is in, the VPC a subnet, a route table, a security
        // group or an endpoint belongs to, the description or the name a security group was
        // created with, the service an endpoint reaches, or the AMI, the key pair and the
        // placement an instance was launched with. So a declaration restating one asks for a
        // DIFFERENT resource, which CloudFormation replaces; every property NOT named here is
        // one the owning service takes a new value for on the resource that exists, which the
        // deployment applies in place.
        EC2_VPC => &["CidrBlock", "Ipv4IpamPoolId", "Ipv4NetmaskLength"],
        EC2_SUBNET => &[
            "AvailabilityZone",
            "AvailabilityZoneId",
            "CidrBlock",
            "Ipv4IpamPoolId",
            "Ipv4NetmaskLength",
            "Ipv6CidrBlock",
            "Ipv6CidrBlocks",
            "Ipv6IpamPoolId",
            "Ipv6Native",
            "Ipv6NetmaskLength",
            "OutpostArn",
            "VpcId",
        ],
        EC2_SECURITY_GROUP => &["GroupDescription", "GroupName", "VpcId"],
        EC2_ROUTE_TABLE => &["VpcId"],
        EC2_VPN_GATEWAY => &["AmazonSideAsn", "Type"],
        EC2_NAT_GATEWAY => &[
            "AllocationId",
            "ConnectivityType",
            "PrivateIpAddress",
            "SubnetId",
        ],
        EC2_EIP => &[
            "Address",
            "Domain",
            "IpamPoolId",
            "NetworkBorderGroup",
            "PublicIpv4Pool",
            "TransferAddress",
        ],
        EC2_VPC_ENDPOINT => &["ServiceName", "VpcEndpointType", "VpcId"],
        // A CLIENT VPN ENDPOINT's three create-only properties: "the address range cannot be
        // changed after the Client VPN endpoint has been created"
        // (`CreateClientVpnEndpointRequest$ClientCidrBlock`), and neither the authentication
        // options an endpoint checks clients against nor the transport protocol a session
        // speaks is a member of `ModifyClientVpnEndpoint` — the one call that reconfigures an
        // endpoint that exists — so a declaration restating one asks for a different endpoint.
        crate::state::provisioner::client_vpn::EC2_CLIENT_VPN_ENDPOINT => {
            &["AuthenticationOptions", "ClientCidrBlock", "TransportProtocol"]
        }
        // An AUTHORIZATION RULE has no updatable property at all: Amazon EC2 publishes no
        // call that changes a rule that exists — the surface is the authorize that adds one
        // and the revoke that takes it off — so every property of the type is create-only and
        // a declaration restating any of them asks for a different rule.
        crate::state::provisioner::client_vpn::EC2_CLIENT_VPN_AUTHORIZATION_RULE => &[
            "AccessGroupId",
            "AuthorizeAllGroups",
            "ClientVpnEndpointId",
            "Description",
            "TargetNetworkCidr",
        ],
        // An attachment IS the pair of a VPC and a gateway, and its physical id is built from
        // the VPC half ([`vpc_gateway_attachment_identity`]) — so a declaration naming another
        // VPC names a different attachment, while the GATEWAY it puts on that VPC is swapped on
        // the attachment that exists.
        EC2_VPC_GATEWAY_ATTACHMENT => &["VpcId"],
        // A ROUTE is identified by the table it is in and the ONE destination it matches on,
        // which is what its physical id is built from ([`route_identity`]): restating either
        // names a different route, while the TARGET is `ReplaceRoute`'s to change on the route
        // that exists.
        EC2_ROUTE => &[
            "DestinationCidrBlock",
            "DestinationIpv6CidrBlock",
            "DestinationPrefixListId",
            "RouteTableId",
        ],
        // An ASSOCIATION is the binding between one subnet and one route table, and Amazon EC2
        // publishes no call that re-points one — `ReplaceRouteTableAssociation` makes a new
        // association, with a new id — so restating either side asks for a different
        // association.
        EC2_SUBNET_ROUTE_TABLE_ASSOCIATION => &["RouteTableId", "SubnetId"],
        // A security group RULE is the ONE permission it is: everything about it but its
        // DESCRIPTION is fixed when the authorize adds it, the only call that changes a rule
        // that exists being the one that writes its description.
        EC2_SECURITY_GROUP_INGRESS => &[
            "CidrIp",
            "CidrIpv6",
            "FromPort",
            "GroupId",
            "GroupName",
            "IpProtocol",
            "SourcePrefixListId",
            "SourceSecurityGroupId",
            "SourceSecurityGroupName",
            "SourceSecurityGroupOwnerId",
            "ToPort",
        ],
        EC2_SECURITY_GROUP_EGRESS => &[
            "CidrIp",
            "CidrIpv6",
            "DestinationPrefixListId",
            "DestinationSecurityGroupId",
            "FromPort",
            "GroupId",
            "IpProtocol",
            "ToPort",
        ],
        // A launch template is a versioned document: its launch DATA reaches the template that
        // exists as a new version of it, and only the name the template is known by is fixed.
        // (That name is also in this position through [`name_is_create_only`].)
        EC2_LAUNCH_TEMPLATE => &["LaunchTemplateName"],
        // An INSTANCE keeps the machine it was launched as: the image it boots, the subnet and
        // the zone it was placed in, the addresses and interfaces the launch gave it, the key
        // pair it admits and the placement it runs under are each a member of `RunInstances`
        // that no call changes afterwards.
        EC2_INSTANCE => &[
            "AdditionalInfo",
            "Affinity",
            "AvailabilityZone",
            "HostId",
            "HostResourceGroupArn",
            "ImageId",
            "Ipv6AddressCount",
            "Ipv6Addresses",
            "KeyName",
            // The launch template an instance was launched FROM is the machine it runs as: the
            // image, the type, the profile and the interfaces the launch read out of that
            // template version, none of which a call of that service re-reads for an instance
            // that exists. A declaration naming another template, or another version of one,
            // asks for an instance launched again.
            "LaunchTemplate",
            "NetworkInterfaces",
            "PlacementGroupName",
            "PrivateIpAddress",
            "SecurityGroups",
            "SubnetId",
            "Tenancy",
        ],
        // An elastic network INTERFACE is created in Amazon EC2 itself
        // ([`crate::state::provisioning::ec2_vpc_network`]), and the ids a declaration states
        // for the resources it names — its subnet, its security groups — are the ones those
        // services minted. Its own create-only properties are the ones no call of that service
        // changes on the interface that exists: the subnet it is in, the addresses the create
        // gave it and the kind of interface it is.
        EC2_NETWORK_INTERFACE => &[
            "EnablePrimaryIpv6",
            "InterfaceType",
            "Ipv6AddressCount",
            "PrivateIpAddress",
            "SecondaryPrivateIpAddressCount",
            "SubnetId",
        ],
        // An Amazon EBS VOLUME keeps the zone it is in, what it was created FROM and how it is
        // encrypted; its size, its performance and its attachment settings are `ModifyVolume`'s
        // to change.
        EC2_VOLUME => &[
            "AvailabilityZone",
            "Encrypted",
            "KmsKeyId",
            "OutpostArn",
            "SnapshotId",
        ],
        _ => composite_identity_properties(resource_type),
    }
}
