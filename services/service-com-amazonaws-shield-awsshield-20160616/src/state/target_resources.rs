// SPDX-License-Identifier: CC0-1.0

//! Whether the resource a protection is asked for EXISTS — a fact the service that OWNS
//! that resource holds, and this one does not.
//!
//! `CreateProtection` "enables Shield Advanced for a specific Amazon Web Services
//! resource. The resource can be an Amazon CloudFront distribution, Amazon Route 53 hosted
//! zone, Global Accelerator standard accelerator, Elastic IP Address, Application Load
//! Balancer, or a Classic Load Balancer"
//! (`smithy_query(query="operation",operation="CreateProtection")`), and
//! `CreateProtectionRequest$ResourceArn` documents the ARN layout of each of those six
//! classes. Protecting a resource therefore starts by RESOLVING it: Shield holds no
//! distribution, zone, accelerator, address or load balancer of its own, so the only thing
//! that can say whether an ARN names one is the service that owns it, asked through the
//! in-process callback the framework hands a handler
//! ([`smithy_server_runtime::OperationContext::callback`]) — no socket is opened and
//! nothing outside this process is reachable from it.
//!
//! That resolution is what the modelled `InvalidResourceException` (HTTP 400, "the
//! resource is invalid. You might not have access to the resource, or the resource might
//! not exist") answers for, and it is judged BEFORE anything about the account, so a caller
//! that sent a stale or wrong ARN is told about its ARN rather than about a subscription.
//!
//! Only an owning service that REPORTS the resource absent refuses the request
//! ([`PROTECTABLE_CLASSES`] names each class, and the doc of that constant names the read
//! each one is resolved by). Everything else establishes nothing and refuses nothing: an
//! ARN in no documented layout, a Region field no endpoint can be reached at, an owning
//! service that answered something else, and an emulator assembled without that service to
//! ask at all — none of those is evidence that a resource is missing, and none of them is
//! invented into one.
//!
//! Nothing here touches this service's own store, and what it asks another service for is
//! a READ: a refused `CreateProtection` leaves every service exactly as it was.

use ::aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use ::aws_smithy_runtime_api::client::result::SdkError;
use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;
use ::smithy_server_runtime::OperationContext;

/// The header every answer of this emulator names the request it answered in — the id a
/// called service's SDK reports for that call where the answer carries no identifier of its
/// own in the body.
const REQUEST_ID_HEADER: &str = "x-amz-request-id";

/// The metadata key an AWS SDK records the called service's request id under, whichever
/// place that service's protocol carries it in (a header, or an element of its error
/// envelope).
const REQUEST_ID_METADATA: &str = "aws_request_id";

/// The proxy an SDK states for a call it made with none configured — which every call over
/// the in-process callback is.
const DIRECT_CALL_PROXY: &str = "null";

/// The Region the partition-global services here are reached in: Amazon CloudFront and
/// Amazon Route 53 each hold one set of resources whichever Region they are asked in, and
/// their endpoints are signed for this one.
const GLOBAL_SIGNING_REGION: &str = "us-east-1";

/// The Region AWS Global Accelerator's control plane is reached in — that service's single
/// control-plane endpoint, which is not the one above.
const GLOBAL_ACCELERATOR_SIGNING_REGION: &str = "us-west-2";

/// The `ProtectedResourceType` of each class, as [`super::protected_resource_type`] reports
/// it for an ARN in that class's documented layout. Members of the modelled
/// `ProtectedResourceType` catalog
/// ([`crate::ops::types::PROTECTED_RESOURCE_TYPE_VALUES`]), which the tests below hold
/// every row of this module's table to.
const ELASTIC_IP_ALLOCATION: &str = "ELASTIC_IP_ALLOCATION";
const APPLICATION_LOAD_BALANCER: &str = "APPLICATION_LOAD_BALANCER";
const CLASSIC_LOAD_BALANCER: &str = "CLASSIC_LOAD_BALANCER";
const CLOUDFRONT_DISTRIBUTION: &str = "CLOUDFRONT_DISTRIBUTION";
const GLOBAL_ACCELERATOR: &str = "GLOBAL_ACCELERATOR";
const ROUTE_53_HOSTED_ZONE: &str = "ROUTE_53_HOSTED_ZONE";

/// One protectable resource class: how Shield's refusal names it, and the name of the SDK
/// client its validation is made through.
#[derive(Debug, Clone, Copy)]
struct ProtectableClass {
    /// The `ProtectedResourceType` of the class.
    resource_type: &'static str,
    /// How the refusal names the class being validated.
    label: &'static str,
    /// The SDK client the owning service is read through, as that client names itself.
    sdk_service: &'static str,
}

/// Every protectable resource class, with the read that resolves one and the absence that
/// read answers for a resource the owning service does not hold:
///
/// * `ELASTIC_IP_ALLOCATION` — Amazon EC2 `DescribeAddresses` over the allocation id,
///   refused with `InvalidAllocationID.NotFound` (and `InvalidAllocationID.Malformed` for
///   a value that is not an allocation id at all);
/// * `APPLICATION_LOAD_BALANCER` — Elastic Load Balancing v2 `DescribeLoadBalancers` over
///   the ARN, refused with `LoadBalancerNotFound`;
/// * `CLASSIC_LOAD_BALANCER` — Elastic Load Balancing `DescribeLoadBalancers` over the
///   load balancer's name, refused with `LoadBalancerNotFound`
///   (`AccessPointNotFoundException`);
/// * `CLOUDFRONT_DISTRIBUTION` — Amazon CloudFront `GetDistribution`, refused with
///   `NoSuchDistribution`;
/// * `GLOBAL_ACCELERATOR` — AWS Global Accelerator `DescribeAccelerator`, refused with
///   `AcceleratorNotFoundException`;
/// * `ROUTE_53_HOSTED_ZONE` — Amazon Route 53 `GetHostedZone`, refused with
///   `NoSuchHostedZone`.
///
/// The `label` and `sdk_service` of the Elastic IP class are the RECORDED ones (see
/// [`failed_to_validate_message`]); the other five name their class the way the service
/// that owns it is named, and carry the simple name of that service's SDK client — which is
/// what `AmazonEC2` is for the recorded one.
const PROTECTABLE_CLASSES: &[ProtectableClass] = &[
    ProtectableClass {
        resource_type: ELASTIC_IP_ALLOCATION,
        label: "EIP",
        sdk_service: "AmazonEC2",
    },
    ProtectableClass {
        resource_type: APPLICATION_LOAD_BALANCER,
        label: "ELB",
        sdk_service: "AmazonElasticLoadBalancing",
    },
    ProtectableClass {
        resource_type: CLASSIC_LOAD_BALANCER,
        label: "ELB",
        sdk_service: "AmazonElasticLoadBalancing",
    },
    ProtectableClass {
        resource_type: CLOUDFRONT_DISTRIBUTION,
        label: "CloudFront",
        sdk_service: "AmazonCloudFront",
    },
    ProtectableClass {
        resource_type: GLOBAL_ACCELERATOR,
        label: "GlobalAccelerator",
        sdk_service: "AWSGlobalAccelerator",
    },
    ProtectableClass {
        resource_type: ROUTE_53_HOSTED_ZONE,
        label: "Route53",
        sdk_service: "AmazonRoute53",
    },
];

/// The class `resource_type` names, or `None` for a `ProtectedResourceType` no read here
/// resolves.
fn protectable_class(resource_type: &str) -> Option<&'static ProtectableClass> {
    PROTECTABLE_CLASSES
        .iter()
        .find(|class| class.resource_type == resource_type)
}

/// What an owning service answered about a resource it holds nothing under: the status,
/// the error code and the sentence it worded, plus the id of that internal call.
#[derive(Debug, Clone)]
struct OwningServiceRefusal {
    status: u16,
    code: String,
    message: String,
    request_id: String,
}

/// How Shield words the failure to validate the resource a request named, as AWS words it:
/// which class of resource was being validated, the ARN the REQUEST carried, the Region
/// that ARN names, and the answer the owning service gave, rendered the way the SDK client
/// the validation is made through raises it.
///
/// Read from the recorded refusal of a `CreateProtection` against an Elastic IP allocation
/// that does not exist, whose message is `Failed to validate EIP resource: <arn> in
/// region: <region>, error: The allocation ID '<id>' does not exist (Service: AmazonEC2;
/// Status Code: 400; Error Code: InvalidAllocationID.NotFound; Request ID: <id>; Proxy:
/// null)` — fixed text apart from the ARN and the Region the request carried, the sentence
/// Amazon EC2 answered, and the per-call values of that internal call.
fn failed_to_validate_message(
    class: &ProtectableClass,
    resource_arn: &str,
    region: &str,
    refusal: &OwningServiceRefusal,
) -> String {
    format!(
        "Failed to validate {} resource: {resource_arn} in region: {region}, error: {} \
         (Service: {}; Status Code: {}; Error Code: {}; Request ID: {}; \
         Proxy: {DIRECT_CALL_PROXY})",
        class.label,
        refusal.message,
        class.sdk_service,
        refusal.status,
        refusal.code,
        refusal.request_id,
    )
}

/// Why `resource_arn` names NO resource a protection can cover, as the modelled
/// `InvalidResourceException` states it — and `None` when nothing of the kind was
/// established (see the module docs), in which case the request is not refused for its
/// resource at all.
pub async fn unresolvable_protected_resource(
    ctx: &OperationContext<'_>,
    resource_arn: &str,
) -> Option<String> {
    let class = protectable_class(super::protected_resource_type(resource_arn)?)?;
    // The Region the ARN names, which is the one the refusal states: the field the request
    // carried, empty for a partition-global resource.
    let stated_region = ::service_common::arn::parse(resource_arn)?.region;
    let refusal = reported_absence(ctx, class, resource_arn, stated_region).await?;
    Some(failed_to_validate_message(
        class,
        resource_arn,
        stated_region,
        &refusal,
    ))
}

/// The owning service's answer that it holds no such resource, or `None` for every other
/// answer.
///
/// `stated_region` is the ARN's own Region field; a regional class is resolved in that
/// Region, and only when the field is a Region LABEL rather than arbitrary text
/// ([`service_common::arn::region_label`]) — a value carrying `/`, `@`, `:`, `?` or `#`
/// would re-point or truncate the client's authority, so such an ARN resolves nothing
/// rather than reaching a host a caller chose.
async fn reported_absence(
    ctx: &OperationContext<'_>,
    class: &ProtectableClass,
    resource_arn: &str,
    stated_region: &str,
) -> Option<OwningServiceRefusal> {
    let resource = ::service_common::arn::resource(resource_arn)?;
    match class.resource_type {
        ELASTIC_IP_ALLOCATION => {
            let region = ::service_common::arn::region_label(stated_region)?;
            absent_elastic_ip(ctx, region, named_resource(resource, "eip-allocation/")?).await
        }
        APPLICATION_LOAD_BALANCER => {
            let region = ::service_common::arn::region_label(stated_region)?;
            absent_application_load_balancer(ctx, region, resource_arn).await
        }
        CLASSIC_LOAD_BALANCER => {
            let region = ::service_common::arn::region_label(stated_region)?;
            absent_classic_load_balancer(ctx, region, named_resource(resource, "loadbalancer/")?)
                .await
        }
        CLOUDFRONT_DISTRIBUTION => {
            absent_distribution(ctx, named_resource(resource, "distribution/")?).await
        }
        GLOBAL_ACCELERATOR => absent_accelerator(ctx, resource_arn).await,
        ROUTE_53_HOSTED_ZONE => {
            absent_hosted_zone(ctx, named_resource(resource, "hostedzone/")?).await
        }
        _ => None,
    }
}

/// The identifier after `prefix` in an ARN's resource field, when the field is that prefix
/// followed by a non-empty identifier — the same reading
/// [`super::protected_resource_type`] places an ARN by.
fn named_resource<'a>(resource: &'a str, prefix: &str) -> Option<&'a str> {
    let id = resource.strip_prefix(prefix)?;
    (!id.is_empty()).then_some(id)
}

/// The codes Amazon EC2 refuses an allocation id it cannot resolve with: one for an id no
/// address of the account has, and one for a value that is not an allocation id at all.
const ALLOCATION_ABSENCE_CODES: &[&str] = &[
    "InvalidAllocationID.NotFound",
    "InvalidAllocationID.Malformed",
];

/// Whether Amazon EC2 holds no Elastic IP allocation under `allocation_id`.
async fn absent_elastic_ip(
    ctx: &OperationContext<'_>,
    region: &str,
    allocation_id: &str,
) -> Option<OwningServiceRefusal> {
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    let answer = ::aws_sdk_ec2::Client::from_conf(config)
        .describe_addresses()
        .allocation_ids(allocation_id)
        .send()
        .await;
    // Amazon EC2's `ec2Query` failures are not modelled error shapes, so the absence is
    // the error CODE that service publishes for it rather than a typed variant.
    reported_refusal(answer.err()?, |error| {
        ALLOCATION_ABSENCE_CODES.contains(&error.meta().code().unwrap_or_default())
    })
}

/// Whether Elastic Load Balancing v2 holds no load balancer under `load_balancer_arn`.
async fn absent_application_load_balancer(
    ctx: &OperationContext<'_>,
    region: &str,
    load_balancer_arn: &str,
) -> Option<OwningServiceRefusal> {
    use ::aws_sdk_elasticloadbalancingv2 as elbv2;

    let config = elbv2::Config::builder()
        .behavior_version(elbv2::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(elbv2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://elasticloadbalancing.{region}.amazonaws.com"))
        .retry_config(elbv2::config::retry::RetryConfig::disabled())
        .timeout_config(elbv2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(elbv2::config::IdentityCache::no_cache())
        .stalled_stream_protection(elbv2::config::StalledStreamProtectionConfig::disabled())
        .build();
    let answer = elbv2::Client::from_conf(config)
        .describe_load_balancers()
        .load_balancer_arns(load_balancer_arn)
        .send()
        .await;
    reported_refusal(
        answer.err()?,
        elbv2::operation::describe_load_balancers::DescribeLoadBalancersError::is_load_balancer_not_found_exception,
    )
}

/// Whether Elastic Load Balancing holds no Classic Load Balancer under `name`.
async fn absent_classic_load_balancer(
    ctx: &OperationContext<'_>,
    region: &str,
    name: &str,
) -> Option<OwningServiceRefusal> {
    use ::aws_sdk_elasticloadbalancing as elb;

    let config = elb::Config::builder()
        .behavior_version(elb::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(elb::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://elasticloadbalancing.{region}.amazonaws.com"))
        .retry_config(elb::config::retry::RetryConfig::disabled())
        .timeout_config(elb::config::timeout::TimeoutConfig::disabled())
        .identity_cache(elb::config::IdentityCache::no_cache())
        .stalled_stream_protection(elb::config::StalledStreamProtectionConfig::disabled())
        .build();
    let answer = elb::Client::from_conf(config)
        .describe_load_balancers()
        .load_balancer_names(name)
        .send()
        .await;
    reported_refusal(
        answer.err()?,
        elb::operation::describe_load_balancers::DescribeLoadBalancersError::is_access_point_not_found_exception,
    )
}

/// Whether Amazon CloudFront holds no distribution under `id`.
async fn absent_distribution(
    ctx: &OperationContext<'_>,
    id: &str,
) -> Option<OwningServiceRefusal> {
    use ::aws_sdk_cloudfront as cloudfront;

    let config = cloudfront::Config::builder()
        .behavior_version(cloudfront::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(cloudfront::config::Region::new(
            GLOBAL_SIGNING_REGION.to_string(),
        ))
        .endpoint_url("https://cloudfront.amazonaws.com")
        .retry_config(cloudfront::config::retry::RetryConfig::disabled())
        .timeout_config(cloudfront::config::timeout::TimeoutConfig::disabled())
        .identity_cache(cloudfront::config::IdentityCache::no_cache())
        .stalled_stream_protection(cloudfront::config::StalledStreamProtectionConfig::disabled())
        .build();
    let answer = cloudfront::Client::from_conf(config)
        .get_distribution()
        .id(id)
        .send()
        .await;
    reported_refusal(
        answer.err()?,
        cloudfront::operation::get_distribution::GetDistributionError::is_no_such_distribution,
    )
}

/// Whether AWS Global Accelerator holds no accelerator under `accelerator_arn`.
async fn absent_accelerator(
    ctx: &OperationContext<'_>,
    accelerator_arn: &str,
) -> Option<OwningServiceRefusal> {
    use ::aws_sdk_globalaccelerator as globalaccelerator;

    let config = globalaccelerator::Config::builder()
        .behavior_version(globalaccelerator::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(globalaccelerator::config::Region::new(
            GLOBAL_ACCELERATOR_SIGNING_REGION.to_string(),
        ))
        .endpoint_url(format!(
            "https://globalaccelerator.{GLOBAL_ACCELERATOR_SIGNING_REGION}.amazonaws.com"
        ))
        .retry_config(globalaccelerator::config::retry::RetryConfig::disabled())
        .timeout_config(globalaccelerator::config::timeout::TimeoutConfig::disabled())
        .identity_cache(globalaccelerator::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            globalaccelerator::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    let answer = globalaccelerator::Client::from_conf(config)
        .describe_accelerator()
        .accelerator_arn(accelerator_arn)
        .send()
        .await;
    reported_refusal(
        answer.err()?,
        globalaccelerator::operation::describe_accelerator::DescribeAcceleratorError::is_accelerator_not_found_exception,
    )
}

/// Whether Amazon Route 53 holds no hosted zone under `id`.
async fn absent_hosted_zone(
    ctx: &OperationContext<'_>,
    id: &str,
) -> Option<OwningServiceRefusal> {
    use ::aws_sdk_route53 as route53;

    let config = route53::Config::builder()
        .behavior_version(route53::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(route53::config::Region::new(
            GLOBAL_SIGNING_REGION.to_string(),
        ))
        .endpoint_url("https://route53.amazonaws.com")
        .retry_config(route53::config::retry::RetryConfig::disabled())
        .timeout_config(route53::config::timeout::TimeoutConfig::disabled())
        .identity_cache(route53::config::IdentityCache::no_cache())
        .stalled_stream_protection(route53::config::StalledStreamProtectionConfig::disabled())
        .build();
    let answer = route53::Client::from_conf(config)
        .get_hosted_zone()
        .id(id)
        .send()
        .await;
    reported_refusal(
        answer.err()?,
        route53::operation::get_hosted_zone::GetHostedZoneError::is_no_such_hosted_zone,
    )
}

/// What the owning service ANSWERED, when its answer is the absence `absent` recognises:
/// the status, code and sentence it worded, and the id of the call it answered.
///
/// The call's id is the one the called service's own answer reports
/// ([`REQUEST_ID_METADATA`], [`REQUEST_ID_HEADER`]) and, for a service whose answer
/// carries none this can read, one minted for that call: it identifies the internal call
/// this handler made, so there is one whichever way the answer arrived.
///
/// `None` for every other failure — a refusal of another class, and a call that reached no
/// such service at all, which is what an emulator assembled without it gives. Neither is
/// evidence about the resource, so it is reported for a developer rather than turned into
/// one: a deployment with no owning service to ask is one where this gate does not run.
fn reported_refusal<E: ProvideErrorMetadata + ::std::fmt::Debug>(
    error: SdkError<E, HttpResponse>,
    absent: impl FnOnce(&E) -> bool,
) -> Option<OwningServiceRefusal> {
    let Some(answered) = error.as_service_error().filter(|answered| absent(answered)) else {
        ::tracing::debug!(
            error = ?error,
            "a protection's resource was not resolved against the service that owns it"
        );
        return None;
    };
    let refusal = OwningServiceRefusal {
        status: error
            .raw_response()
            .map_or(0, |response| response.status().as_u16()),
        code: answered.meta().code().unwrap_or_default().to_string(),
        message: answered.meta().message().unwrap_or_default().to_string(),
        request_id: answered
            .meta()
            .extra(REQUEST_ID_METADATA)
            .or_else(|| {
                error
                    .raw_response()
                    .and_then(|response| response.headers().get(REQUEST_ID_HEADER))
            })
            .map_or_else(super::new_uuid, str::to_string),
    };
    Some(refusal)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `ProtectedResourceType` the model publishes
    /// ([`crate::ops::types::PROTECTED_RESOURCE_TYPE_VALUES`]) is a class resolved here,
    /// and every class resolved here is one of those values: a protectable resource class
    /// is never admitted unresolved, and no row names a type the model does not.
    #[test]
    fn the_resolved_classes_are_exactly_the_modelled_protected_resource_types() {
        for resource_type in crate::ops::types::PROTECTED_RESOURCE_TYPE_VALUES {
            assert!(
                protectable_class(resource_type).is_some(),
                "case {resource_type:?}: expected a resolving read for every modelled \
                 protected resource type, got none"
            );
        }
        for class in PROTECTABLE_CLASSES {
            assert!(
                crate::ops::types::PROTECTED_RESOURCE_TYPE_VALUES.contains(&class.resource_type),
                "case {:?}: every resolved class must be a modelled ProtectedResourceType, \
                 and the catalog holds {:?}",
                class.resource_type,
                crate::ops::types::PROTECTED_RESOURCE_TYPE_VALUES
            );
        }
    }

    /// Each class the documented ARN layouts place a resource in is the class this module
    /// resolves it as, so the read a request reaches is the one that owns its resource.
    #[test]
    fn each_documented_arn_layout_resolves_as_its_own_class() {
        // (case, ARN in the layout `CreateProtectionRequest$ResourceArn` documents for the
        //  class, the class it is resolved as)
        let rows: [(&str, &str, &str); 7] = [
            (
                "an Elastic IP allocation",
                "arn:aws:ec2:us-east-1:000000000000:eip-allocation/eipalloc-0123456789abcdef0",
                ELASTIC_IP_ALLOCATION,
            ),
            (
                "an Application Load Balancer",
                "arn:aws:elasticloadbalancing:eu-west-1:000000000000:loadbalancer/app/web/50dc6c495c0c9188",
                APPLICATION_LOAD_BALANCER,
            ),
            (
                "a Classic Load Balancer",
                "arn:aws:elasticloadbalancing:ap-south-1:000000000000:loadbalancer/legacy-web",
                CLASSIC_LOAD_BALANCER,
            ),
            (
                "a CloudFront distribution",
                "arn:aws:cloudfront::000000000000:distribution/E1XMPLDISTRIBUTION",
                CLOUDFRONT_DISTRIBUTION,
            ),
            (
                "a Global Accelerator standard accelerator",
                "arn:aws:globalaccelerator::000000000000:accelerator/1234abcd-abcd-1234-abcd-1234abcdefgh",
                GLOBAL_ACCELERATOR,
            ),
            (
                "a Route 53 hosted zone",
                "arn:aws:route53:::hostedzone/Z1XMPLHOSTEDZONE",
                ROUTE_53_HOSTED_ZONE,
            ),
            (
                "a partition the layouts do not name",
                "arn:aws-cn:ec2:cn-north-1:000000000000:eip-allocation/eipalloc-0123456789abcdef0",
                ELASTIC_IP_ALLOCATION,
            ),
        ];

        for (case, arn, expected) in rows {
            let placed = super::super::protected_resource_type(arn);
            assert_eq!(
                placed,
                Some(expected),
                "case {case:?}: expected the ARN to be placed in {expected:?}, got {placed:?}"
            );
            let resolved = placed.and_then(protectable_class).map(|class| class.label);
            assert!(
                resolved.is_some(),
                "case {case:?}: expected a resolving read for the class, got none"
            );
        }
    }

    /// The refusal states the class, the ARN and Region the REQUEST carried, and the
    /// owning service's own answer, in the one form AWS words it in.
    #[test]
    fn the_refusal_states_the_class_the_arn_and_the_owning_service_s_answer() {
        let class = protectable_class(ELASTIC_IP_ALLOCATION).expect("the Elastic IP class");
        let arn = "arn:aws:ec2:eu-west-2:111122223333:eip-allocation/eipalloc-0123456789abcdef0";
        let message = failed_to_validate_message(
            class,
            arn,
            "eu-west-2",
            &OwningServiceRefusal {
                status: 400,
                code: "InvalidAllocationID.NotFound".to_string(),
                message: "The allocation ID 'eipalloc-0123456789abcdef0' does not exist"
                    .to_string(),
                request_id: "11111111-2222-3333-4444-555555555555".to_string(),
            },
        );
        let expected = format!(
            "Failed to validate EIP resource: {arn} in region: eu-west-2, error: The \
             allocation ID 'eipalloc-0123456789abcdef0' does not exist (Service: AmazonEC2; \
             Status Code: 400; Error Code: InvalidAllocationID.NotFound; Request ID: \
             11111111-2222-3333-4444-555555555555; Proxy: null)"
        );
        assert_eq!(
            message, expected,
            "case an absent Elastic IP allocation: expected {expected:?}, got {message:?}"
        );
    }
}
