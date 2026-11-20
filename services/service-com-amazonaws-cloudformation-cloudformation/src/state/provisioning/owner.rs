// SPDX-License-Identifier: CC0-1.0

//! The one place each provisioning question is PUT to whichever module owns a resource type
//! ([`super::Owner`]).
//!
//! Every method here is the same question asked of one owner — create it, is it ready, apply
//! a changed declaration, re-tag it, delete it, what does its service report about it — so the
//! deployment algorithm beside it ([`super::reconcile`]) is unchanged by where an arm lives.
//! It is a file of its own so that adding a type's arms does not grow the algorithm's file
//! past the length a change to this repository may touch.

use super::*;

impl Owner {
    /// Create one resource in the service that owns it — see
    /// [`Provisioner::create`](super::provisioner::Provisioner::create).
    ///
    /// The declaration is validated against what its type's schema requires FIRST
    /// ([`model_validation`]): a provider validates the model it is handed before it calls
    /// anybody, so a declaration missing a property that schema marks required fails the
    /// resource with the validator's own sentence and nothing is asked of the owning service.
    pub(super) async fn create(
        self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        resource: &StoredResource,
        propagated: &[(String, String)],
        acknowledged: &[String],
    ) -> Result<Created, Refusal> {
        if let Some(refusal) =
            model_validation::refusal(&resource.resource_type, &resource.properties)
        {
            return Err(refusal);
        }
        match self {
            Self::Builtin(provisioner) => {
                // BOXED, and so is every other call this file makes into the catalogue: that
                // one `match` carries an arm per resource type this emulator creates, so the
                // future it returns is as large as its largest arm's, and holding it inline
                // here would put that whole state inside the future of every deployment —
                // one that a `Box::pin` in a handler builds ON THE STACK before it moves it
                // to the heap. One pointer instead keeps a stack deployment's cost
                // independent of how many types the catalogue has grown to carry, which is
                // the reason its own arms give for boxing themselves
                // ([`Provisioner::create`]).
                Box::pin(provisioner.create(ctx, region, resource, propagated)).await
            }
            Self::DynamodbTable => {
                dynamodb_table::create(ctx, region, resource, propagated).await
            }
            // A global table is the same table plus the replicas of its group, created by
            // the same service's calls ([`dynamodb_table::global_table`]).
            Self::DynamodbGlobalTable => {
                dynamodb_table::global_table::create(ctx, region, resource, propagated).await
            }
            // An inline policy carries no tags of its own: it is not a resource, so
            // CloudFormation propagates none to it.
            Self::IamPolicy(kind) => iam_policy::create(ctx, region, kind, resource)
                .await
                .map(|()| Created::default()),
            // Neither an alias nor a published version takes tags: Lambda's tagging calls
            // address FUNCTIONS, so there is no call that could carry a tag to one.
            Self::LambdaAlias => lambda_alias::create(ctx, region, resource).await,
            Self::LambdaVersion => lambda_version::create(ctx, region, resource).await,
            Self::LambdaEventSourceMapping => {
                lambda_event_source_mapping::create(ctx, region, resource, propagated).await
            }
            // An asynchronous-invocation configuration carries no tags of its own: it is not
            // a resource, but the configuration of a qualifier of the function it names, so
            // CloudFormation propagates none to it.
            Self::LambdaEventInvokeConfig => {
                lambda_event_invoke_config::create(ctx, region, resource).await
            }
            Self::LambdaFunction => {
                lambda_function::create(ctx, region, resource, propagated).await
            }
            // A layer version takes no tags: Lambda's tagging calls address FUNCTIONS, so
            // there is no call that could carry a tag to one.
            Self::LambdaLayerVersion => {
                lambda_layer_version::create(ctx, region, resource).await
            }
            // A policy statement carries no tags of its own: it is not a resource, so
            // CloudFormation propagates none to it.
            Self::LambdaPermission => lambda_permission::create(ctx, region, resource).await,
            Self::LogsResource(kind) => {
                logs_resources::create(ctx, region, kind, resource, propagated).await
            }
            Self::IotAuthorizer => {
                iot_authorizer::create(ctx, region, resource, propagated).await
            }
            // A provisioning template and a role alias are each taggable: `Tags` is the tag
            // list their creates take, so each carries the tags CloudFormation gives the
            // resource beside its own.
            Self::IotProvisioningResource(kind) => {
                iot_provisioning_resources::create(ctx, region, kind, resource, propagated).await
            }
            // A topic rule IS taggable: `TagResource` addresses one by the ARN its create
            // minted, so it carries the tags CloudFormation gives the resource beside its own
            // ([`iot_topic_rule::create`]).
            Self::IotTopicRule => {
                iot_topic_rule::create(ctx, region, resource, propagated).await
            }
            Self::Ec2VpcNetwork(kind) => {
                ec2_vpc_network::create(ctx, region, kind, resource, propagated).await
            }
            Self::Ec2Instance => {
                ec2_instance::create(ctx, region, resource, propagated).await
            }
            // A task-definition revision IS taggable: its `Tags` property is the tag list
            // `RegisterTaskDefinition` takes, so the revision carries the tags CloudFormation
            // gives the resource beside its own.
            Self::EcsTaskDefinition => {
                ecs_task_definition::create(ctx, region, resource, propagated).await
            }
            Self::CognitoIdentityPool(kind) => {
                cognito_identity_pool::create(ctx, region, kind, resource, propagated).await
            }
            // A POOL is taggable: its `UserPoolTags` property is the tag map `CreateUserPool`
            // takes, so it carries the tags CloudFormation gives the resource beside its own.
            // Neither an app client nor an identity provider is: every tagging call of that
            // service addresses a pool, so there is no call that could carry a tag to one.
            Self::CognitoUserPool(kind) => {
                cognito_user_pool::create(ctx, region, kind, resource, propagated).await
            }
            // A rotation schedule carries no tags of its own: it is not a resource of the
            // owning service, but the rotation CONFIGURATION of the secret it names, so
            // CloudFormation propagates none to it.
            Self::SecretsManagerRotationSchedule => {
                secretsmanager_rotation_schedule::create(ctx, region, resource).await
            }
            // A resource policy carries no tags of its own either: it is not a resource of
            // the owning service, but the POLICY of the secret it names, so CloudFormation
            // propagates none to it.
            Self::SecretsManagerResourcePolicy => {
                secretsmanager_resource_policy::create(ctx, region, resource).await
            }
            // A secret IS taggable: its `Tags` property is the tag list `CreateSecret` takes,
            // so it carries the tags CloudFormation gives the resource beside its own.
            Self::SecretsManagerSecret => {
                secretsmanager_secret::create(ctx, region, resource, propagated).await
            }
            // A nested stack carries the stack-level tags CloudFormation propagates to the
            // resources it creates and is created under the acknowledgement the parent's own
            // operation gave, which `CreateStack` takes as its `Tags` and `Capabilities`.
            //
            // BOXED, unlike every other arm: this call reaches CloudFormation's OWN
            // `CreateStack`, so the whole deploy-a-stack chain — this walk included — runs
            // again inside it. Boxing keeps the inner chain's state off this future, so the
            // stack a request needs does not multiply with the nesting the template declares.
            //
            // HOW DEEP that re-entry can go is bounded before the call is made, not here:
            // every level carries the chain of template references above it, and
            // `nested_stack::beyond_the_bounds` refuses a child past
            // `nested_stack::MAX_NESTING_DEPTH` — and one whose own reference the chain is
            // already deploying, which is what a template that nests itself is — as this
            // resource's failure. So the number of re-entries is fixed by that ceiling rather
            // than by the template graph, and a cyclic reference fails the resource instead of
            // recursing. The same bound is applied by the update and delete arms below, which
            // re-enter the same way.
            Self::NestedStack => {
                Box::pin(nested_stack::create(
                    ctx,
                    region,
                    resource,
                    propagated,
                    acknowledged,
                ))
                .await
            }
            Self::StepFunctionsStateMachine => {
                step_functions_state_machine::create(ctx, region, resource, propagated).await
            }
            // Neither a published version nor an alias takes tags: Step Functions' tagging
            // calls address a state machine or an activity, so there is no call that could
            // carry a tag to one.
            Self::StepFunctionsVersionOrAlias(kind) => {
                step_functions_versions_and_aliases::create(ctx, region, kind, resource).await
            }
            Self::ApiGatewayRestApi(kind) => {
                apigateway_rest_api::create(ctx, region, kind, resource, propagated).await
            }
            // Neither of these types publishes a `Tags` property, so CloudFormation
            // propagates no tag to one: there is no call that could carry one.
            Self::ApplicationAutoScaling(kind) => {
                application_auto_scaling::create(ctx, region, kind, resource).await
            }
            // Every one of these types IS taggable: its `Tags` property is the tag map each
            // AppConfig create takes, so the resource carries the tags CloudFormation gives it
            // beside its own.
            Self::AppConfigResource(kind) => {
                appconfig_resources::create(ctx, region, kind, resource, propagated).await
            }
            // A hosted configuration version is not taggable — AppConfig publishes no call that
            // carries a tag to one — while a DEPLOYMENT is: `StartDeployment$Tags` is the tag map
            // that call takes, so the rollout carries the tags CloudFormation gives the resource
            // beside its own ([`appconfig_rollout::create`]).
            Self::AppConfigRollout(kind) => {
                appconfig_rollout::create(ctx, region, kind, resource, propagated).await
            }
            // A GraphQL API is taggable: its `Tags` property is the tag map `CreateGraphqlApi`
            // takes, so the API carries the tags CloudFormation gives it beside its own. None
            // of its children is: every tagging call of that service addresses an API, so
            // there is no call that could carry a tag to a schema, a data source, a resolver, a
            // function or a key.
            Self::AppSyncResource(kind) => {
                appsync_resources::create(ctx, region, kind, resource, propagated).await
            }
            // Every one of these types publishes a `Tags` property and every one of the four
            // creates takes that list as its own member, so a stack's load balancer, target
            // group, listener and rule carry the tags CloudFormation gives them.
            Self::ElasticLoadBalancing(kind) => {
                elastic_load_balancing::create(ctx, region, kind, resource, propagated).await
            }
            // A DISTRIBUTION is taggable: its `Tags` property is the tag list the create that
            // carries tags takes, so it carries the tags CloudFormation gives it beside its
            // own. An origin access control is not: that type publishes no `Tags` property
            // and `CreateOriginAccessControl` takes none.
            Self::CloudFrontResource(kind) => {
                // Boxed, as the catalogue's own arms are: the enclosing future then holds one
                // pointer instead of this arm's whole state, so adding a type does not
                // enlarge the future a nested stack's provisioning walk keeps on the stack.
                Box::pin(cloudfront_resources::create(
                    ctx, region, kind, resource, propagated,
                ))
                .await
            }
            // A custom DOMAIN is taggable: its `Tags` property is the tag map
            // `CreateDomainName` takes, so it carries the tags CloudFormation gives the
            // resource beside its own. The MAPPING is not: it publishes no `Tags` property
            // and `AssociateApi` takes none, so CloudFormation propagates no tag to one.
            Self::AppSyncDomainName(kind) => {
                appsync_domain_names::create(ctx, region, kind, resource, propagated).await
            }
            // An EVENT API and a channel namespace are both taggable: each type publishes a
            // `Tags` property and its create takes that map, so the resource carries the tags
            // CloudFormation gives it beside its own. An ASSOCIATION is not: it publishes no
            // `Tags` property and `AssociateSourceGraphqlApi` takes none, so CloudFormation
            // propagates no tag to one.
            Self::AppSyncEventOrMergedApi(kind) => {
                appsync_event_and_merged_apis::create(ctx, region, kind, resource, propagated)
                    .await
            }
            // A namespace and a service are both taggable: each type publishes a `Tags`
            // property and its create takes that list, so the resource carries the tags
            // CloudFormation gives it beside its own. An INSTANCE is not: it publishes no
            // `Tags` property and `RegisterInstance` takes none, so CloudFormation propagates
            // no tag to one.
            Self::ServiceDiscovery(kind) => {
                service_discovery::create(ctx, region, kind, resource, propagated).await
            }
            // A workgroup and a data catalog are both taggable: each type publishes a `Tags`
            // property and its create takes that list, so the resource carries the tags
            // CloudFormation gives it beside its own — less the reserved namespace, which
            // Athena refuses on every public call that takes a tag list. A SAVED QUERY is not:
            // it publishes no `Tags` property and `CreateNamedQuery` takes none, so
            // CloudFormation propagates no tag to one.
            Self::Athena(kind) => {
                athena_resources::create(ctx, region, kind, resource, propagated).await
            }
            // All three MediaConvert types are taggable: each publishes a `Tags` property and
            // its create takes that map, so the resource carries the tags CloudFormation gives
            // it beside its own — less the reserved namespace, which that service publishes no
            // call to write from outside AWS.
            Self::MediaConvert(kind) => {
                mediaconvert_resources::create(ctx, region, kind, resource, propagated).await
            }
            // A patch baseline IS taggable: its `Tags` property is the tag list
            // `CreatePatchBaseline` takes, so it carries the tags CloudFormation gives the
            // resource beside its own ([`ssm_patch_baseline::create`], which also registers
            // the patch groups the declaration asks for).
            Self::SsmPatchBaseline => {
                ssm_patch_baseline::create(ctx, region, resource, propagated).await
            }
        }
    }

    /// Whether the owning service reports the created resource ready — see
    /// [`Provisioner::readiness`](super::provisioner::Provisioner::readiness).
    ///
    /// `declared` is the properties the resource was declared with, where the records say
    /// which they are, so an arm that waits for a declared SETTING of a resource asks about
    /// the ones the declaration carries rather than about every setting of the type
    /// ([`dynamodb_table::readiness`]). `None` means the declaration is not known here, and
    /// an arm that reads it then holds to whatever that service reports about the resource.
    pub(super) async fn readiness(
        self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        physical_resource_id: &str,
        declared: Option<&Value>,
    ) -> Result<Readiness, Refusal> {
        match self {
            Self::Builtin(provisioner) => {
                // Boxed for the reason [`Self::create`]'s arm records.
                Box::pin(provisioner.readiness(ctx, region, physical_resource_id, declared)).await
            }
            Self::DynamodbTable => {
                Ok(dynamodb_table::readiness(ctx, region, physical_resource_id, declared).await)
            }
            // A global table is a COMPOSITE resource: the table itself and then each
            // replica of its group has a window of its own, and the resource is ready once
            // that service reports every one of them settled
            // ([`dynamodb_table::global_table::readiness`]).
            Self::DynamodbGlobalTable => Ok(dynamodb_table::global_table::readiness(
                ctx,
                region,
                physical_resource_id,
                declared,
            )
            .await),
            // The document is embedded in every entity it names the moment the put answers.
            Self::IamPolicy(_) => Ok(Readiness::Ready),
            // The alias is in force, and the version exists and can be invoked, the moment
            // the call that made it answers: both are snapshots of a function AWS Lambda
            // already reports settled.
            Self::LambdaAlias | Self::LambdaVersion => Ok(Readiness::Ready),
            // `CreateEventSourceMapping` answers the mapping `Creating`, in which it reads
            // nothing from its event source, so this arm waits for AWS Lambda to report it in
            // the state its `Enabled` setting settles it in
            // ([`lambda_event_source_mapping::readiness`]).
            Self::LambdaEventSourceMapping => {
                Ok(lambda_event_source_mapping::readiness(ctx, region, physical_resource_id).await)
            }
            Self::LambdaEventInvokeConfig => Ok(lambda_event_invoke_config::readiness()),
            Self::LambdaFunction => Ok(lambda_function::readiness()),
            // The version exists, and a function may name it, the moment
            // `PublishLayerVersion` answers.
            Self::LambdaLayerVersion => Ok(Readiness::Ready),
            // The statement is in force the moment `AddPermission` answers.
            Self::LambdaPermission => Ok(Readiness::Ready),
            // A log group, a stream and a metric filter are each in force the moment the
            // call that made them answers, and an authorizer exists in the status its
            // declaration asked for.
            Self::LogsResource(_) => Ok(logs_resources::ready()),
            Self::IotAuthorizer => Ok(iot_authorizer::ready()),
            // A patch baseline exists from the moment its create answers too.
            Self::SsmPatchBaseline => Ok(ssm_patch_baseline::ready()),
            // A provisioning template and a role alias each EXIST from the moment their
            // create answers ([`iot_provisioning_resources::ready`]).
            Self::IotProvisioningResource(_) => Ok(iot_provisioning_resources::ready()),
            // A rule is LIVE the moment `CreateTopicRule` answers
            // ([`iot_topic_rule::ready`]).
            Self::IotTopicRule => Ok(iot_topic_rule::ready()),
            // A distribution and an origin access control each EXIST from the moment their
            // create answers ([`cloudfront_resources::ready`]).
            Self::CloudFrontResource(_) => Ok(cloudfront_resources::ready()),
            // A task-definition revision is `ACTIVE` the moment `RegisterTaskDefinition`
            // answers.
            Self::EcsTaskDefinition => Ok(ecs_task_definition::ready()),
            // Each of these resources EXISTS from the moment its create answers: the table
            // takes its routes, the association holds, the gateway is there to be attached,
            // the attachment is in force, the address is allocated, and the endpoint's own
            // route is in the tables the create named. A NAT GATEWAY is the one that comes up
            // in a state nothing routes through, which that arm waits out
            // ([`ec2_vpc_network::readiness`]).
            Self::Ec2VpcNetwork(kind) => {
                ec2_vpc_network::readiness(ctx, region, kind, physical_resource_id).await
            }
            // `RunInstances` answers the instance `pending`, in which nothing runs on it, so
            // this arm waits for Amazon EC2 to report it `running`
            // ([`ec2_instance::readiness`]).
            Self::Ec2Instance => {
                ec2_instance::readiness(ctx, region, physical_resource_id).await
            }
            // A pool exists, its role configuration is in force, and a provider's
            // principal-tag attribute map is set, from the moment the owning service's own
            // call answers.
            Self::CognitoIdentityPool(_) => Ok(cognito_identity_pool::ready()),
            // A user pool, an app client and an identity provider each exist from the moment
            // their own create answers.
            Self::CognitoUserPool(_) => Ok(cognito_user_pool::ready()),
            // `CreateStack` answers as soon as the stack's creation has STARTED, so the
            // parent's resource is held open until CloudFormation reports the child settled.
            Self::NestedStack => {
                Box::pin(nested_stack::readiness(ctx, region, physical_resource_id)).await
            }
            // `RotateSecret` answers while the rotation it started is still in flight, so
            // the resource is held open until the owning service reports the secret settled
            // ([`secretsmanager_rotation_schedule::readiness`]).
            Self::SecretsManagerRotationSchedule => {
                secretsmanager_rotation_schedule::readiness(ctx, region, physical_resource_id)
                    .await
            }
            Self::SecretsManagerSecret => Ok(secretsmanager_secret::readiness()),
            Self::SecretsManagerResourcePolicy => {
                Ok(secretsmanager_resource_policy::readiness())
            }
            Self::StepFunctionsStateMachine => Ok(step_functions_state_machine::readiness()),
            // A published version and an alias each exist from the moment their own create
            // answers ([`step_functions_versions_and_aliases::ready`]).
            Self::StepFunctionsVersionOrAlias(_) => {
                Ok(step_functions_versions_and_aliases::ready())
            }
            // Each of these resources EXISTS from the moment its create answers: API Gateway
            // reports no state an api, a resource, a method, a deployment or a stage is still
            // coming up in — a stage serves the deployment it points at as soon as the create
            // that made it has answered. A VPC LINK is the one that comes up in a state no
            // integration carries traffic through, which that arm waits out
            // ([`apigateway_rest_api::readiness`]).
            Self::ApiGatewayRestApi(kind) => {
                apigateway_rest_api::readiness(ctx, region, kind, physical_resource_id).await
            }
            // A target is registered, and a policy is in force, from the moment the owning
            // service's own call answers.
            Self::ApplicationAutoScaling(_) => Ok(application_auto_scaling::ready()),
            // An application, an environment, a configuration profile and a deployment
            // strategy each exist from the moment their own create answers.
            Self::AppConfigResource(_) => Ok(appconfig_resources::ready()),
            // A hosted configuration version exists from the moment its create answers; a
            // DEPLOYMENT is reported at the first stage of its rollout, so that arm waits for
            // AppConfig to report the rollout finished
            // ([`appconfig_rollout::readiness`]).
            Self::AppConfigRollout(kind) => {
                appconfig_rollout::readiness(ctx, region, kind, physical_resource_id).await
            }
            // An API, a data source, a resolver, a function and a key each exist from the
            // moment their own create answers; a SCHEMA is installed ASYNCHRONOUSLY, so that
            // arm reads the creation's status back
            // ([`appsync_resources::readiness`]).
            Self::AppSyncResource(kind) => {
                appsync_resources::readiness(ctx, region, kind, physical_resource_id).await
            }
            // `CreateLoadBalancer` answers the load balancer `provisioning`, in which it
            // serves no connection, so that arm waits for Elastic Load Balancing to report it
            // `active` ([`elastic_load_balancing::readiness`]); a target group, a listener and
            // a rule each exist the moment their own create answers.
            Self::ElasticLoadBalancing(kind) => {
                elastic_load_balancing::readiness(ctx, region, kind, physical_resource_id).await
            }
            // A custom domain exists as `CreateDomainName` answers; a MAPPING is reported
            // `PROCESSING` until AppSync has deployed it, which that arm waits out
            // ([`appsync_domain_names::readiness`]).
            Self::AppSyncDomainName(kind) => {
                appsync_domain_names::readiness(ctx, region, kind, physical_resource_id).await
            }
            // An Event API and a channel namespace exist as their creates answer; an
            // ASSOCIATION comes up `MERGE_SCHEDULED`, the merge it started being what makes the
            // Merged API serve the source API's schema, so that arm waits it out
            // ([`appsync_event_and_merged_apis::readiness`]).
            Self::AppSyncEventOrMergedApi(kind) => {
                appsync_event_and_merged_apis::readiness(ctx, region, kind, physical_resource_id)
                    .await
            }
            // Cloud Map holds a namespace, a service and an instance from the moment the call
            // that makes it has answered, and none of those shapes publishes a status member at
            // all, so nothing is asked and nothing waits
            // ([`service_discovery::ready`]).
            Self::ServiceDiscovery(_) => Ok(service_discovery::ready()),
            // A WORKGROUP exists as its create answers and publishes no status at all, and so
            // does a SAVED QUERY; a DATA CATALOG publishes one, and Athena creates a federated
            // catalog asynchronously, so that arm asks ([`athena_resources::readiness`]).
            Self::Athena(kind) => {
                athena_resources::readiness(ctx, region, kind, physical_resource_id).await
            }
            // A queue, a job template and a preset each EXIST from the moment their own create
            // answers, and none of the three shapes publishes a state a brand-new one is still
            // coming up in ([`mediaconvert_resources::ready`]).
            Self::MediaConvert(_) => Ok(mediaconvert_resources::ready()),
        }
    }

    /// The attributes the owning service publishes for a resource it now reports up — see
    /// [`Provisioner::ready_attributes`](super::provisioner::Provisioner::ready_attributes).
    pub(super) async fn ready_attributes(
        self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        resource_type: &str,
        physical_resource_id: &str,
    ) -> Result<BTreeMap<String, String>, Refusal> {
        match self {
            Self::Builtin(provisioner) => {
                // Boxed for the reason [`Self::create`]'s arm records.
                Box::pin(provisioner.ready_attributes(
                    ctx,
                    region,
                    resource_type,
                    physical_resource_id,
                ))
                .await
            }
            // A function's `Arn` is the one its create answered with, so a resource this
            // owner created has every attribute it publishes already, and neither a layer
            // version nor a policy statement publishes one at all.
            // A mapping publishes the id its own create answered, which the resource already
            // carries as its physical id, so there is nothing left to read for it either; and
            // an inline policy publishes no attribute at all.
            // An alias's and a version's identifiers are the ones their own create
            // answered with, so there is nothing left to read for either.
            // An asynchronous-invocation configuration publishes no attribute at all: the
            // type's only return value is the pair that addresses it, which the resource
            // already carries as its physical id.
            Self::IamPolicy(_)
            | Self::LambdaAlias
            | Self::LambdaEventInvokeConfig
            | Self::LambdaEventSourceMapping
            | Self::LambdaFunction
            | Self::LambdaLayerVersion
            | Self::LambdaPermission
            | Self::LambdaVersion => Ok(BTreeMap::new()),
            // A table's `Arn` and `StreamArn` are the ones its own `CreateTable` answered
            // with, and a global table's `TableId` beside them, so there is nothing left to
            // read once either resource is up.
            Self::DynamodbTable | Self::DynamodbGlobalTable => Ok(BTreeMap::new()),
            // A log group's `Arn` is the one CloudFormation's own naming rules compute for
            // it, and neither a stream nor a metric filter publishes an attribute at all; an
            // authorizer's `Arn` is the one its create answered with. So there is nothing
            // left to read once any of them is up.
            Self::LogsResource(_) | Self::IotAuthorizer => Ok(BTreeMap::new()),
            // A patch baseline's `Id` is the id its own create minted, so there is nothing
            // left to read once it is up.
            Self::SsmPatchBaseline => Ok(BTreeMap::new()),
            // A provisioning template's `TemplateArn` and a role alias's `RoleAliasArn` are
            // the ones their own creates answered with, so there is nothing left to read once
            // either is up.
            Self::IotProvisioningResource(_) => Ok(BTreeMap::new()),
            // A topic rule's `Arn` is the one its create read back from the service that
            // minted it, so there is nothing left to read once the rule is up.
            Self::IotTopicRule => Ok(BTreeMap::new()),
            // A distribution's id and domain name, and an origin access control's id, are the
            // ones their own creates answered with, so there is nothing left to read once
            // either resource is up.
            Self::CloudFrontResource(_) => Ok(BTreeMap::new()),
            // A custom domain's endpoint, zone and ARN are the ones `CreateDomainName`
            // answered with, and a mapping's identifier is the domain it was made on, so
            // there is nothing left to read once either resource is up.
            Self::AppSyncDomainName(_) => Ok(BTreeMap::new()),
            // An Event API's id, ARN and endpoints, and a channel namespace's ARN, are the ones
            // their own creates answered with; an ASSOCIATION publishes the STATE of the link,
            // which the merge its create scheduled has since left behind, so that one is read
            // again ([`appsync_event_and_merged_apis::ready_attributes`]).
            Self::AppSyncEventOrMergedApi(kind) => {
                appsync_event_and_merged_apis::ready_attributes(
                    ctx,
                    region,
                    kind,
                    physical_resource_id,
                )
                .await
            }
            // A DNS namespace publishes the Route 53 hosted zone Cloud Map created WITH it,
            // which lives in another service and is therefore read again; a service's id, ARN
            // and name are the ones its own create answered with, and an instance publishes
            // nothing ([`service_discovery::ready_attributes`]).
            Self::ServiceDiscovery(kind) => {
                service_discovery::ready_attributes(ctx, region, kind, physical_resource_id)
                    .await
            }
            // A workgroup's and a data catalog's identity is the NAME the stack created it
            // under, and a saved query's is the id its own create answered with, and none of
            // the three publishes a further `Fn::GetAtt` attribute this emulator carries, so
            // there is nothing to read once one is up.
            Self::Athena(_) => Ok(BTreeMap::new()),
            // A MediaConvert queue, job template and preset each publish the NAME the stack
            // created them under and the ARN their own create answered with, so there is
            // nothing left to read once one is up.
            Self::MediaConvert(_) => Ok(BTreeMap::new()),
            // A task-definition revision's `TaskDefinitionArn` is the one its own
            // registration answered with, so there is nothing left to read once it is up.
            Self::EcsTaskDefinition => Ok(BTreeMap::new()),
            // Each of these types publishes its own identifier (an Elastic IP address its
            // address and allocation id, an attachment nothing at all), and the create
            // answered it, so there is nothing to read once the resource is up.
            Self::Ec2VpcNetwork(_) => Ok(BTreeMap::new()),
            // An INSTANCE is the one Amazon EC2 type here with more to read once it is up:
            // its addresses and DNS names are not assigned until it is running
            // ([`ec2_instance::ready_attributes`]).
            Self::Ec2Instance => {
                ec2_instance::ready_attributes(ctx, region, physical_resource_id).await
            }
            // A pool's `Name` is the one its own create answered with, and neither a role
            // attachment nor a principal-tag attribute map publishes an attribute this
            // emulator computes, so there is nothing left to read once the resource is up.
            Self::CognitoIdentityPool(_) => Ok(BTreeMap::new()),
            // A pool's ARN and issuer are read off the id its own create answered, a client's
            // name and secret are what that create answered, and an identity provider
            // publishes no attribute at all — so there is nothing left to read once any of
            // these resources is up.
            Self::CognitoUserPool(_) => Ok(BTreeMap::new()),
            // A nested stack's own id is what its create answered with, and the attributes it
            // publishes are its CHILD's outputs — "Fn::GetAtt … Outputs.<NestedStackOutputName>"
            // — which the child publishes only once it has finished coming up, so they are
            // read now ([`nested_stack::ready_attributes`]).
            Self::NestedStack => {
                Box::pin(nested_stack::ready_attributes(
                    ctx,
                    region,
                    physical_resource_id,
                ))
                .await
            }
            // A secret's ARN and name are the ones its own create answered with, so there is
            // nothing left to read once the resource is up; a state machine's `Arn` and `Name`
            // are the same case.
            // A rotation schedule's `Id` is the ARN of the secret it was configured on, which
            // its own create answered, so there is nothing left to read once it is up.
            Self::SecretsManagerRotationSchedule => Ok(BTreeMap::new()),
            // A resource policy's `Id` is the ARN of the secret it was attached to, which its
            // own create answered, so there is nothing left to read once it is up.
            Self::SecretsManagerResourcePolicy => Ok(BTreeMap::new()),
            Self::SecretsManagerSecret | Self::StepFunctionsStateMachine => Ok(BTreeMap::new()),
            // A version's and an alias's `Arn` are the ones their own creates answered with, so
            // there is nothing left to read once either resource is up.
            Self::StepFunctionsVersionOrAlias(_) => Ok(BTreeMap::new()),
            // An api's id and its root resource, and the id of a resource or a deployment, are
            // the ones their own creates answered with, so there is nothing left to read once
            // the resource is up.
            Self::ApiGatewayRestApi(_) => Ok(BTreeMap::new()),
            // A target publishes no attribute at all, and a policy's `Arn` is the one its own
            // put answered with, so there is nothing left to read once either is up.
            Self::ApplicationAutoScaling(_) => Ok(BTreeMap::new()),
            // Each of these resources publishes the identifier its own create answered — which
            // the resource already carries as its physical id — and a profile's `KmsKeyArn`
            // came back with that same create, so there is nothing left to read once the
            // resource is up.
            Self::AppConfigResource(_) => Ok(BTreeMap::new()),
            // A hosted version's number came back with its own create, so there is nothing left
            // to read for one; a DEPLOYMENT publishes the STATE of the rollout, which the stages
            // it has been through since its create have left behind, so that one is read again
            // ([`appconfig_rollout::ready_attributes`]).
            Self::AppConfigRollout(kind) => {
                appconfig_rollout::ready_attributes(ctx, region, kind, physical_resource_id).await
            }
            // Each AppSync resource publishes the identifiers its own create answered — the
            // API's id, ARN and endpoints among them — so there is nothing left to read once
            // the resource is up.
            Self::AppSyncResource(_) => Ok(BTreeMap::new()),
            // A load balancer is the one type here with more to read once it is up: the host
            // it publishes, its hosted zone and the groups in front of it are what Elastic
            // Load Balancing fills in as it provisions the load balancer
            // ([`elastic_load_balancing::ready_attributes`]). A target group, a listener and a
            // rule publish the ARN their own create answered, so there is nothing left to read
            // for any of them.
            Self::ElasticLoadBalancing(kind) => {
                elastic_load_balancing::ready_attributes(
                    ctx,
                    region,
                    kind,
                    physical_resource_id,
                )
                .await
            }
        }
    }

    /// Whether the owning service has finished removing a deleted resource — see
    /// [`Provisioner::removal`](super::provisioner::Provisioner::removal).
    pub(super) async fn removal(
        self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        physical_resource_id: &str,
    ) -> Result<Removal, Refusal> {
        match self {
            Self::Builtin(provisioner) => {
                // Boxed for the reason [`Self::create`]'s arm records.
                Box::pin(provisioner.removal(ctx, region, physical_resource_id)).await
            }
            // The table in the stack's own Region is the resource either DynamoDB type is
            // addressed by, so the same read answers whether that service still holds it.
            Self::DynamodbTable | Self::DynamodbGlobalTable => {
                dynamodb_table::removal(ctx, region, physical_resource_id).await
            }
            // `Delete<entity>Policy` takes the embedded document away, so there is nothing
            // to wait for.
            Self::IamPolicy(_) => Ok(Removal::Gone),
            // `DeleteAlias` takes the alias away, so there is nothing to wait for.
            Self::LambdaAlias => Ok(Removal::Gone),
            Self::LambdaVersion => {
                lambda_version::removal(ctx, region, physical_resource_id).await
            }
            Self::LambdaEventSourceMapping => {
                lambda_event_source_mapping::removal(ctx, region, physical_resource_id).await
            }
            Self::LambdaFunction => {
                lambda_function::removal(ctx, region, physical_resource_id).await
            }
            // `DeleteLayerVersion` takes the version away, so there is nothing to wait for.
            Self::LambdaLayerVersion => Ok(Removal::Gone),
            // `RemovePermission` takes the statement away, so there is nothing to wait for.
            Self::LambdaPermission => Ok(Removal::Gone),
            // `DeleteFunctionEventInvokeConfig` takes the configuration away as it answers,
            // so there is nothing to wait for either.
            Self::LambdaEventInvokeConfig => Ok(Removal::Gone),
            // Each of these deletes takes the resource away as it answers: neither
            // CloudWatch Logs nor AWS IoT reports a state a removed one is still being
            // deleted in.
            Self::LogsResource(_) => Ok(logs_resources::removal()),
            Self::IotAuthorizer => Ok(iot_authorizer::removal()),
            // `DeletePatchBaseline` takes the baseline away as it answers, so there is
            // nothing to wait for either.
            Self::SsmPatchBaseline => Ok(ssm_patch_baseline::removal()),
            Self::IotProvisioningResource(_) => Ok(iot_provisioning_resources::removal()),
            // `DeleteTopicRule` takes the rule away as it answers too
            // ([`iot_topic_rule::removal`]).
            Self::IotTopicRule => Ok(iot_topic_rule::removal()),
            // An origin access control is gone as its delete answers; a DISTRIBUTION has to
            // be disabled and its disabled configuration has to reach the edge locations
            // first, which this arm waits out ([`cloudfront_resources::removal`]).
            Self::CloudFrontResource(kind) => {
                // Boxed for the reason [`Self::create`]'s arm records.
                Box::pin(cloudfront_resources::removal(
                    ctx,
                    region,
                    kind,
                    physical_resource_id,
                ))
                .await
            }
            // `DeregisterTaskDefinition` marks the revision `INACTIVE` as it answers, so
            // there is nothing to wait for.
            Self::EcsTaskDefinition => Ok(ecs_task_definition::removal()),
            // Each of these deletes takes the resource away as it answers — Amazon EC2
            // reports no state a deleted route table, association, endpoint, gateway,
            // attachment, address or route is still being removed in — except the NAT
            // gateway, whose `deleting` state that arm waits out
            // ([`ec2_vpc_network::removal`]).
            Self::Ec2VpcNetwork(kind) => {
                ec2_vpc_network::removal(ctx, region, kind, physical_resource_id).await
            }
            // `TerminateInstances` moves the instance into `shutting-down`, so this arm waits
            // for Amazon EC2 to report it `terminated` ([`ec2_instance::removal`]).
            Self::Ec2Instance => {
                ec2_instance::removal(ctx, region, physical_resource_id).await
            }
            // `DeleteIdentityPool` takes the pool away as it answers, and the calls that
            // clear a pool's roles and put a provider back on the default principal-tag
            // mappings leave nothing behind either.
            Self::CognitoIdentityPool(_) => Ok(Removal::Gone),
            // `DeleteUserPool`, `DeleteUserPoolClient` and `DeleteIdentityProvider` each take
            // the resource away as they answer.
            Self::CognitoUserPool(_) => Ok(Removal::Gone),
            // `DeleteStack` starts the child's deletion, so the parent's resource is held
            // open until CloudFormation no longer holds the stack.
            Self::NestedStack => {
                Box::pin(nested_stack::removal(ctx, region, physical_resource_id)).await
            }
            Self::SecretsManagerRotationSchedule => {
                Ok(secretsmanager_rotation_schedule::removal())
            }
            Self::SecretsManagerResourcePolicy => {
                Ok(secretsmanager_resource_policy::removal())
            }
            Self::SecretsManagerSecret => {
                secretsmanager_secret::removal(ctx, region, physical_resource_id).await
            }
            Self::StepFunctionsStateMachine => {
                step_functions_state_machine::removal(ctx, region, physical_resource_id).await
            }
            // `DeleteStateMachineVersion` and `DeleteStateMachineAlias` each take the resource
            // away as they answer, so there is nothing to wait for.
            Self::StepFunctionsVersionOrAlias(_) => {
                Ok(step_functions_versions_and_aliases::removal())
            }
            // Every API Gateway delete here takes the resource away as it answers: that
            // service reports no state a deleted api, resource, method, deployment, stage or
            // VPC link is still being removed in.
            Self::ApiGatewayRestApi(_) => Ok(Removal::Gone),
            // Deregistering a target and deleting a policy each take the resource away as the
            // call answers: Application Auto Scaling reports no state a deleted one is still
            // being removed in.
            Self::ApplicationAutoScaling(_) => Ok(Removal::Gone),
            // Each AppConfig delete takes the resource away as it answers: that service reports
            // no state a deleted application, environment, profile or strategy is still being
            // removed in.
            Self::AppConfigResource(_) => Ok(appconfig_resources::removal()),
            // `DeleteHostedConfigurationVersion` takes the version away as it answers, and a
            // deployment's removal stops a rollout still in flight, which AppConfig reports no
            // state a removed resource is still being deleted in
            // ([`appconfig_rollout::removal`]).
            Self::AppConfigRollout(_) => Ok(appconfig_rollout::removal()),
            // Each AppSync delete takes the resource away as it answers: that service reports
            // no state a removed API, data source, resolver, function or key is still being
            // deleted in.
            Self::AppSyncResource(_) => Ok(appsync_resources::removal()),
            // Each Elastic Load Balancing delete takes the resource away as it answers: that
            // service reports no state a deleted load balancer, target group, listener or rule
            // is still being removed in.
            Self::ElasticLoadBalancing(_) => Ok(elastic_load_balancing::removal()),
            // `DeleteDomainName` takes the domain away as it answers — the mapping it
            // carried with it — and `DisassociateApi` leaves the domain carrying none, so
            // there is nothing to wait for either.
            Self::AppSyncDomainName(_) => Ok(appsync_domain_names::removal()),
            // `DeleteApi` and `DeleteChannelNamespace` each take the resource away as they
            // answer; `DisassociateSourceGraphqlApi` SCHEDULES the association's deletion, so
            // that arm asks until AppSync no longer holds the link
            // ([`appsync_event_and_merged_apis::removal`]).
            Self::AppSyncEventOrMergedApi(kind) => {
                appsync_event_and_merged_apis::removal(ctx, region, kind, physical_resource_id)
                    .await
            }
            // `DeleteNamespace`, `DeleteService` and `DeregisterInstance` each take the resource
            // away as they answer, so there is nothing to wait for
            // ([`service_discovery::removal`]).
            Self::ServiceDiscovery(_) => Ok(service_discovery::removal()),
            // `DeleteWorkGroup`, `DeleteDataCatalog` and `DeleteNamedQuery` each take the
            // resource away as they answer, so there is nothing to wait for
            // ([`athena_resources::removal`]).
            Self::Athena(_) => Ok(athena_resources::removal()),
            // `DeleteQueue`, `DeleteJobTemplate` and `DeletePreset` each take the resource away
            // as they answer, so there is nothing to wait for
            // ([`mediaconvert_resources::removal`]).
            Self::MediaConvert(_) => Ok(mediaconvert_resources::removal()),
        }
    }

    /// Apply a changed declaration to a resource that exists — see
    /// [`Provisioner::update`](super::provisioner::Provisioner::update).
    ///
    /// The declaration being applied is validated against what its type's schema requires
    /// first, as the create's is: the provider validates every model it is handed, so an
    /// update that DROPS a property that schema marks required fails the resource with the
    /// validator's own sentence rather than being applied without it.
    pub(super) async fn update(
        self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        resource: &StoredResource,
        declared: &Value,
        propagated: &[(String, String)],
        acknowledged: &[String],
    ) -> Result<Option<Created>, Refusal> {
        if let Some(refusal) = model_validation::refusal(&resource.resource_type, declared) {
            return Err(refusal);
        }
        match self {
            // The bucket's own provider, whose update half lives beside this algorithm
            // ([`s3_bucket`]): a property Amazon S3 publishes a call to write on a live
            // bucket is applied to the bucket the stack created rather than replacing it.
            Self::Builtin(Provisioner::S3Bucket) => {
                s3_bucket::update(ctx, region, resource, declared).await
            }
            Self::Builtin(provisioner) => {
                // Boxed for the reason [`Self::create`]'s arm records.
                Box::pin(provisioner.update(ctx, region, resource, declared, propagated)).await
            }
            Self::DynamodbTable => {
                dynamodb_table::update(ctx, region, resource, declared).await
            }
            // A changed declaration of a global table moves the Regions its group spans,
            // which is `UpdateTable`'s `ReplicaUpdates`
            // ([`dynamodb_table::global_table::update`]).
            Self::DynamodbGlobalTable => {
                dynamodb_table::global_table::update(ctx, region, resource, declared).await
            }
            Self::IamPolicy(kind) => iam_policy::update(ctx, region, kind, resource, declared)
                .await
                .map(|()| Some(Created::default())),
            Self::LambdaAlias => lambda_alias::update(ctx, region, resource, declared)
                .await
                .map(Some),
            // A published version cannot be CHANGED: its code and configuration are the
            // snapshot the publish took, and Lambda publishes no call that edits one — so a
            // changed declaration is reported as not applied rather than as a success that
            // dropped the caller's change.
            Self::LambdaVersion => Ok(None),
            Self::LambdaEventSourceMapping => {
                lambda_event_source_mapping::update(ctx, region, resource, declared).await
            }
            // `PutFunctionEventInvokeConfig` writes the WHOLE configuration of the qualifier
            // it addresses, so a changed declaration is applied by putting it
            // ([`lambda_event_invoke_config::update`]).
            Self::LambdaEventInvokeConfig => {
                lambda_event_invoke_config::update(ctx, region, resource, declared)
                    .await
                    .map(Some)
            }
            Self::LambdaFunction => lambda_function::update(ctx, region, resource, declared)
                .await
                .map(Some),
            // A published layer version cannot be CHANGED: Lambda publishes no call that
            // edits one, so a changed declaration is reported as not applied rather than as
            // a success that dropped the caller's change.
            Self::LambdaLayerVersion => Ok(None),
            // A statement of a policy cannot be CHANGED in place: `AddPermission` adds one
            // and `RemovePermission` removes one, so a changed declaration is applied by
            // replacing the statement with the two calls Lambda publishes
            // ([`lambda_permission::update`]).
            Self::LambdaPermission => lambda_permission::update(ctx, region, resource, declared)
                .await
                .map(Some),
            Self::LogsResource(kind) => {
                logs_resources::update(ctx, region, kind, resource, declared).await
            }
            Self::IotAuthorizer => {
                iot_authorizer::update(ctx, region, resource, declared).await
            }
            // A changed declaration is written to the baseline the stack already has, and the
            // patch groups it registers are brought into line with what it now asks for
            // ([`ssm_patch_baseline::update`]).
            Self::SsmPatchBaseline => {
                ssm_patch_baseline::update(ctx, region, resource, declared).await
            }
            Self::IotProvisioningResource(kind) => {
                iot_provisioning_resources::update(ctx, region, kind, resource, declared).await
            }
            // `ReplaceTopicRule` writes the WHOLE payload of the rule it addresses by name, so
            // a changed declaration is applied by replacing it and the rule keeps its identity
            // and its ARN ([`iot_topic_rule::update`]).
            Self::IotTopicRule => iot_topic_rule::update(ctx, region, resource, declared)
                .await
                .map(Some),
            // Both CloudFront types are changed by that service's own update, which takes the
            // whole configuration and the version the caller read, so the resource keeps the
            // id its create minted ([`cloudfront_resources::update`]).
            Self::CloudFrontResource(kind) => {
                // Boxed for the reason [`Self::create`]'s arm records.
                Box::pin(cloudfront_resources::update(
                    ctx, region, kind, resource, declared, propagated,
                ))
                .await
            }
            // A task-definition revision cannot be CHANGED: Amazon ECS publishes no call that
            // edits one, so a changed declaration registers a new revision rather than
            // reaching this ([`ecs_task_definition::update`]).
            Self::EcsTaskDefinition => Ok(ecs_task_definition::update()),
            // Each of these Amazon EC2 types takes a changed property through the call that
            // service publishes for changing it on the resource that exists — a gateway
            // attachment's gateway through the detach and attach pair, a route's target
            // through `ReplaceRoute`, an endpoint's members through `ModifyVpcEndpoint`, a
            // rule's description through the update for its direction, and every taggable one's
            // tags through the tagging calls ([`ec2_vpc_network::update`]) — so the resource
            // keeps the identity it has. A property no such call carries is create-only and
            // replaces the resource instead.
            Self::Ec2VpcNetwork(kind) => {
                ec2_vpc_network::update(ctx, region, kind, resource, declared).await
            }
            // An INSTANCE takes the policies `ModifyInstanceAttribute` writes while it runs,
            // and its tags ([`ec2_instance::update`]); the properties that are part of the
            // machine it runs as are either create-only or applied by stopping the instance,
            // which this deployment does not do.
            Self::Ec2Instance => ec2_instance::update(ctx, region, resource, declared).await,
            Self::CognitoIdentityPool(kind) => {
                cognito_identity_pool::update(ctx, region, kind, resource, declared).await
            }
            Self::CognitoUserPool(kind) => {
                cognito_user_pool::update(ctx, region, kind, resource, declared, propagated)
                    .await
            }
            // A nested stack is CHANGED by CloudFormation's own `UpdateStack`, under the id
            // the create minted, so the child keeps its identity and its resources. Boxed for
            // the reason [`Self::create`]'s arm records.
            Self::NestedStack => {
                Box::pin(nested_stack::update(
                    ctx,
                    region,
                    resource,
                    declared,
                    propagated,
                    acknowledged,
                ))
                .await
            }
            Self::SecretsManagerRotationSchedule => {
                secretsmanager_rotation_schedule::update(ctx, region, resource, declared).await
            }
            Self::SecretsManagerResourcePolicy => {
                secretsmanager_resource_policy::update(ctx, region, resource, declared).await
            }
            Self::SecretsManagerSecret => {
                secretsmanager_secret::update(ctx, region, resource, declared).await
            }
            Self::StepFunctionsStateMachine => {
                step_functions_state_machine::update(ctx, region, resource, declared, propagated)
                    .await
            }
            // An alias is re-pointed in place by `UpdateStateMachineAlias`; a published
            // VERSION is the snapshot its publish took, so a changed declaration of one is
            // deployed as a new version rather than applied
            // ([`step_functions_versions_and_aliases::update`]).
            Self::StepFunctionsVersionOrAlias(kind) => {
                step_functions_versions_and_aliases::update(ctx, region, kind, resource, declared)
                    .await
            }
            // A CHANGED declaration of the api, of a resource of it, of a method, of a
            // deployment or of a stage is applied through the PATCH call API Gateway publishes
            // for that record, which is how that service changes one; a change no pointer of
            // that call can express is reported as NOT applied rather than as a success that
            // dropped the caller's change ([`apigateway_rest_api::update`]).
            Self::ApiGatewayRestApi(kind) => {
                apigateway_rest_api::update(ctx, region, kind, resource, declared).await
            }
            Self::ApplicationAutoScaling(kind) => {
                application_auto_scaling::update(ctx, region, kind, resource, declared).await
            }
            Self::AppConfigResource(kind) => {
                appconfig_resources::update(ctx, region, kind, resource, declared).await
            }
            // Every property of a hosted configuration version and of a deployment is fixed when
            // the resource is made, so a change to either is reported as NOT applied rather than
            // as a success that dropped the caller's change — which is why a deployment restating
            // one replaces the resource ([`appconfig_rollout::update`]).
            Self::AppConfigRollout(kind) => {
                appconfig_rollout::update(ctx, region, kind, resource, declared).await
            }
            Self::AppSyncResource(kind) => {
                appsync_resources::update(ctx, region, kind, resource, declared).await
            }
            Self::ElasticLoadBalancing(kind) => {
                elastic_load_balancing::update(ctx, region, kind, resource, declared).await
            }
            // A domain takes its DESCRIPTION, which is the one member `UpdateDomainName`
            // carries, and a mapping takes its API, `AssociateApi` on a domain that already
            // carries one replacing the mapping ([`appsync_domain_names::update`]).
            Self::AppSyncDomainName(kind) => {
                appsync_domain_names::update(ctx, region, kind, resource, declared).await
            }
            // Each of these three is changed by the update call AppSync publishes for it, which
            // takes what that call takes and no more; a declaration restating a value the call's
            // own path binds names a different resource, which the deployment replaces
            // ([`appsync_event_and_merged_apis::update`]).
            Self::AppSyncEventOrMergedApi(kind) => {
                appsync_event_and_merged_apis::update(ctx, region, kind, resource, declared).await
            }
            // Each namespace takes its description — and a DNS namespace its SOA record —
            // which is what its own update carries; a service takes what `UpdateService`
            // carries, and an instance is re-registered under the same id, which is the one
            // call that changes its attributes ([`service_discovery::update`]).
            Self::ServiceDiscovery(kind) => {
                service_discovery::update(ctx, region, kind, resource, declared).await
            }
            // A workgroup takes what `UpdateWorkGroup` carries and a catalog what
            // `UpdateDataCatalog` carries, each addressed by the name it was created under; a
            // declaration that DROPS a value neither call publishes a way to clear reports the
            // change as not applied, and so does any change to a SAVED QUERY, every property of
            // which is fixed when the query is saved ([`athena_resources::update`]).
            Self::Athena(kind) => {
                athena_resources::update(ctx, region, kind, resource, declared).await
            }
            // Each of these three is changed by the update call MediaConvert publishes for it,
            // addressed by the name it was created under and carrying what that call carries; a
            // declaration that DROPS a value the call publishes a way to clear carries that
            // value, and one restating a queue's pricing plan — which `UpdateQueue` has no
            // member for — replaces the resource rather than reaching this arm
            // ([`mediaconvert_resources::update`]).
            Self::MediaConvert(kind) => {
                mediaconvert_resources::update(ctx, region, kind, resource, declared).await
            }
        }
    }

    /// Bring a carried-forward resource's tags back into step — see
    /// [`Provisioner::retag`](super::provisioner::Provisioner::retag).
    pub(super) async fn retag(
        self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        resource: &StoredResource,
        declared: &Value,
        propagated: &[(String, String)],
    ) -> Result<(), Refusal> {
        match self {
            // A bucket's tags are Amazon S3's own `PutBucketTagging`/`DeleteBucketTagging`,
            // which is where the set a bucket must end up carrying is reconciled
            // ([`s3_bucket::retag`]) — the create's tagging call applies to the bucket it
            // creates and to no bucket that already exists.
            Self::Builtin(Provisioner::S3Bucket) => {
                s3_bucket::retag(ctx, region, resource, declared, propagated).await
            }
            Self::Builtin(provisioner) => {
                // Boxed for the reason [`Self::create`]'s arm records.
                Box::pin(provisioner.retag(ctx, region, resource, declared, propagated)).await
            }
            Self::LambdaFunction => {
                lambda_function::retag(ctx, region, resource, declared, propagated).await
            }
            // A table's tags are DynamoDB's own `TagResource`/`UntagResource`:
            // `CreateTable`'s `Tags` member applies to the table that call creates and to no
            // table that already exists — a table the stack ADOPTED was created by no call of
            // that service's at all — so this is where the set a table is to end up carrying
            // is reconciled ([`dynamodb_table::retag`]).
            Self::DynamodbTable => {
                dynamodb_table::retag(ctx, region, resource, declared, propagated).await
            }
            // A global table states its tag set per REPLICA, and the one this stack's
            // resource carries is the table in its own Region: that replica's declaration is
            // the tag set reconciled, by the same call pair.
            Self::DynamodbGlobalTable => {
                let local = dynamodb_table::global_table::local_replica(declared, region);
                dynamodb_table::retag(ctx, region, resource, &local, propagated).await
            }
            // Neither a layer version nor a policy statement is taggable, and neither is an
            // inline policy, so nothing is asked about their tags; the tags of a mapping that
            // already exists are reconciled by no arm here yet ([`Provisioner::retag`]'s own
            // default).
            // Neither an alias nor a published version is taggable either: Lambda's tagging
            // calls address functions, so nothing is asked about their tags.
            // An asynchronous-invocation configuration is not taggable either: Lambda's
            // tagging calls address functions, so nothing is asked about its tags.
            Self::IamPolicy(_)
            | Self::LambdaAlias
            | Self::LambdaEventInvokeConfig
            | Self::LambdaEventSourceMapping
            | Self::LambdaLayerVersion
            | Self::LambdaPermission
            | Self::LambdaVersion => Ok(()),
            // Neither a log stream nor a metric filter is taggable at all, and the tags of a
            // log group and of an authorizer that already exist are reconciled by no arm here
            // yet ([`Provisioner::retag`]'s own default), so nothing is asked about them.
            Self::LogsResource(_) | Self::IotAuthorizer => Ok(()),
            // The tags of a patch baseline that already exists are reconciled by no arm here
            // yet ([`Provisioner::retag`]'s own default), so nothing is asked about them.
            Self::SsmPatchBaseline => Ok(()),
            // The tags of a provisioning template and of a role alias that already exist are
            // reconciled by no arm here yet either, so nothing is asked about them.
            Self::IotProvisioningResource(_) => Ok(()),
            // The tags of a topic rule that already exists are reconciled by its own update,
            // which re-applies the declaration's whole tag set through `TagResource`
            // ([`iot_topic_rule::update`]), so nothing is asked about them here.
            Self::IotTopicRule => Ok(()),
            // An origin access control is not taggable at all, and the tags of a distribution
            // that already exists are reconciled by no arm here yet
            // ([`Provisioner::retag`]'s own default), so nothing is asked about them.
            Self::CloudFrontResource(_) => Ok(()),
            // A task-definition revision's tags are the ones its registration carried, and no
            // deployment changes a revision, so nothing is asked about them.
            Self::EcsTaskDefinition => Ok(()),
            // The tags of an Amazon EC2 resource that already exists are that service's own
            // `CreateTags` and `DeleteTags`: a create's tagging call applies to the resource it
            // created and to no resource that already exists, and no call that changes one of
            // these resources carries a tag — so this is where the set one is to end up
            // carrying is reconciled ([`ec2_vpc_network::retag`], [`ec2_instance::retag`]).
            Self::Ec2VpcNetwork(kind) => {
                ec2_vpc_network::retag(ctx, region, kind, resource, declared, propagated).await
            }
            Self::Ec2Instance => {
                ec2_instance::retag(ctx, region, resource, declared, propagated).await
            }
            // A pool's tags are replaced by its own update call, which carries the
            // declaration's whole configuration, so nothing is asked about them here.
            Self::CognitoIdentityPool(_) => Ok(()),
            // A user pool's tags are replaced by its own update call, which carries the
            // declaration's whole tag map, and neither an app client nor an identity provider
            // is taggable — so nothing is asked about any of them here.
            Self::CognitoUserPool(_) => Ok(()),
            // A nested stack's own tags are the ones its `UpdateStack` carries, over the
            // template it already has: the set it must end up with is reconciled here, since
            // a deployment can state the stack's tags differently with no declaration
            // changing at all ([`nested_stack::retag`]). Boxed for the reason
            // [`Self::create`]'s arm records.
            Self::NestedStack => {
                Box::pin(nested_stack::retag(ctx, region, resource, declared, propagated))
                    .await
            }
            // The tags of a secret and of a state machine that already exist are reconciled by
            // no arm here yet ([`Provisioner::retag`]'s own default), so nothing is asked about
            // them.
            // A rotation schedule is not taggable at all, so nothing is asked about its tags.
            Self::SecretsManagerRotationSchedule => Ok(()),
            // A resource policy is not taggable either, so nothing is asked about its tags.
            Self::SecretsManagerResourcePolicy => Ok(()),
            Self::SecretsManagerSecret | Self::StepFunctionsStateMachine => Ok(()),
            // Neither a published version nor an alias is taggable at all, so nothing is asked
            // about their tags.
            Self::StepFunctionsVersionOrAlias(_) => Ok(()),
            // Nor are the tags of an api or a stage that already exists ([`Provisioner::retag`]'s
            // own default), so nothing is asked about those either.
            Self::ApiGatewayRestApi(_) => Ok(()),
            // Neither of these types is taggable, so nothing is asked about their tags.
            Self::ApplicationAutoScaling(_) => Ok(()),
            // The tags of an AppConfig resource that already exists are reconciled by no arm
            // here yet ([`Provisioner::retag`]'s own default), so nothing is asked about them.
            Self::AppConfigResource(_) => Ok(()),
            // Neither a hosted configuration version nor a deployment that already exists has its
            // tags reconciled by an arm here yet ([`Provisioner::retag`]'s own default), so
            // nothing is asked about them.
            Self::AppConfigRollout(_) => Ok(()),
            // The tags of a GraphQL API that already exists are reconciled by no arm here yet
            // ([`Provisioner::retag`]'s own default), and no child of one is taggable at all, so
            // nothing is asked about them.
            Self::AppSyncResource(_) => Ok(()),
            // The tags of a load balancer, a target group, a listener or a rule that already
            // exists are reconciled by no arm here yet ([`Provisioner::retag`]'s own default),
            // so nothing is asked about them.
            Self::ElasticLoadBalancing(_) => Ok(()),
            // Nor are the tags of a custom domain that already exists; a mapping is not
            // taggable at all.
            Self::AppSyncDomainName(_) => Ok(()),
            // The tags of an Event API or a channel namespace that already exists are
            // reconciled by no arm here yet ([`Provisioner::retag`]'s own default), and an
            // association is not taggable at all, so nothing is asked about them.
            Self::AppSyncEventOrMergedApi(_) => Ok(()),
            // The tags of a namespace or a service that already exists are reconciled by no arm
            // here yet ([`Provisioner::retag`]'s own default), and an instance is not taggable
            // at all, so nothing is asked about them.
            Self::ServiceDiscovery(_) => Ok(()),
            // The tags of a workgroup or a data catalog that already exists are reconciled by
            // no arm here yet ([`Provisioner::retag`]'s own default), and a saved query is not
            // taggable at all, so nothing is asked about them.
            Self::Athena(_) => Ok(()),
            // The tags of a MediaConvert queue, job template or preset that already exists are
            // reconciled by no arm here yet ([`Provisioner::retag`]'s own default), so nothing
            // is asked about them.
            Self::MediaConvert(_) => Ok(()),
        }
    }

    /// Delete one resource from the service that owns it — see
    /// [`Provisioner::delete`](super::provisioner::Provisioner::delete).
    pub(super) async fn delete(
        self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        resource: &StoredResource,
        final_snapshot: Option<&str>,
    ) -> Result<(), Refusal> {
        match self {
            Self::Builtin(provisioner) => {
                // Boxed for the reason [`Self::create`]'s arm records.
                Box::pin(provisioner.delete(ctx, region, resource, final_snapshot)).await
            }
            // DynamoDB's `DeleteTable` takes no snapshot of a table on the way out: there
            // is no such call (a backup is `CreateBackup`, a call of its own), so a
            // `Snapshot` deletion policy on this type deletes the table with no snapshot
            // invented for it.
            Self::DynamodbTable => dynamodb_table::delete(ctx, region, resource).await,
            // A global table is deleted the way it was created, in reverse: its replicas
            // first, then the table in the stack's own Region
            // ([`dynamodb_table::global_table::delete`]). DynamoDB takes no snapshot of
            // either on the way out, for the reason above.
            Self::DynamodbGlobalTable => {
                dynamodb_table::global_table::delete(ctx, region, resource).await
            }
            // Lambda's `DeleteFunction` takes no snapshot of a function on the way out:
            // there is no such call, so a `Snapshot` deletion policy on this type deletes
            // the function with no snapshot invented for it.
            // IAM takes no snapshot of an embedded policy on the way out: there is no such
            // call, so a `Snapshot` deletion policy on this type deletes it with no snapshot
            // invented for it.
            Self::IamPolicy(kind) => iam_policy::delete(ctx, region, kind, resource).await,
            // Lambda takes no snapshot of a mapping on the way out either: there is no such
            // call, so a `Snapshot` deletion policy on this type deletes the mapping with no
            // snapshot invented for it.
            // Lambda takes no snapshot of an alias or of a published version on the way out
            // either: there is no such call, so a `Snapshot` deletion policy on either type
            // deletes the resource with no snapshot invented for it.
            Self::LambdaAlias => lambda_alias::delete(ctx, region, resource).await,
            Self::LambdaVersion => lambda_version::delete(ctx, region, resource).await,
            Self::LambdaEventSourceMapping => {
                lambda_event_source_mapping::delete(ctx, region, resource).await
            }
            // Lambda takes no snapshot of an asynchronous-invocation configuration on the way
            // out either: there is no such call, so a `Snapshot` deletion policy on this type
            // takes the configuration off with no snapshot invented for it.
            Self::LambdaEventInvokeConfig => {
                lambda_event_invoke_config::delete(ctx, region, resource).await
            }
            Self::LambdaFunction => lambda_function::delete(ctx, region, resource).await,
            // `DeleteLayerVersion` takes no snapshot of a version on the way out either.
            Self::LambdaLayerVersion => {
                lambda_layer_version::delete(ctx, region, resource).await
            }
            Self::LambdaPermission => lambda_permission::delete(ctx, region, resource).await,
            // Neither CloudWatch Logs nor AWS IoT takes a snapshot of one of these on the way
            // out: there is no such call for a log group, a stream, a metric filter or an
            // authorizer, so a `Snapshot` deletion policy on one of them deletes the resource
            // with no snapshot invented for it.
            Self::LogsResource(kind) => {
                logs_resources::delete(ctx, region, kind, resource).await
            }
            Self::IotAuthorizer => iot_authorizer::delete(ctx, region, resource).await,
            // Systems Manager takes no snapshot of a patch baseline on the way out either:
            // there is no such call, so a `Snapshot` deletion policy on this type deletes the
            // baseline — and the registrations that hold it — with no snapshot invented for
            // it ([`ssm_patch_baseline::delete`]).
            Self::SsmPatchBaseline => ssm_patch_baseline::delete(ctx, region, resource).await,
            // Nor of a provisioning template or a role alias, for the same reason.
            Self::IotProvisioningResource(kind) => {
                iot_provisioning_resources::delete(ctx, region, kind, resource).await
            }
            // AWS IoT takes no snapshot of a topic rule on the way out either.
            Self::IotTopicRule => iot_topic_rule::delete(ctx, region, resource).await,
            // CloudFront takes no snapshot of a distribution or an origin access control on
            // the way out: there is no such call, so a `Snapshot` deletion policy on either
            // deletes the resource with no snapshot invented for it. A distribution is
            // DISABLED on the way out, which is what that service requires before it will
            // take one away ([`cloudfront_resources::delete`]).
            Self::CloudFrontResource(kind) => {
                // Boxed for the reason [`Self::create`]'s arm records.
                Box::pin(cloudfront_resources::delete(ctx, region, kind, resource)).await
            }
            // Amazon ECS takes no snapshot of a task-definition revision on the way out:
            // there is no such call, so a `Snapshot` deletion policy on one deregisters the
            // revision with no snapshot invented for it.
            Self::EcsTaskDefinition => ecs_task_definition::delete(ctx, region, resource).await,
            // Amazon EC2 takes no snapshot of any of these on the way out: there is no such
            // call for a route table, an association, an endpoint, a gateway, an attachment
            // or an address, so a `Snapshot` deletion policy on one of them deletes the
            // resource with no snapshot invented for it.
            Self::Ec2VpcNetwork(kind) => {
                ec2_vpc_network::delete(ctx, region, kind, resource).await
            }
            // Nor of an INSTANCE: `CreateImage` and `CreateSnapshot` are calls of their own
            // that this teardown does not make, so a `Snapshot` deletion policy on one
            // terminates the instance with no image or snapshot invented for it.
            Self::Ec2Instance => ec2_instance::delete(ctx, region, resource).await,
            // Amazon Cognito Identity takes no snapshot of a pool on the way out: there is no
            // such call, so a `Snapshot` deletion policy on any of these types deletes the
            // resource with no snapshot invented for it.
            Self::CognitoIdentityPool(kind) => {
                cognito_identity_pool::delete(ctx, region, kind, resource).await
            }
            // Amazon Cognito user pools takes no snapshot of a pool, an app client or an
            // identity provider on the way out: there is no such call, so a `Snapshot`
            // deletion policy on one of them deletes the resource with no snapshot invented
            // for it.
            Self::CognitoUserPool(kind) => {
                cognito_user_pool::delete(ctx, region, kind, resource).await
            }
            // CloudFormation takes no snapshot of a stack on the way out: there is no such
            // call, so a `Snapshot` deletion policy on this type deletes the nested stack —
            // and each resource of it under ITS own deletion policies — with no snapshot
            // invented for it. Boxed for the reason [`Self::create`]'s arm records.
            Self::NestedStack => Box::pin(nested_stack::delete(ctx, region, resource)).await,
            // Secrets Manager takes no snapshot of a rotation configuration on the way out:
            // there is no such call, so a `Snapshot` deletion policy on this type turns the
            // rotation off with no snapshot invented for it.
            Self::SecretsManagerRotationSchedule => {
                secretsmanager_rotation_schedule::delete(ctx, region, resource).await
            }
            // Nor of a resource policy: there is no such call, so a `Snapshot` deletion
            // policy on this type takes the document off with no snapshot invented for it.
            Self::SecretsManagerResourcePolicy => {
                secretsmanager_resource_policy::delete(ctx, region, resource).await
            }
            // Secrets Manager takes no snapshot of a secret on the way out: there is no such
            // call, so a `Snapshot` deletion policy on this type deletes the secret with no
            // snapshot invented for it.
            Self::SecretsManagerSecret => {
                secretsmanager_secret::delete(ctx, region, resource).await
            }
            // Step Functions takes no snapshot of a state machine on the way out: there is no
            // such call, so a `Snapshot` deletion policy on this type deletes the machine with
            // no snapshot invented for it.
            Self::StepFunctionsStateMachine => {
                step_functions_state_machine::delete(ctx, region, resource).await
            }
            // Step Functions takes no snapshot of a version or an alias on the way out either:
            // there is no such call, so a `Snapshot` deletion policy on one of these types
            // deletes the resource with no snapshot invented for it.
            Self::StepFunctionsVersionOrAlias(kind) => {
                step_functions_versions_and_aliases::delete(ctx, region, kind, resource).await
            }
            // API Gateway takes no snapshot of an api, a resource, a method, a deployment or a
            // stage on the way out: there is no such call, so a `Snapshot` deletion policy on
            // one of them deletes the resource with no snapshot invented for it.
            Self::ApiGatewayRestApi(kind) => {
                apigateway_rest_api::delete(ctx, region, kind, resource).await
            }
            // Application Auto Scaling takes no snapshot of a target or a policy on the way
            // out: there is no such call, so a `Snapshot` deletion policy on one of them
            // deletes the resource with no snapshot invented for it.
            Self::ApplicationAutoScaling(kind) => {
                application_auto_scaling::delete(ctx, region, kind, resource).await
            }
            // AppConfig takes no snapshot of an application, an environment, a profile or a
            // strategy on the way out: there is no such call, so a `Snapshot` deletion policy
            // on one of them deletes the resource with no snapshot invented for it.
            Self::AppConfigResource(kind) => {
                appconfig_resources::delete(ctx, region, kind, resource).await
            }
            // AppConfig takes no snapshot of a hosted configuration version or of a deployment on
            // the way out either: there is no such call, so a `Snapshot` deletion policy on one of
            // them removes the resource with no snapshot invented for it
            // ([`appconfig_rollout::delete`]).
            Self::AppConfigRollout(kind) => {
                appconfig_rollout::delete(ctx, region, kind, resource).await
            }
            // AppSync takes no snapshot of an API or of anything under one on the way out:
            // there is no such call, so a `Snapshot` deletion policy on one of them deletes the
            // resource with no snapshot invented for it.
            Self::AppSyncResource(kind) => {
                appsync_resources::delete(ctx, region, kind, resource).await
            }
            // Elastic Load Balancing takes no snapshot of a load balancer, a target group, a
            // listener or a rule on the way out: there is no such call, so a `Snapshot`
            // deletion policy on one of them deletes the resource with no snapshot invented
            // for it.
            Self::ElasticLoadBalancing(kind) => {
                elastic_load_balancing::delete(ctx, region, kind, resource).await
            }
            // AppSync takes no snapshot of a custom domain or of a mapping on the way out:
            // there is no such call, so a `Snapshot` deletion policy on either type deletes
            // the resource with no snapshot invented for it.
            Self::AppSyncDomainName(kind) => {
                appsync_domain_names::delete(ctx, region, kind, resource).await
            }
            // AppSync takes no snapshot of an Event API, a channel namespace or an association
            // on the way out: there is no such call, so a `Snapshot` deletion policy on one of
            // them deletes the resource with no snapshot invented for it.
            Self::AppSyncEventOrMergedApi(kind) => {
                appsync_event_and_merged_apis::delete(ctx, region, kind, resource).await
            }
            // Cloud Map takes no snapshot of a namespace, a service or an instance on the way
            // out: there is no such call, so a `Snapshot` deletion policy on one of them deletes
            // the resource with no snapshot invented for it.
            Self::ServiceDiscovery(kind) => {
                service_discovery::delete(ctx, region, kind, resource).await
            }
            // Athena takes no snapshot of a workgroup, a data catalog or a saved query on the
            // way out: there is no such call, so a `Snapshot` deletion policy on one of them
            // deletes the resource with no snapshot invented for it.
            Self::Athena(kind) => athena_resources::delete(ctx, region, kind, resource).await,
            // MediaConvert takes no snapshot of a queue, a job template or a preset on the way
            // out: there is no such call, so a `Snapshot` deletion policy on one of them deletes
            // the resource with no snapshot invented for it.
            Self::MediaConvert(kind) => {
                mediaconvert_resources::delete(ctx, region, kind, resource).await
            }
        }
    }

    /// The resource's configuration as its owning service reports it, for drift detection —
    /// see
    /// [`Provisioner::actual_configuration`](super::provisioner::Provisioner::actual_configuration).
    ///
    /// Every arm below that READS the owning service answers in that convention, where
    /// `None` is that service answering that it no longer holds the resource
    /// ([`ActualConfiguration::read`]). An arm for a type whose configuration no read here
    /// reports answers [`ActualConfiguration::Unchecked`] instead: nothing was asked of any
    /// service for such a resource, which says nothing about it — and is a different answer
    /// from the resource being gone.
    pub(crate) async fn actual_configuration(
        self,
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        resource: &StoredResource,
    ) -> Result<ActualConfiguration, Refusal> {
        let reported = match self {
            Self::Builtin(provisioner) => {
                // Boxed for the reason [`Self::create`]'s arm records.
                Box::pin(provisioner.actual_configuration(ctx, region, resource)).await?
            }
            Self::DynamodbTable => {
                dynamodb_table::actual_configuration(ctx, region, resource).await?
            }
            Self::DynamodbGlobalTable => {
                dynamodb_table::global_table::actual_configuration(ctx, region, resource).await?
            }
            // An embedded policy reports no configuration of its own to compare: the
            // document is read by the entity's own drift read, as a policy statement's is.
            Self::IamPolicy(_) => return Ok(ActualConfiguration::Unchecked),
            Self::LambdaAlias => {
                lambda_alias::actual_configuration(ctx, region, resource).await?
            }
            // A published version is immutable, so nothing of it can have drifted from its
            // declaration.
            Self::LambdaVersion => return Ok(ActualConfiguration::Unchecked),
            Self::LambdaEventSourceMapping => {
                lambda_event_source_mapping::actual_configuration(
                    ctx,
                    region,
                    &resource.physical_resource_id,
                )
                .await?
            }
            Self::LambdaEventInvokeConfig => {
                lambda_event_invoke_config::actual_configuration(ctx, region, resource).await?
            }
            Self::LambdaFunction => {
                lambda_function::actual_configuration(
                    ctx,
                    region,
                    &resource.physical_resource_id,
                )
                .await?
            }
            // A published layer version is immutable — every property of the type requires
            // the resource to be replaced, and Lambda publishes no call that edits one — so
            // nothing of it can have drifted from its declaration; and the policy a
            // statement belongs to is read by the function's own drift read, so a statement
            // reports no configuration of its own to compare either.
            Self::LambdaLayerVersion | Self::LambdaPermission => {
                return Ok(ActualConfiguration::Unchecked)
            }
            Self::LogsResource(kind) => {
                logs_resources::actual_configuration(ctx, region, kind, resource).await?
            }
            Self::IotAuthorizer => {
                iot_authorizer::actual_configuration(ctx, region, resource).await?
            }
            Self::SsmPatchBaseline => {
                ssm_patch_baseline::actual_configuration(ctx, region, resource).await?
            }
            Self::IotProvisioningResource(kind) => {
                iot_provisioning_resources::actual_configuration(ctx, region, kind, resource)
                    .await?
            }
            Self::IotTopicRule => {
                iot_topic_rule::actual_configuration(ctx, region, resource).await?
            }
            // Neither CloudFront type reports a configuration of its own to compare yet: this
            // arm reads nothing back for drift detection, so neither one's configuration is
            // guessed at.
            Self::CloudFrontResource(_) => return Ok(ActualConfiguration::Unchecked),
            // A task-definition revision is immutable — every property of the type requires
            // the resource to be replaced, and Amazon ECS publishes no call that edits one —
            // so nothing of it can have drifted from its declaration.
            Self::EcsTaskDefinition => return Ok(ActualConfiguration::Unchecked),
            // No read of either type's configuration is served here, so nothing is asked of
            // any service for one: the VPC-network types' every property is part of their
            // identity or of the call that made them, and an INSTANCE's were all set by the
            // launch, so neither has an arm that reports a configuration to compare.
            Self::Ec2VpcNetwork(_) | Self::Ec2Instance => {
                return Ok(ActualConfiguration::Unchecked)
            }
            Self::CognitoIdentityPool(kind) => {
                cognito_identity_pool::actual_configuration(ctx, region, kind, resource).await?
            }
            Self::CognitoUserPool(kind) => {
                cognito_user_pool::actual_configuration(ctx, region, kind, resource).await?
            }
            // A nested stack's own configuration is its TEMPLATE, and the drift of a nested
            // stack is detected by running a detection against that stack itself ("perform
            // DetectStackDrift directly on the nested stack"), so this arm reads nothing
            // back for the parent's resource rather than guessing at it.
            Self::NestedStack => return Ok(ActualConfiguration::Unchecked),
            Self::SecretsManagerRotationSchedule => {
                secretsmanager_rotation_schedule::actual_configuration(ctx, region, resource)
                    .await?
            }
            Self::SecretsManagerResourcePolicy => {
                secretsmanager_resource_policy::actual_configuration(ctx, region, resource)
                    .await?
            }
            Self::SecretsManagerSecret => {
                secretsmanager_secret::actual_configuration(ctx, region, resource).await?
            }
            Self::StepFunctionsStateMachine => {
                step_functions_state_machine::actual_configuration(
                    ctx,
                    region,
                    &resource.physical_resource_id,
                )
                .await?
            }
            Self::StepFunctionsVersionOrAlias(kind) => match kind {
                // A published version is the snapshot its publish took, and Step Functions
                // publishes no call that edits one, so nothing of it can have drifted from its
                // declaration.
                step_functions_versions_and_aliases::Kind::Version => {
                    return Ok(ActualConfiguration::Unchecked)
                }
                step_functions_versions_and_aliases::Kind::Alias => {
                    step_functions_versions_and_aliases::alias_configuration(
                        ctx, region, resource,
                    )
                    .await?
                }
            },
            Self::ApiGatewayRestApi(kind) => {
                apigateway_rest_api::actual_configuration(ctx, region, kind, resource).await?
            }
            // A target and a policy report no configuration of their own to compare yet: this
            // arm reads nothing back for drift detection, so neither one's configuration is
            // guessed at.
            Self::ApplicationAutoScaling(_) => return Ok(ActualConfiguration::Unchecked),
            Self::AppConfigResource(kind) => {
                appconfig_resources::actual_configuration(ctx, region, kind, resource).await?
            }
            // A hosted configuration version and a deployment are each immutable — every property
            // of both types requires the resource to be replaced, and AppConfig publishes no call
            // that edits one — so nothing of either can have drifted from its declaration.
            Self::AppConfigRollout(_) => return Ok(ActualConfiguration::Unchecked),
            Self::AppSyncResource(kind) => {
                appsync_resources::actual_configuration(ctx, region, kind, resource).await?
            }
            Self::ElasticLoadBalancing(kind) => {
                elastic_load_balancing::actual_configuration(ctx, region, kind, resource).await?
            }
            Self::AppSyncDomainName(kind) => {
                appsync_domain_names::actual_configuration(ctx, region, kind, resource).await?
            }
            Self::AppSyncEventOrMergedApi(kind) => {
                appsync_event_and_merged_apis::actual_configuration(ctx, region, kind, resource)
                    .await?
            }
            Self::ServiceDiscovery(kind) => {
                service_discovery::actual_configuration(ctx, region, kind, resource).await?
            }
            // No Athena type here reports a configuration of its own to compare yet. Athena
            // answers one error code for every refusal its reads make, so a read could not tell
            // the resource being GONE from any other refusal of it, and reporting a resource its
            // owning service still holds as deleted would report drift that is not there: this
            // arm reads nothing back, so no one's configuration is guessed at.
            Self::Athena(_) => return Ok(ActualConfiguration::Unchecked),
            // No MediaConvert type here reports a configuration of its own to compare yet: the
            // settings document a job template and a preset are declared with is normalised by
            // the owning service as it stores it — every member of the graph it did not state
            // reported with the value that service applies — so comparing a read of it against
            // the declaration would report drift that is not there. This arm reads nothing back,
            // so no one's configuration is guessed at.
            Self::MediaConvert(_) => return Ok(ActualConfiguration::Unchecked),
        };
        Ok(ActualConfiguration::read(reported))
    }

    /// How this type's provider words a refusal it classified — see
    /// [`Provisioner::handler_message`](super::provisioner::Provisioner::handler_message),
    /// which `token` is the resource request's own token for.
    pub(super) fn handler_message(self, class: &str, message: &str, token: &str) -> String {
        match self {
            Self::Builtin(provisioner) => provisioner.handler_message(class, message, token),
            // AWS IAM's resource provider reports the service's own message as it came.
            Self::IamPolicy(_) => message.to_string(),
            // The mapping's provider is written against the CloudFormation
            // resource-provider plugin and words a refusal through the exception class it put
            // it in, so an invalid-request refusal of AWS Lambda's reaches the reason as
            // `Invalid request provided: <the service's message>` — as the recorded
            // `ResourceStatusReason` of an `AWS::Lambda::EventSourceMapping` whose function's
            // execution role may not read the queue it polls carries it (in the behavioral
            // corpus `simparity-behavioral-aws-v1`, check
            // `cloudformation__lambda__sqs__ssm.sam-sqs-event-role-without-queue-access-fails-the-mapping-create`,
            // whose reason reads `Resource handler returned message: "Invalid request
            // provided: <the service's sentence> (Service: Lambda, Status Code: 400, Request
            // ID: …) (SDK Attempt Count: …)" (RequestToken: …, HandlerErrorCode:
            // InvalidRequest)`). Every refusal of this type that falls in that class is
            // worded this way, whichever call made it, and a refusal of any other class is
            // reported as it came.
            Self::LambdaEventSourceMapping
                if class == super::HANDLER_ERROR_INVALID_REQUEST =>
            {
                format!(
                    "{}{message}",
                    crate::state::provisioner::INVALID_REQUEST_PROVIDED,
                )
            }
            // AWS Lambda's other resource providers report the service's own message as it
            // came: each type's wording is its provider's own and cannot be derived from the
            // service they all call, so nothing is added for a type no recorded reason shows
            // the plugin's wording on.
            Self::LambdaAlias
            | Self::LambdaEventInvokeConfig
            | Self::LambdaEventSourceMapping
            | Self::LambdaFunction
            | Self::LambdaLayerVersion
            | Self::LambdaPermission
            | Self::LambdaVersion => message.to_string(),
            // Amazon DynamoDB's does the same, for the table and the global table alike.
            Self::DynamodbTable | Self::DynamodbGlobalTable => message.to_string(),
            // AWS IoT's does the same. Amazon CloudWatch Logs's providers word the plugin's
            // invalid-request exception around the SDK exception they wrapped, and that
            // wording is applied where the refusal is READ — at the call, which is where the
            // type whose provider made it is known ([`logs_resources::provider_exception`]) —
            // so nothing of it is added a second time here.
            Self::LogsResource(_) | Self::IotAuthorizer | Self::IotTopicRule => {
                message.to_string()
            }
            // The two other AWS IoT providers report that service's own message as it came
            // too.
            Self::IotProvisioningResource(_) => message.to_string(),
            // Amazon CloudFront's does the same.
            Self::CloudFrontResource(_) => message.to_string(),
            // Amazon ECS's does the same.
            Self::EcsTaskDefinition => message.to_string(),
            // Amazon EC2's does the same.
            Self::Ec2VpcNetwork(_) | Self::Ec2Instance => message.to_string(),
            // Amazon Cognito Identity's and Amazon Cognito user pools' do the same.
            Self::CognitoIdentityPool(_) | Self::CognitoUserPool(_) => message.to_string(),
            // A nested stack is provisioned by CloudFormation itself rather than by a provider
            // of another service ([`Self::reported_by_a_resource_provider`]), so there is no
            // provider wording to add: what CloudFormation answered about the child is the
            // account of why the stack was not created, and it stands as it came.
            Self::NestedStack => message.to_string(),
            // AWS Secrets Manager's and AWS Step Functions' do the same.
            Self::SecretsManagerRotationSchedule
            | Self::SecretsManagerResourcePolicy
            | Self::SecretsManagerSecret
            | Self::StepFunctionsStateMachine
            | Self::StepFunctionsVersionOrAlias(_) => message.to_string(),
            // Amazon API Gateway's does the same.
            Self::ApiGatewayRestApi(_) => message.to_string(),
            // Application Auto Scaling's does the same.
            Self::ApplicationAutoScaling(_) => message.to_string(),
            // AWS AppConfig's does the same.
            Self::AppConfigResource(_) => message.to_string(),
            // And so does its rollout provider.
            Self::AppConfigRollout(_) => message.to_string(),
            // AWS AppSync's does the same.
            Self::AppSyncResource(_) => message.to_string(),
            // Elastic Load Balancing's does the same.
            Self::ElasticLoadBalancing(_) => message.to_string(),
            // AppSync's does the same.
            Self::AppSyncDomainName(_) => message.to_string(),
            // And so does its Event API and merged-API provider.
            Self::AppSyncEventOrMergedApi(_) => message.to_string(),
            // And so does AWS Cloud Map's.
            Self::ServiceDiscovery(_) => message.to_string(),
            // And so does Amazon Athena's.
            Self::Athena(_) => message.to_string(),
            // And so does AWS Elemental MediaConvert's.
            Self::MediaConvert(_) => message.to_string(),
            // And so does AWS Systems Manager's patch-baseline provider.
            Self::SsmPatchBaseline => message.to_string(),
        }
    }

    /// Whether a failure of a resource of this type is a RESOURCE PROVIDER's answer — the
    /// registry extension of the service that owns the type, which CloudFormation asks to
    /// create, update and delete it and whose refusal it then quotes
    /// ([`super::handler_failure_reason`]).
    ///
    /// A type CloudFormation deploys ITSELF is provisioned by no such provider: a nested stack
    /// is created by CloudFormation's own `CreateStack` ([`nested_stack`]), so what fails its
    /// resource is CloudFormation's own account of why the stack was not created and is
    /// reported in those words ([`super::own_failure_reason`]) rather than attributed to a
    /// provider that answered nothing.
    pub(super) fn reported_by_a_resource_provider(self) -> bool {
        !matches!(self, Self::NestedStack)
    }

    /// Which `HandlerErrorCode` class this type's provider puts a refusal carrying `code`
    /// in — see
    /// [`Provisioner::handler_error_class`](super::provisioner::Provisioner::handler_error_class).
    pub(super) fn handler_error_class(self, code: &str, classified: &'static str) -> &'static str {
        match self {
            Self::Builtin(provisioner) => provisioner.handler_error_class(code, classified),
            // A rule's PRIORITY is where its listener evaluates it — "a listener can't have
            // multiple rules with the same priority" — so a priority that listener already
            // holds a rule at is a rule that ALREADY EXISTS at the place the declaration asks
            // for, which is the class this provider puts that refusal in rather than the
            // in-use class the code's own wording reads as. The distinction is what a client
            // branches on: an already-exists failure is one no retry of the same template can
            // clear.
            Self::ElasticLoadBalancing(_) if code.contains(PRIORITY_IN_USE) => {
                super::HANDLER_ERROR_ALREADY_EXISTS
            }
            // Every other provider here reports the class the error code itself falls in.
            _ => classified,
        }
    }
}

/// The error code Elastic Load Balancing refuses a rule asking for a priority its listener
/// already evaluates a rule at with: the `awsQueryError` code of `CreateRule`'s own
/// `PriorityInUseException`
/// (`smithy_query(query="operation",operation="CreateRule")`: `awsQueryError {code:
/// 'PriorityInUse', httpResponseCode: 400}`), which `SetRulePriorities` refuses the same
/// conflict with.
const PRIORITY_IN_USE: &str = "PriorityInUse";
