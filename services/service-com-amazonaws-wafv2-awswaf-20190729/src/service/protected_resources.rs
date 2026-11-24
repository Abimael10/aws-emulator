// SPDX-License-Identifier: CC0-1.0

//! The PROTECTED RESOURCE a web ACL is put in front of, resolved with the service that
//! owns it.
//!
//! # The rule
//!
//! A protected resource is never WAFv2's own: it is an Application Load Balancer, an
//! API Gateway stage, an AppSync GraphQL API, an Amazon Cognito user pool, an App
//! Runner service, a Verified Access instance, an Amplify application or a Bedrock
//! AgentCore gateway — the eight forms `AssociateWebACLRequest$ResourceArn` documents.
//! WAF has to RETRIEVE one before it can act on it, and says so in the error every
//! association operation models for the case it cannot: "WAF couldn't retrieve a
//! resource that you specified for this operation"
//! (`WAFUnavailableEntityException`), with `WAFNonexistentItemException` for the reads
//! addressed by a resource ARN. So the resource's existence is the OWNING service's
//! answer and nothing else: a binding written against an ARN nothing holds would
//! report a protection that does not exist.
//!
//! # How the question is asked
//!
//! Each owning service is asked for the resource by its own read — the one operation
//! that resolves that resource kind — over the AWS SDK driven on the emulator's own
//! in-process transport, through the operation callback
//! ([`smithy_server_runtime::OperationContext::callback`]): no socket is opened, no
//! name is resolved, nothing leaves this process, and this crate names no other
//! service's crate, which is also why asking cannot form a dependency cycle with the
//! services that ask WAFv2 for a decision ([`crate::service::edge`]). The credentials
//! are the callback's, which sign that in-process hop and never appear in a response.
//!
//! The Region asked in is the one the resource's OWN ARN carries, which is where that
//! resource lives, read as a Region LABEL ([`::service_common::arn::region_label`])
//! because it ends up in an endpoint URL's authority.
//!
//! # What an unanswered question means
//!
//! Three outcomes, not two ([`Retrieved`]). A resource is ABSENT only when the service
//! that owns it says it holds none — that is the answer WAF refuses on. A hop that is
//! not answered AT ALL says nothing about the resource: an emulator in which the owning
//! service is not mounted has no one to ask, so WAF cannot report a resource missing
//! that nothing was asked about, and the call proceeds as the caller asked. Nothing is
//! invented either way: the only claim this module makes is one an owning service made.

use ::service_common::arn::region_label;
/// What the service that owns a protected resource answered about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retrieved {
    /// The owning service answered with the resource, so WAF can act on it.
    Held,
    /// The owning service answered that it holds no such resource.
    Absent,
    /// Nothing answered about the resource: the owning service is not part of this
    /// emulator, the ARN carries no Region to ask in, or the hop failed.
    Unknown,
}

impl Retrieved {
    /// `true` when WAF must refuse the call because the resource is not there.
    ///
    /// ONLY [`Self::Absent`] — the one outcome an owning service actually reported.
    pub fn is_absent(self) -> bool {
        self == Self::Absent
    }
}

/// An AWS SDK client for one owning service, over the in-process callback into this
/// same emulator.
///
/// `$sdk` is that service's generated client crate, `$host` the endpoint prefix its
/// endpoints are addressed by. The endpoint URL is only the address the callback's
/// transport routes on IN PROCESS. The transport being in process is also why there is
/// nothing to retry, time out or watch for a stalled stream — and each of those would
/// need a sleep implementation this client is not given. The same goes for caching the
/// identity: the credentials the callback hands over are used directly.
macro_rules! in_process_client {
    ($sdk:ident, $host:expr, $ctx:expr, $region:expr) => {{
        let config = ::$sdk::Config::builder()
            .behavior_version(::$sdk::config::BehaviorVersion::latest())
            .http_client($ctx.callback().http_client())
            .credentials_provider($ctx.callback().aws_credentials_provider())
            .region(::$sdk::config::Region::new($region.to_string()))
            .endpoint_url(format!("https://{}.{}.amazonaws.com", $host, $region))
            .retry_config(::$sdk::config::retry::RetryConfig::disabled())
            .timeout_config(::$sdk::config::timeout::TimeoutConfig::disabled())
            .identity_cache(::$sdk::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                ::$sdk::config::StalledStreamProtectionConfig::disabled(),
            )
            .build();
        ::$sdk::Client::from_conf(config)
    }};
}

/// What the service that owns the resource `resource_arn` names answers about it.
///
/// [`Retrieved::Unknown`] for an ARN that names no protected resource form at all —
/// such a value is the malformed-ARN parameter refusal every association operation
/// makes about it BEFORE it asks anyone (`crate::state::protected_resource_type`), so
/// there is nothing to retrieve.
///
/// Call it with NO state guard held: it awaits.
pub async fn retrieved(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    resource_arn: &str,
) -> Retrieved {
    let Some(kind) = crate::state::protected_resource_type(resource_arn) else {
        return Retrieved::Unknown;
    };
    let Some((region, segments)) = addressed(resource_arn) else {
        ::tracing::debug!(
            resource_type = kind,
            "the protected resource's ARN names no Region to ask its owner in"
        );
        return Retrieved::Unknown;
    };
    // One arm per resource form, each asking the ONE read of the service that owns that
    // form. The segment layout each arm reads is the one
    // `crate::state::PROTECTED_RESOURCE_FORMS` matched, so an arm only ever sees the
    // segments its own form has.
    let retrieved = match (kind, segments.as_slice()) {
        ("APPLICATION_LOAD_BALANCER", _) => load_balancer(ctx, region, resource_arn).await,
        ("API_GATEWAY", [_, rest_api_id, _, stage_name]) => {
            api_gateway_stage(ctx, region, rest_api_id, stage_name).await
        }
        ("APPSYNC", [_, api_id]) => graphql_api(ctx, region, api_id).await,
        ("COGNITO_USER_POOL", [_, user_pool_id]) => user_pool(ctx, region, user_pool_id).await,
        ("APP_RUNNER_SERVICE", _) => app_runner_service(ctx, region, resource_arn).await,
        ("VERIFIED_ACCESS_INSTANCE", [_, instance_id]) => {
            verified_access_instance(ctx, region, instance_id).await
        }
        ("AMPLIFY", [_, app_id]) => amplify_app(ctx, region, app_id).await,
        ("AGENTCORE_GATEWAY", [_, gateway_id]) => agentcore_gateway(ctx, region, gateway_id).await,
        // A form the table matched but no arm above reads: nothing was asked, so
        // nothing is known.
        _ => Retrieved::Unknown,
    };
    ::tracing::debug!(
        resource_type = kind,
        retrieved = ?retrieved,
        "the service that owns the protected resource was asked about it"
    );
    retrieved
}

/// The Region `resource_arn` lives in and the slash-separated segments of its resource
/// part, or `None` when it is no well-formed ARN or carries no Region label to ask an
/// endpoint in.
///
/// The fields come from the ONE parse this tree shares
/// ([`::service_common::arn::parse`]); the leading slash an API Gateway stage's resource
/// part opens with is dropped, so the segments line up with the forms
/// `crate::state::PROTECTED_RESOURCE_FORMS` matches.
fn addressed(resource_arn: &str) -> Option<(&str, Vec<&str>)> {
    let arn = ::service_common::arn::parse(resource_arn)?;
    let region = region_label(arn.region)?;
    Some((
        region,
        arn.resource.trim_start_matches('/').split('/').collect(),
    ))
}

/// Whether Elastic Load Balancing holds the load balancer `load_balancer_arn`.
///
/// `DescribeLoadBalancers` takes the ARN itself and answers
/// `LoadBalancerNotFoundException` for one it holds none of — "the specified load
/// balancer does not exist" — which is the ABSENT answer; a selection that comes back
/// empty says the same thing.
async fn load_balancer(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    load_balancer_arn: &str,
) -> Retrieved {
    let client = in_process_client!(
        aws_sdk_elasticloadbalancingv2,
        "elasticloadbalancing",
        ctx,
        region
    );
    match client
        .describe_load_balancers()
        .load_balancer_arns(load_balancer_arn)
        .send()
        .await
    {
        Ok(described) => reported(!described.load_balancers().is_empty()),
        Err(error) => match error.as_service_error() {
            Some(::aws_sdk_elasticloadbalancingv2::operation::describe_load_balancers::DescribeLoadBalancersError::LoadBalancerNotFoundException(_)) => Retrieved::Absent,
            _ => unanswered(&error),
        },
    }
}

/// Whether API Gateway holds the stage `stage_name` of the REST API `rest_api_id`.
///
/// `GetStage` answers `NotFoundException` for a stage it holds none of, which is the
/// ABSENT answer.
async fn api_gateway_stage(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    rest_api_id: &str,
    stage_name: &str,
) -> Retrieved {
    let client = in_process_client!(aws_sdk_apigateway, "apigateway", ctx, region);
    match client
        .get_stage()
        .rest_api_id(rest_api_id)
        .stage_name(stage_name)
        .send()
        .await
    {
        Ok(_) => Retrieved::Held,
        Err(error) => match error.as_service_error() {
            Some(::aws_sdk_apigateway::operation::get_stage::GetStageError::NotFoundException(
                _,
            )) => Retrieved::Absent,
            _ => unanswered(&error),
        },
    }
}

/// Whether AppSync holds the GraphQL API `api_id`. `GetGraphqlApi` answers
/// `NotFoundException` for one it holds none of.
async fn graphql_api(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    api_id: &str,
) -> Retrieved {
    let client = in_process_client!(aws_sdk_appsync, "appsync", ctx, region);
    match client.get_graphql_api().api_id(api_id).send().await {
        Ok(_) => Retrieved::Held,
        Err(error) => match error.as_service_error() {
            Some(
                ::aws_sdk_appsync::operation::get_graphql_api::GetGraphqlApiError::NotFoundException(
                    _,
                ),
            ) => Retrieved::Absent,
            _ => unanswered(&error),
        },
    }
}

/// Whether Amazon Cognito holds the user pool `user_pool_id`. `DescribeUserPool`
/// answers `ResourceNotFoundException` for one it holds none of.
async fn user_pool(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    user_pool_id: &str,
) -> Retrieved {
    let client = in_process_client!(aws_sdk_cognitoidentityprovider, "cognito-idp", ctx, region);
    match client
        .describe_user_pool()
        .user_pool_id(user_pool_id)
        .send()
        .await
    {
        Ok(_) => Retrieved::Held,
        Err(error) => match error.as_service_error() {
            Some(::aws_sdk_cognitoidentityprovider::operation::describe_user_pool::DescribeUserPoolError::ResourceNotFoundException(_)) => Retrieved::Absent,
            _ => unanswered(&error),
        },
    }
}

/// Whether App Runner holds the service `service_arn`. `DescribeService` takes the ARN
/// itself and answers `ResourceNotFoundException` for one it holds none of.
async fn app_runner_service(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    service_arn: &str,
) -> Retrieved {
    let client = in_process_client!(aws_sdk_apprunner, "apprunner", ctx, region);
    match client.describe_service().service_arn(service_arn).send().await {
        Ok(_) => Retrieved::Held,
        Err(error) => match error.as_service_error() {
            Some(::aws_sdk_apprunner::operation::describe_service::DescribeServiceError::ResourceNotFoundException(_)) => Retrieved::Absent,
            _ => unanswered(&error),
        },
    }
}

/// Whether Amazon EC2 holds the Verified Access instance `instance_id`.
///
/// `DescribeVerifiedAccessInstances` reports the instances of a selection, so an empty
/// answer is the ABSENT one: the account holds no instance of that identifier.
async fn verified_access_instance(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    instance_id: &str,
) -> Retrieved {
    let client = in_process_client!(aws_sdk_ec2, "ec2", ctx, region);
    match client
        .describe_verified_access_instances()
        .verified_access_instance_ids(instance_id)
        .send()
        .await
    {
        Ok(described) => reported(!described.verified_access_instances().is_empty()),
        Err(error) => unanswered(&error),
    }
}

/// Whether Amplify holds the application `app_id`. `GetApp` answers `NotFoundException`
/// for one it holds none of.
async fn amplify_app(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    app_id: &str,
) -> Retrieved {
    let client = in_process_client!(aws_sdk_amplify, "amplify", ctx, region);
    match client.get_app().app_id(app_id).send().await {
        Ok(_) => Retrieved::Held,
        Err(error) => match error.as_service_error() {
            Some(::aws_sdk_amplify::operation::get_app::GetAppError::NotFoundException(_)) => {
                Retrieved::Absent
            }
            _ => unanswered(&error),
        },
    }
}

/// Whether Bedrock AgentCore holds the gateway `gateway_id`. `GetGateway` answers
/// `ResourceNotFoundException` for one it holds none of.
async fn agentcore_gateway(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    gateway_id: &str,
) -> Retrieved {
    let client = in_process_client!(
        aws_sdk_bedrockagentcorecontrol,
        "bedrock-agentcore-control",
        ctx,
        region
    );
    match client.get_gateway().gateway_identifier(gateway_id).send().await {
        Ok(_) => Retrieved::Held,
        Err(error) => match error.as_service_error() {
            Some(::aws_sdk_bedrockagentcorecontrol::operation::get_gateway::GetGatewayError::ResourceNotFoundException(_)) => Retrieved::Absent,
            _ => unanswered(&error),
        },
    }
}

/// The outcome for a read that REPORTS the resources of a selection: `held` says
/// whether the selection came back with any.
fn reported(held: bool) -> Retrieved {
    if held {
        Retrieved::Held
    } else {
        Retrieved::Absent
    }
}

/// [`Retrieved::Unknown`] for a hop that was not answered, with the reason traced.
///
/// At `debug`: an emulator that does not mount the owning service answers every such
/// hop this way, which is a property of the deployment rather than of the request.
fn unanswered(error: &impl ::std::error::Error) -> Retrieved {
    ::tracing::debug!(
        error = %::aws_smithy_types::error::display::DisplayErrorContext(error),
        "the service that owns the protected resource did not answer about it"
    );
    Retrieved::Unknown
}
