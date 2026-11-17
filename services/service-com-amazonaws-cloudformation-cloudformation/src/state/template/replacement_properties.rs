// SPDX-License-Identifier: CC0-1.0

use super::*;

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
        IAM_ACCESS_KEY => &["Serial", "UserName"],
        // A MemoryDB parameter group's engine FAMILY and its description are fixed when
        // Amazon MemoryDB makes it: both are members of `CreateParameterGroup`, and the one
        // call that changes a group afterwards (`UpdateParameterGroup`) takes nothing but
        // the group's parameter values — so a declaration restating either asks for a
        // different group, which CloudFormation replaces. (A subnet group needs no entry:
        // both of its properties beside the name are `UpdateSubnetGroup`'s own members.)
        MEMORYDB_PARAMETER_GROUP => &["Description", "Family"],
        // A spending limit is a cost control ABOUT one device: `UpdateSpendingLimit` takes
        // the limit's amount and its period and no device at all, so a declaration naming
        // another device asks for a different limit.
        BRAKET_SPENDING_LIMIT => &["DeviceArn"],
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
        _ => composite_identity_properties(resource_type),
    }
}
