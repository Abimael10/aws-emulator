// SPDX-License-Identifier: CC0-1.0

//! What an `AWS::CloudFront::Distribution` whose declaration REFERENCES a CloudFront resource
//! that does not exist reports, end to end over the awsQuery wire with Amazon CloudFront
//! mounted beside CloudFormation on one engine.
//!
//! RULE under test: **a cache behaviour attaches resources CloudFront holds apart from the
//! distribution — a cache policy, an origin request policy, a response headers policy, a
//! real-time log configuration, a field-level encryption configuration — so a declaration
//! naming one that is not there is refused by CloudFront itself with that kind's own
//! `NoSuch…`, and the resource's `ResourceStatusReason` carries that service's own sentence
//! followed by its SDK's diagnostic of the 404, reported under the plugin's INVALID-REQUEST
//! class** — the fault is in the TEMPLATE, which named a resource that does not exist, not in
//! the distribution the provider was managing — **and the stack rolls back, deleting the
//! resources it had already created.**
//!
//! Why it matters: a client reading a rolled-back deployment diagnoses it from the failed
//! resource's `ResourceStatusReason` and branches on its `HandlerErrorCode`. A reason that
//! names a fault of the handler's own making rather than CloudFront's refusal attributes the
//! failure to nothing a caller can fix, and a declaration that gives only
//! `OriginProtocolPolicy` for a custom origin — the ordinary shape for an API Gateway, load
//! balancer or function-URL origin — must reach CloudFront with the ports the resource type
//! documents defaults for, so that CloudFront's own validation is what answers.
//!
//! The rows differ along what the rule generalises over: all five reference members a cache
//! behaviour carries, each with its own error code and sentence; identifiers of several shapes
//! nothing holds (uuids of each namespace, an ARN whose name no configuration answers to);
//! and both places a behaviour can carry a reference (the default cache behaviour and an
//! ordered one). Every row's custom origin states only its protocol policy, so each one also
//! exercises the ports the resource type supplies.
//!
//! The POSITIVE CONTROL deploys the same distribution against a cache policy the SAME template
//! declares: the refusal above is what an absent reference earns, and a reference that resolves
//! still completes.

use serde_json::{json, Value};

use crate::harness::{
    assert_sdk_diagnostic_with_attempts, cloudfront, failed_resource_reason,
    physical_resource_id, quoted_and_class, settled_stack_status, CLOUDFRONT_API_VERSION,
};
use crate::stack_lifecycle::{
    create_stack, router, settled_stack, xml_text, xml_texts, ACCOUNT_ID, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// The name Amazon CloudFront's SDK states for the service in the diagnostic it appends to the
/// exception it raises for a refused call: that model's own `sdkId`
/// (`smithy_query(query="service_info")`).
const CLOUDFRONT_SERVICE: &str = "CloudFront";

/// The status CloudFront answers a reference that resolves to nothing with: the `httpError`
/// its model declares for every one of these error shapes
/// (`smithy_query(query="shape",shape="NoSuchCachePolicy")` and its siblings).
const ABSENT_REFERENCE_STATUS: u16 = 404;

/// The `HandlerErrorCode` class the plugin reports a refusal of the DECLARATION under — a
/// value of the model's own `HandlerErrorCode` enum.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The resource status a resource its provider could not create records.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// Every `Name` the listing at `path` reports — the names CloudFront holds for that resource
/// kind, which is what says whether a rollback released the one a stack created.
async fn listed_names(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    path: &str,
) -> Vec<String> {
    let (code, body) = cloudfront(router, "GET", path).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the listing {path} must answer 200, got {code} {body}"
    );
    xml_texts(&body, "Name")
}

/// One custom origin naming ONLY its protocol policy, which is the shape the resource type
/// documents the two ports' defaults for.
fn origins() -> Value {
    json!([{
        "Id": "the-origin",
        "DomainName": "origin.example.com",
        "CustomOriginConfig": {"OriginProtocolPolicy": "https-only"},
    }])
}

/// Where a declaration carries the reference under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// On `DefaultCacheBehavior`.
    Default,
    /// On one ordered cache behaviour.
    Ordered,
}

/// The `DistributionConfig` of a distribution whose behaviour at `placement` attaches `value`
/// through `member`.
fn distribution_config(member: &str, value: &str, placement: Placement) -> Value {
    let mut default = json!({
        "TargetOriginId": "the-origin",
        "ViewerProtocolPolicy": "allow-all",
    });
    let mut ordered = json!({
        "PathPattern": "/images/*",
        "TargetOriginId": "the-origin",
        "ViewerProtocolPolicy": "https-only",
    });
    let carrier = match placement {
        Placement::Default => &mut default,
        Placement::Ordered => &mut ordered,
    };
    carrier
        .as_object_mut()
        .expect("the behaviour is an object")
        .insert(member.to_string(), Value::String(value.to_string()));
    json!({
        "Enabled": true,
        "Comment": "a distribution attaching a resource by identifier",
        "Origins": origins(),
        "DefaultCacheBehavior": default,
        "CacheBehaviors": [ordered],
    })
}

/// One row: which reference member a declaration carries, what it names, and what CloudFront's
/// refusal of it has to say.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The member the reference travels in.
    member: &'static str,
    /// An identifier of that member's own kind that no record answers to.
    absent: String,
    /// Which behaviour carries it.
    placement: Placement,
    /// CloudFront's own sentence for that kind, word for word.
    sentence: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a cache policy the declaration names and CloudFront does not hold",
            stack: "cf-reference-absent-cache-policy",
            member: "CachePolicyId",
            absent: "0c5e53f3-9d2a-4f27-8a5b-1f4e7f6a2b11".to_string(),
            placement: Placement::Default,
            sentence: "The specified cache policy does not exist.",
        },
        Row {
            case: "the same cache policy named by an ORDERED cache behaviour",
            stack: "cf-reference-absent-cache-policy-ordered",
            member: "CachePolicyId",
            // The nil uuid, which is well formed and is no policy's.
            absent: "00000000-0000-0000-0000-000000000000".to_string(),
            placement: Placement::Ordered,
            sentence: "The specified cache policy does not exist.",
        },
        Row {
            case: "an origin request policy the declaration names and CloudFront does not hold",
            stack: "cf-reference-absent-origin-request-policy",
            member: "OriginRequestPolicyId",
            absent: "6f1b2c3d-4e5a-4b6c-8d9e-0f1a2b3c4d5e".to_string(),
            placement: Placement::Default,
            sentence: "The specified origin request policy does not exist.",
        },
        Row {
            case: "a response headers policy the declaration names and CloudFront does not hold",
            stack: "cf-reference-absent-response-headers-policy",
            member: "ResponseHeadersPolicyId",
            absent: "7b1c4f90-3e52-4c8a-9d6f-2a0b5c7d8e9f".to_string(),
            placement: Placement::Ordered,
            sentence: "The specified response headers policy does not exist.",
        },
        Row {
            case: "a real-time log configuration the declaration names and CloudFront does not \
                   hold",
            stack: "cf-reference-absent-realtime-log-config",
            member: "RealtimeLogConfigArn",
            absent: format!(
                "arn:aws:cloudfront::{ACCOUNT_ID}:realtime-log-config/absent-log-configuration"
            ),
            placement: Placement::Default,
            sentence: "The specified real-time log configuration does not exist.",
        },
        Row {
            case: "a field-level encryption configuration the declaration names and CloudFront \
                   does not hold",
            stack: "cf-reference-absent-field-level-encryption",
            member: "FieldLevelEncryptionId",
            absent: "A1B2C3D4E5F6G7".to_string(),
            placement: Placement::Ordered,
            sentence: "The specified field level encryption config does not exist.",
        },
    ]
}

#[tokio::test]
async fn a_distribution_naming_a_resource_cloudfront_does_not_hold_reports_that_refusal() {
    for row in rows() {
        let router = router();
        let template = json!({"Resources": {
            "Cdn": {
                "Type": "AWS::CloudFront::Distribution",
                "Properties": {"DistributionConfig":
                    distribution_config(row.member, &row.absent, row.placement)},
            }
        }})
        .to_string();

        let (code, created) = create_stack(&router, row.stack, &template, &[]).await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must answer 200, got {code} {created}",
            row.case
        );
        let body = settled_stack(&router, row.case, row.stack).await;
        assert_eq!(
            xml_text(&body, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {:?}: expected the refused create to be rolled back, got {body}",
            row.case
        );

        let reason = failed_resource_reason(
            row.case,
            &router,
            REGION,
            row.stack,
            "Cdn",
            CREATE_FAILED,
        )
        .await;
        let (quoted, class) = quoted_and_class(row.case, &reason);
        assert_eq!(
            class, INVALID_REQUEST,
            "case {:?}: expected the {INVALID_REQUEST} class for a reference the declaration \
             named, got {class:?} in {reason:?}",
            row.case
        );
        assert!(
            quoted.starts_with(row.sentence),
            "case {:?}: expected the message to open with {:?}, got {quoted:?}",
            row.case,
            row.sentence
        );
        assert_sdk_diagnostic_with_attempts(
            row.case,
            &quoted,
            CLOUDFRONT_SERVICE,
            ABSENT_REFERENCE_STATUS,
        );
        // The refusal names the kind that is missing and nothing of the identifier the
        // template wrote: the value is the caller's own, and repeating it is not what CloudFront
        // answers.
        assert!(
            !quoted.contains(&row.absent),
            "case {:?}: expected the refusal to carry nothing of the declared identifier, got \
             {quoted:?}",
            row.case
        );
    }
}

/// The name the rollback case's cache policy is declared under, and its origin access control's.
const ROLLED_BACK_POLICY: &str = "cf-rollback-released-policy";
const ROLLED_BACK_CONTROL: &str = "cf-rollback-released-control";

/// The resources a stack CREATED before its distribution was refused are released by the
/// rollback: the cache policy and the origin access control the same template declared are not
/// held by CloudFront once the stack reaches `ROLLBACK_COMPLETE`.
#[tokio::test]
async fn the_rollback_releases_the_resources_the_stack_had_already_created() {
    let case = "a distribution refused after its siblings were created";
    let stack = "cf-reference-rollback-releases-siblings";
    let router = router();
    let template = json!({"Resources": {
        "Policy": {
            "Type": "AWS::CloudFront::CachePolicy",
            "Properties": {"CachePolicyConfig": {
                "Name": ROLLED_BACK_POLICY,
                "MinTTL": 1,
                "ParametersInCacheKeyAndForwardedToOrigin": {
                    "EnableAcceptEncodingGzip": false,
                    "HeadersConfig": {"HeaderBehavior": "none"},
                    "CookiesConfig": {"CookieBehavior": "none"},
                    "QueryStringsConfig": {"QueryStringBehavior": "none"},
                },
            }},
        },
        "Control": {
            "Type": "AWS::CloudFront::OriginAccessControl",
            "Properties": {"OriginAccessControlConfig": {
                "Name": ROLLED_BACK_CONTROL,
                "OriginAccessControlOriginType": "s3",
                "SigningBehavior": "always",
                "SigningProtocol": "sigv4",
            }},
        },
        "Cdn": {
            "Type": "AWS::CloudFront::Distribution",
            // Declared AFTER both, so the two are created before the distribution is
            // attempted and the rollback has something to release.
            "DependsOn": ["Policy", "Control"],
            "Properties": {"DistributionConfig": distribution_config(
                "CachePolicyId",
                "3f2d1c0b-9a8e-4d7c-b6a5-5e4f3d2c1b0a",
                Placement::Default,
            )},
        },
    }})
    .to_string();

    let (code, created) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let body = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&body, "StackStatus").as_deref(),
        Some(status::ROLLBACK_COMPLETE),
        "case {case:?}: expected the refused create to be rolled back, got {body}"
    );

    let reason =
        failed_resource_reason(case, &router, REGION, stack, "Cdn", CREATE_FAILED).await;
    assert!(
        reason.contains("The specified cache policy does not exist."),
        "case {case:?}: expected the distribution's reason to name the missing cache policy, \
         got {reason:?}"
    );

    for (kind, path, name) in [
        (
            "cache policy",
            format!("/{CLOUDFRONT_API_VERSION}/cache-policy"),
            ROLLED_BACK_POLICY,
        ),
        (
            "origin access control",
            format!("/{CLOUDFRONT_API_VERSION}/origin-access-control"),
            ROLLED_BACK_CONTROL,
        ),
    ] {
        let held = listed_names(&router, case, &path).await;
        assert!(
            !held.iter().any(|listed| listed == name),
            "case {case:?}: expected the rollback to release the {kind} {name:?}, got {held:?}"
        );
    }
}

/// The control: the same distribution against a cache policy the SAME template declares
/// completes, and the behaviour CloudFront holds attaches the id that stack published — so the
/// refusal above is what an ABSENT reference earns rather than something every declaration
/// carrying one now meets.
#[tokio::test]
async fn a_reference_that_resolves_still_completes() {
    let case = "a distribution attaching a cache policy the same template declares";
    let stack = "cf-reference-resolved-cache-policy";
    let router = router();
    let mut config = distribution_config("CachePolicyId", "", Placement::Default);
    config
        .get_mut("DefaultCacheBehavior")
        .and_then(Value::as_object_mut)
        .expect("the default cache behaviour is an object")
        .insert("CachePolicyId".to_string(), json!({"Ref": "Policy"}));
    let template = json!({"Resources": {
        "Policy": {
            "Type": "AWS::CloudFront::CachePolicy",
            "Properties": {"CachePolicyConfig": {
                "Name": "cf-reference-resolved-policy",
                "MinTTL": 1,
                "ParametersInCacheKeyAndForwardedToOrigin": {
                    "EnableAcceptEncodingGzip": false,
                    "HeadersConfig": {"HeaderBehavior": "none"},
                    "CookiesConfig": {"CookieBehavior": "none"},
                    "QueryStringsConfig": {"QueryStringBehavior": "none"},
                },
            }},
        },
        "Cdn": {
            "Type": "AWS::CloudFront::Distribution",
            "Properties": {"DistributionConfig": config},
        },
    }})
    .to_string();

    let (code, created) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let settled = settled_stack_status(case, &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::CREATE_COMPLETE,
        "case {case:?}: the stack must reach CREATE_COMPLETE, got {settled:?}"
    );

    // The distribution CloudFront holds attaches the policy by the id that service minted, and
    // the custom origin the declaration gave only a protocol policy for carries the ports the
    // resource type documents defaults for.
    let policy_id = physical_resource_id(&router, case, stack, "Policy").await;
    let distribution_id = physical_resource_id(&router, case, stack, "Cdn").await;

    let (code, document) = cloudfront(
        &router,
        "GET",
        &format!("/{CLOUDFRONT_API_VERSION}/distribution/{distribution_id}/config"),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: GetDistributionConfig({distribution_id}) must answer 200, got {code} \
         {document}"
    );
    assert_eq!(
        xml_text(&document, "CachePolicyId").as_deref(),
        Some(policy_id.as_str()),
        "case {case:?}: expected the behaviour to attach the declared policy, got {document}"
    );
    for (member, defaulted) in [("HTTPPort", "80"), ("HTTPSPort", "443")] {
        assert_eq!(
            xml_text(&document, member).as_deref(),
            Some(defaulted),
            "case {case:?}: expected the resource type's default {member} {defaulted}, got \
             {document}"
        );
    }
}
