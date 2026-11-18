// SPDX-License-Identifier: CC0-1.0

//! WHICH distribution serves a viewer request while a CONTINUOUS DEPLOYMENT POLICY is
//! attached, end to end over HTTP.
//!
//! The rule: a distribution that attaches an ENABLED continuous deployment policy serves the
//! requests that policy SELECTS from its staging distribution — "contains the parameters for
//! routing production traffic from your primary to staging distributions" — and every other
//! request from itself. The `SingleHeader` traffic configuration is the one a request selects
//! itself into: "if the HTTP request contains a header and value that matches what you specify
//! here, the request is sent to the staging distribution. Otherwise the request is sent to the
//! primary distribution." A policy that is not enabled "has no effect", so it selects nothing
//! and the primary serves every request.
//!
//! What serves the request is the whole configuration of the distribution that serves it: the
//! cache behaviour it matches and the response headers policy that behaviour attaches are the
//! serving distribution's. Each distribution here attaches a policy that stamps its own name
//! on the answer, so the header the viewer reads says WHICH distribution served it.
//!
//! The rows differ along what the rule generalises over: whether the request carries the
//! configured header, whether its value matches, the CASE the header name is spelled in (a
//! header name is case-insensitive), whether another header carries the matching value, and
//! whether the policy is enabled — including after it is disabled and enabled again.
//!
//! The harness is the lifecycle suites', reused rather than copied.

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::continuous_deployment_policies::{create_policy as create_cd_policy, Policy, Traffic};
use crate::continuous_deployment_policy_update::update_policy as update_cd_policy;
use crate::distribution_lifecycle::{
    awaited_deployment, call, create_distribution, emulator, API_VERSION, HOST,
};
use crate::response_headers_policy_lifecycle::{
    create_policy as create_response_headers_policy, Policy as ResponseHeadersPolicy,
};

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: the
/// behaviours below cache nothing, so every viewer request is decided and served afresh.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// An origin this emulator SERVES: this service's own control-plane endpoint, which answers
/// `GET /2020-05-31/distribution` with a `DistributionList`. Both distributions name it, so
/// the bytes are the same whichever serves the request and the marker header is what says
/// which one did.
const SERVED_ORIGIN: &str = HOST;

/// A viewer path the served origin answers.
const SERVED_PATH: &str = "/2020-05-31/distribution";

/// The header the distributions stamp their own name on the answer with. A header of the
/// TEST's choosing, carried by a response headers policy a client created.
const MARKER: &str = "x-served-by";

/// The `ContinuousDeploymentSingleHeaderConfig$Header` the policy selects requests by: "the
/// header must contain the prefix `aws-cf-cd-`".
const ROUTING_HEADER: &str = "aws-cf-cd-slice";

/// The value that header has to carry for the policy to select the request.
const ROUTING_VALUE: &str = "staging-slice";

/// Send one VIEWER request: no AWS credential, addressed to `host` — which is what resolves
/// the distribution — carrying `headers` beside the `Host`, exactly as a browser reaches a
/// distribution.
async fn view(
    router: &MultiServiceRouter,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> (u16, String, String) {
    let mut builder = ::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let marker = response
        .headers()
        .get(MARKER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, marker, body)
}

/// Create a response headers policy that stamps `marker` on every answer it is applied to,
/// and report its id.
async fn marking_policy(router: &MultiServiceRouter, marker: &str) -> String {
    let policy = ResponseHeadersPolicy {
        case: "the marking policy",
        name: format!("marks-{marker}"),
        comment: Some(format!("stamps {MARKER}: {marker}")),
        custom_headers: Some(vec![(MARKER.to_string(), marker.to_string(), true)]),
        ..ResponseHeadersPolicy::default()
    };
    let created = create_response_headers_policy(router, &policy.xml()).await;
    assert_eq!(
        created.status, 201,
        "creating the {marker} response headers policy answered {} with {}",
        created.status, created.raw
    );
    created
        .root()
        .find_text("Id")
        .expect("the created response headers policy reports an id")
        .to_string()
}

/// The `DistributionConfig` of one distribution these cases serve through: one origin, a
/// default behaviour that caches nothing and attaches the response headers policy
/// `response_headers_policy`, and the continuous deployment policy `continuous_deployment`
/// where the row attaches one.
fn distribution_xml(
    reference: &str,
    response_headers_policy: &str,
    continuous_deployment: Option<&str>,
    staging: bool,
) -> String {
    let continuous_deployment_element = continuous_deployment
        .map(|id| format!("<ContinuousDeploymentPolicyId>{id}</ContinuousDeploymentPolicyId>"))
        .unwrap_or_default();
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>only</Id><DomainName>{SERVED_ORIGIN}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>only</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         <ResponseHeadersPolicyId>{response_headers_policy}</ResponseHeadersPolicyId>\
         </DefaultCacheBehavior>\
         <Comment>continuous deployment routing</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled>\
         {continuous_deployment_element}\
         <Staging>{staging}</Staging>\
         </DistributionConfig>"
    )
}

/// What one created distribution is addressed and updated by.
struct Deployed {
    id: String,
    domain: String,
    etag: String,
    /// The `CallerReference` it was created under, which an update submits unchanged.
    reference: String,
}

/// Create one distribution and let the clock deploy it.
async fn deployed(
    router: &MultiServiceRouter,
    reference: &str,
    response_headers_policy: &str,
    staging: bool,
) -> Deployed {
    let created = create_distribution(
        router,
        &distribution_xml(reference, response_headers_policy, None, staging),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "creating the distribution {reference} answered {} with {}",
        created.status, created.raw
    );
    let deployed = Deployed {
        id: created
            .root()
            .find_text("Id")
            .expect("the created distribution reports an id")
            .to_string(),
        domain: created
            .root()
            .find_text("DomainName")
            .expect("the created distribution reports a domain name")
            .to_string(),
        etag: created.header("etag").to_string(),
        reference: reference.to_string(),
    };
    awaited_deployment(router, reference, &deployed.id).await;
    deployed
}

/// Attach `policy` to the distribution `distribution` and report the version token the update
/// handed back.
async fn attach_policy(
    router: &MultiServiceRouter,
    distribution: &Deployed,
    response_headers_policy: &str,
    policy: &str,
) -> String {
    let updated = call(
        router,
        "PUT",
        &format!("/{API_VERSION}/distribution/{}/config", distribution.id),
        Some(&distribution_xml(
            &distribution.reference,
            response_headers_policy,
            Some(policy),
            false,
        )),
        Some(&distribution.etag),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "attaching the continuous deployment policy answered {} with {}",
        updated.status, updated.raw
    );
    assert_eq!(
        updated
            .root()
            .find_text("ContinuousDeploymentPolicyId")
            .unwrap_or_default(),
        policy,
        "the updated configuration does not attach the policy: {}",
        updated.raw
    );
    awaited_deployment(router, "the primary after the attach", &distribution.id).await;
    updated.header("etag").to_string()
}

/// One row of the routing table: what the request is a case of, the headers the viewer
/// carries beside the `Host`, and the distribution that must serve it.
type RoutedRequest<'r> = (&'r str, Vec<(&'r str, &'r str)>, &'r str);

/// A matching header reaches the staging distribution and everything else the primary — and
/// the policy's `Enabled` flag is what decides whether anything reaches staging at all.
#[tokio::test]
async fn a_matching_single_header_is_served_by_the_staging_distribution() {
    let router = emulator();
    let primary_marking = marking_policy(&router, "primary").await;
    let staging_marking = marking_policy(&router, "staging").await;

    let primary = deployed(&router, "cd-routing-primary", &primary_marking, false).await;
    let staging = deployed(&router, "cd-routing-staging", &staging_marking, true).await;

    // The policy a client creates over the staging distribution's own domain name, and the
    // attach that puts it in front of the primary.
    let policy = Policy {
        case: "the routing policy",
        dns_names: vec![staging.domain.clone()],
        enabled: true,
        traffic: Traffic::SingleHeader {
            header: ROUTING_HEADER.to_string(),
            value: ROUTING_VALUE.to_string(),
        },
    };
    let created = create_cd_policy(&router, &policy.xml()).await;
    assert_eq!(
        created.status, 201,
        "creating the continuous deployment policy answered {} with {}",
        created.status, created.raw
    );
    let policy_id = created
        .root()
        .find_text("Id")
        .expect("the created policy reports an id")
        .to_string();
    let mut policy_etag = created.header("etag").to_string();
    attach_policy(&router, &primary, &primary_marking, &policy_id).await;

    // (case, the headers the viewer carries, the distribution that must serve it).
    let rows: Vec<RoutedRequest<'_>> = vec![
        (
            "the configured header and value",
            vec![(ROUTING_HEADER, ROUTING_VALUE)],
            "staging",
        ),
        (
            "the configured header name in another case",
            vec![("AWS-CF-CD-SLICE", ROUTING_VALUE)],
            "staging",
        ),
        (
            "the configured header beside another header",
            vec![("x-request-id", "r-1"), (ROUTING_HEADER, ROUTING_VALUE)],
            "staging",
        ),
        (
            "the configured header with another value",
            vec![(ROUTING_HEADER, "another-slice")],
            "primary",
        ),
        (
            "the configured header with an empty value",
            vec![(ROUTING_HEADER, "")],
            "primary",
        ),
        (
            "the matching value under another header name",
            vec![("aws-cf-cd-other", ROUTING_VALUE)],
            "primary",
        ),
        ("no header at all", Vec::new(), "primary"),
    ];

    for (case, headers, expected) in &rows {
        let (status, marker, body) = view(&router, &primary.domain, SERVED_PATH, headers).await;
        assert_eq!(
            status, 200,
            "case {case}: expected the request to be served, got {status} with {body:?}"
        );
        assert_eq!(
            marker, *expected,
            "case {case}: expected the {expected} distribution to serve it, got {marker:?} \
             (body {body:?})"
        );
        assert!(
            body.contains("DistributionList"),
            "case {case}: expected the origin's own bytes, got {body:?}"
        );
    }

    // A policy that is not enabled "has no effect": the SAME matching request is served by the
    // primary while it is disabled, and by staging again once it is enabled.
    for (case, enabled, expected) in [
        ("the policy disabled", false, "primary"),
        ("the policy enabled again", true, "staging"),
    ] {
        let moved = Policy {
            case: "the routing policy",
            dns_names: vec![staging.domain.clone()],
            enabled,
            traffic: policy.traffic.clone(),
        };
        let updated = update_cd_policy(&router, &policy_id, &moved.xml(), Some(&policy_etag)).await;
        assert_eq!(
            updated.status, 200,
            "case {case}: updating the policy answered {} with {}",
            updated.status, updated.raw
        );
        policy_etag = updated.header("etag").to_string();

        let (status, marker, body) = view(
            &router,
            &primary.domain,
            SERVED_PATH,
            &[(ROUTING_HEADER, ROUTING_VALUE)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the request to be served, got {status} with {body:?}"
        );
        assert_eq!(
            marker, expected,
            "case {case}: expected the {expected} distribution to serve the matching request, \
             got {marker:?}"
        );
    }

    // The staging distribution's own address is unaffected by any of this: a viewer that
    // addresses it directly is served by it, with or without the routing header.
    for (case, headers) in [
        ("with the routing header", vec![(ROUTING_HEADER, ROUTING_VALUE)]),
        ("without it", Vec::new()),
    ] {
        let (status, marker, body) = view(&router, &staging.domain, SERVED_PATH, &headers).await;
        assert_eq!(
            status, 200,
            "case the staging address {case}: expected the request to be served, got {status} \
             with {body:?}"
        );
        assert_eq!(
            marker, "staging",
            "case the staging address {case}: expected the staging distribution to serve it, \
             got {marker:?}"
        );
    }
}

/// A distribution that attaches NO policy serves every request itself, whatever headers the
/// viewer carries: the routing is the attached policy's, not the header's.
#[tokio::test]
async fn an_unattached_distribution_serves_every_request_itself() {
    let router = emulator();
    let marking = marking_policy(&router, "primary").await;
    let primary = deployed(&router, "cd-unattached-primary", &marking, false).await;

    for (case, headers) in [
        ("the routing header", vec![(ROUTING_HEADER, ROUTING_VALUE)]),
        ("no header", Vec::new()),
    ] {
        let (status, marker, body) = view(&router, &primary.domain, SERVED_PATH, &headers).await;
        assert_eq!(
            status, 200,
            "case {case}: expected the request to be served, got {status} with {body:?}"
        );
        assert_eq!(
            marker, "primary",
            "case {case}: expected the primary distribution to serve it, got {marker:?}"
        );
    }
}
