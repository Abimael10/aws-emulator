// SPDX-License-Identifier: CC0-1.0

//! CONTINUOUS DEPLOYMENT at the edge, end to end over HTTP with Amazon S3 mounted beside
//! CloudFront on one engine.
//!
//! RULE: a viewer request addressed to a distribution that attaches an ENABLED continuous
//! deployment policy is served by the STAGING distribution the policy names whenever the
//! policy's `TrafficConfig` selects that request, and by the distribution the viewer addressed
//! otherwise — "the parameters for routing production traffic from your primary to staging
//! distributions". Which of the two configurations selects the request is `TrafficConfig$Type`:
//!
//! * `SingleHeader` — "if the HTTP request contains a header and value that matches what you
//!   specify here, the request is sent to the staging distribution. Otherwise the request is
//!   sent to the primary distribution";
//! * `SingleWeight` — "the percentage of traffic to send to a staging distribution, expressed
//!   as a decimal number between 0 and 0.15. For example, a value of 0.10 means 10% of traffic
//!   is sent to the staging distribution", drawn once per viewer SESSION where the
//!   configuration names session stickiness, which "prevents the potentially inconsistent
//!   experience of sending some of a given user's requests to your staging distribution, while
//!   others are sent to your primary distribution".
//!
//! Only a suite that mounts the object store can state this: the two distributions serve two
//! different origin buckets, so WHICH distribution served a viewer request is read off the
//! bytes that came back — bytes a client really put in a bucket — rather than off anything the
//! edge says about itself.
//!
//! The rows span what the rule generalises over: whether the request carries the configured
//! header, carries it with another value, carries another header of the same family or carries
//! none; the viewer path, and so which object of which bucket answers; whether the policy is
//! enabled; whether it carries a traffic configuration at all; the weight, at each published
//! bound of "between 0 and 0.15"; and whether a weight configuration names session stickiness.

use std::sync::Arc;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    MAX_TRAFFIC_WEIGHT, MIN_SESSION_STICKINESS_TTL, MIN_TRAFFIC_WEIGHT,
};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::continuous_deployment_policies::{create_policy, Policy, Traffic};
use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_signed_access::{view, CACHING_DISABLED, MISS};
use crate::edge_signed_s3_origin::mounted;

/// The Region the origin buckets are created in, and so the Region their endpoint hosts
/// address.
const REGION: &str = "us-east-1";

/// The service principal a bucket policy grants the edge's origin read to.
const CLOUDFRONT_PRINCIPAL: &str = "cloudfront.amazonaws.com";

/// The bucket the PRIMARY distribution's origin is, and the bucket the STAGING distribution's
/// origin is: two origins, so the bytes a viewer receives say which distribution served it.
const PRIMARY_BUCKET: &str = "cf-continuous-deployment-primary";
/// See [`PRIMARY_BUCKET`].
const STAGING_BUCKET: &str = "cf-continuous-deployment-staging";

/// The two objects every bucket holds, so a row can vary the viewer PATH as well.
const ROOT_KEY: &str = "index.html";
/// See [`ROOT_KEY`].
const DEEP_KEY: &str = "deep/page.html";

/// What each bucket answers for each key: the bucket a body names is the distribution that
/// served the request.
const PRIMARY_ROOT_BODY: &str = "primary-origin root";
/// See [`PRIMARY_ROOT_BODY`].
const PRIMARY_DEEP_BODY: &str = "primary-origin deep";
/// See [`PRIMARY_ROOT_BODY`].
const STAGING_ROOT_BODY: &str = "staging-origin root";
/// See [`PRIMARY_ROOT_BODY`].
const STAGING_DEEP_BODY: &str = "staging-origin deep";

/// The header name the single-header policies below route on. "The request header name that
/// you want CloudFront to send to your staging distribution. The header must contain the prefix
/// `aws-cf-cd-`", so the name carries that prefix and the rest of it is this suite's own.
const ROUTING_HEADER: &str = "aws-cf-cd-fidelity";

/// The value that header has to carry for the request to be routed.
const ROUTING_VALUE: &str = "staged";

/// Another header of the same family, which the policy does NOT name.
const OTHER_ROUTING_HEADER: &str = "aws-cf-cd-other";

/// Which of the two distributions answered a viewer request, as the ORIGIN bytes say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ServedBy {
    Primary,
    Staging,
}

/// One row of the single-header table: the case, the viewer path, the headers the viewer
/// carries beside its `Host`, and which of the two distributions serves it.
type HeaderCase<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)], ServedBy);

/// One row of the session-stickiness table: the case, the label its distributions are created
/// under, the `(IdleTTL, MaximumTTL)` the weight configuration names (or none), and whether one
/// viewer's requests all land on one distribution.
type StickinessRow<'a> = (&'a str, &'a str, Option<(i32, i32)>, bool);

// ---------------------------------------------------------------------------
// The emulator: CloudFront plus the object store the origins live in
// ---------------------------------------------------------------------------

/// CloudFront and Amazon S3 over ONE shared engine, mounted through the one two-service
/// constructor this target has; a viewer presents no credential, so the plane is the
/// unauthenticated one.
struct Emulator {
    router: MultiServiceRouter,
    callback: EmbeddedEmulator,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = mounted(&engine, false);
        let callback = EmbeddedEmulator::from_router(router.clone());
        Self { router, callback }
    }

    /// The Amazon S3 client, built by the one shared constructor over the in-process callback,
    /// so the objects this suite stages are the ones the edge's origin fetch reads.
    fn s3(&self) -> ::aws_sdk_s3::Client {
        service_common::s3_client::s3_client(&self.callback, REGION)
    }

    /// Create `bucket`, put the two objects in it, and grant the CloudFront service principal
    /// the read its signed origin request makes.
    async fn staged_bucket(&self, bucket: &str, root_body: &str, deep_body: &str) {
        let s3 = self.s3();
        s3.create_bucket()
            .bucket(bucket)
            .send()
            .await
            .unwrap_or_else(|error| panic!("creating {bucket} must succeed, got {error:?}"));
        for (key, body) in [(ROOT_KEY, root_body), (DEEP_KEY, deep_body)] {
            s3.put_object()
                .bucket(bucket)
                .key(key)
                .body(::aws_sdk_s3::primitives::ByteStream::from(
                    body.as_bytes().to_vec(),
                ))
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!("putting {bucket}/{key} must succeed, got {error:?}")
                });
        }
        s3.put_bucket_policy()
            .bucket(bucket)
            .policy(grant(bucket))
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("the policy of {bucket} must be stored, got {error:?}")
            });
    }
}

/// A bucket policy granting the CloudFront service principal `s3:GetObject` on every object of
/// `bucket` — the document AWS publishes for an origin access control.
fn grant(bucket: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\
         \"Sid\":\"AllowCloudFrontServicePrincipal\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{CLOUDFRONT_PRINCIPAL}\"}},\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{bucket}/*\"}}]}}"
    )
}

/// The host a bucket of this Region is addressed at virtual-hosted style, which is what an S3
/// origin's `DomainName` carries.
fn origin_domain(bucket: &str) -> String {
    format!("{bucket}.s3.{REGION}.amazonaws.com")
}

// ---------------------------------------------------------------------------
// Standing up one continuous deployment
// ---------------------------------------------------------------------------

/// `CreateOriginAccessControl` for an `s3` origin CloudFront signs every request to, answering
/// the control's id.
async fn create_origin_access_control(router: &MultiServiceRouter, name: &str) -> String {
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/origin-access-control"),
        Some(&format!(
            "<OriginAccessControlConfig \
             xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <Name>{name}</Name>\
             <SigningProtocol>sigv4</SigningProtocol>\
             <SigningBehavior>always</SigningBehavior>\
             <OriginAccessControlOriginType>s3</OriginAccessControlOriginType>\
             </OriginAccessControlConfig>"
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "CreateOriginAccessControl answered {}: {}",
        created.status, created.raw
    );
    created
        .root()
        .find_text("Id")
        .expect("the created origin access control reports an id")
        .to_string()
}

/// The `DistributionConfig` of one distribution of a continuous deployment: the bucket origin
/// under the origin access control `control`, a default behaviour that caches nothing — so
/// every viewer request below really reaches an origin and the bytes say which one — and the
/// continuous deployment policy it attaches, when it attaches one.
fn distribution_config(
    caller_reference: &str,
    bucket: &str,
    control: &str,
    staging: bool,
    policy_id: Option<&str>,
) -> String {
    let continuous_deployment = policy_id
        .map(|id| format!("<ContinuousDeploymentPolicyId>{id}</ContinuousDeploymentPolicyId>"))
        .unwrap_or_default();
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>origin-bucket</Id><DomainName>{}</DomainName>\
         <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
         <OriginAccessControlId>{control}</OriginAccessControlId>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items>\
         <CachedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
         </AllowedMethods>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>continuous deployment at the edge</Comment>\
         <Staging>{staging}</Staging>\
         {continuous_deployment}\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        origin_domain(bucket)
    )
}

/// `CreateDistribution` with that configuration, answering the distribution's
/// `(Id, DomainName)`.
async fn created(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    bucket: &str,
    control: &str,
    staging: bool,
    policy_id: Option<&str>,
) -> (String, String) {
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&distribution_config(
            caller_reference,
            bucket,
            control,
            staging,
            policy_id,
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected HTTP 201 from CreateDistribution, got {}: {}",
        created.status, created.raw
    );
    let read = |member: &str| {
        created
            .root()
            .find_text(member)
            .unwrap_or_else(|| panic!("case {case:?}: the created distribution reports no {member}"))
            .to_string()
    };
    (read("Id"), read("DomainName"))
}

/// One continuous deployment standing up: a staging distribution over [`STAGING_BUCKET`], a
/// policy routing to it, and a primary distribution over [`PRIMARY_BUCKET`] attaching that
/// policy. Answers the PRIMARY's domain name, which is the only address a viewer uses.
///
/// The order is the one the control plane requires: the policy names its staging distribution
/// by "the CloudFront domain name of the staging distribution", so that distribution exists
/// first, and the primary attaches the policy's id, so the policy exists before it.
async fn deployment(
    emulator: &Emulator,
    case: &str,
    control: &str,
    label: &str,
    enabled: bool,
    traffic: Traffic,
) -> String {
    let router = &emulator.router;
    let (staging_id, staging_domain) = created(
        router,
        case,
        &format!("{label}-staging"),
        STAGING_BUCKET,
        control,
        true,
        None,
    )
    .await;
    let policy = Policy {
        case: "the continuous deployment of this row",
        dns_names: vec![staging_domain],
        enabled,
        traffic,
    };
    let answer = create_policy(router, &policy.xml()).await;
    assert_eq!(
        answer.status, 201,
        "case {case:?}: expected HTTP 201 from CreateContinuousDeploymentPolicy, got {}: {}",
        answer.status, answer.raw
    );
    let policy_id = answer
        .root()
        .find_text("Id")
        .expect("the created policy reports an id")
        .to_string();
    let (primary_id, primary_domain) = created(
        router,
        case,
        &format!("{label}-primary"),
        PRIMARY_BUCKET,
        control,
        false,
        Some(&policy_id),
    )
    .await;
    // A configuration that has not reached the edge locations serves no viewer, so BOTH
    // distributions are awaited before any request is sent.
    awaited_deployment(router, case, &staging_id).await;
    awaited_deployment(router, case, &primary_id).await;
    primary_domain
}

/// Stand up the two origin buckets and the one origin access control every row shares.
async fn given_origins(emulator: &Emulator) -> String {
    emulator
        .staged_bucket(PRIMARY_BUCKET, PRIMARY_ROOT_BODY, PRIMARY_DEEP_BODY)
        .await;
    emulator
        .staged_bucket(STAGING_BUCKET, STAGING_ROOT_BODY, STAGING_DEEP_BODY)
        .await;
    create_origin_access_control(&emulator.router, "continuous-deployment-origins").await
}

/// Send one viewer request to `host` and say which distribution served it, from the ORIGIN
/// bytes that came back.
async fn served_by(
    router: &MultiServiceRouter,
    case: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> ServedBy {
    let answer = view(router, host, path, headers).await;
    assert_eq!(
        (answer.status, answer.x_cache.as_str()),
        (200, MISS),
        "case {case:?}: expected the origin's 200 with X-Cache {MISS:?}, got {} with {:?} \
         (body {:?})",
        answer.status,
        answer.x_cache,
        answer.body
    );
    if answer.body.starts_with("primary-origin") {
        return ServedBy::Primary;
    }
    assert!(
        answer.body.starts_with("staging-origin"),
        "case {case:?}: expected the bytes of one of the two origins, got {:?}",
        answer.body
    );
    ServedBy::Staging
}

// ---------------------------------------------------------------------------
// SingleHeader
// ---------------------------------------------------------------------------

/// A `SingleHeader` policy routes exactly the requests that carry the header AND the value it
/// names; every other request is served by the distribution the viewer addressed.
#[tokio::test]
async fn a_single_header_policy_routes_the_requests_carrying_the_header_it_names() {
    let emulator = Emulator::new();
    let control = given_origins(&emulator).await;
    let primary_domain = deployment(
        &emulator,
        "a single-header continuous deployment",
        &control,
        "single-header",
        true,
        Traffic::SingleHeader {
            header: ROUTING_HEADER.to_string(),
            value: ROUTING_VALUE.to_string(),
        },
    )
    .await;

    // (case, the viewer path, the headers the viewer carries, which distribution serves it)
    let cases: [HeaderCase<'_>; 8] = [
        ("no header at all", "/index.html", &[], ServedBy::Primary),
        (
            "the configured header and value",
            "/index.html",
            &[(ROUTING_HEADER, ROUTING_VALUE)],
            ServedBy::Staging,
        ),
        (
            "the configured header with another value",
            "/index.html",
            &[(ROUTING_HEADER, "not-staged")],
            ServedBy::Primary,
        ),
        (
            "the configured header with an empty value",
            "/index.html",
            &[(ROUTING_HEADER, "")],
            ServedBy::Primary,
        ),
        (
            "the configured value under another header of the same family",
            "/index.html",
            &[(OTHER_ROUTING_HEADER, ROUTING_VALUE)],
            ServedBy::Primary,
        ),
        (
            "the configured value in another case, a header value being case-sensitive",
            "/index.html",
            &[(ROUTING_HEADER, "STAGED")],
            ServedBy::Primary,
        ),
        (
            "the configured header and value on a nested path",
            "/deep/page.html",
            &[(ROUTING_HEADER, ROUTING_VALUE)],
            ServedBy::Staging,
        ),
        (
            "no header on a nested path",
            "/deep/page.html",
            &[],
            ServedBy::Primary,
        ),
    ];

    for (case, path, headers, expected) in cases {
        let actual = served_by(&emulator.router, case, &primary_domain, path, headers).await;
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the request to be served by {expected:?}, got {actual:?}"
        );
    }
}

/// A policy that routes nothing — one that is not enabled, and one that names no
/// `TrafficConfig` at all — leaves every request with the distribution the viewer addressed,
/// header or no header.
#[tokio::test]
async fn a_policy_that_routes_nothing_leaves_every_request_with_the_primary() {
    let emulator = Emulator::new();
    let control = given_origins(&emulator).await;

    // (case, the label its distributions are created under, whether the policy is enabled, its
    // traffic configuration)
    let rows: [(&str, &str, bool, Traffic); 3] = [
        (
            "a policy that is not enabled",
            "disabled",
            false,
            Traffic::SingleHeader {
                header: ROUTING_HEADER.to_string(),
                value: ROUTING_VALUE.to_string(),
            },
        ),
        (
            "a policy that names no traffic configuration",
            "no-traffic",
            true,
            Traffic::None,
        ),
        (
            "a policy whose weight sends nothing",
            "zero-weight",
            true,
            Traffic::SingleWeight {
                weight: MIN_TRAFFIC_WEIGHT,
                stickiness: None,
            },
        ),
    ];

    for (case, label, enabled, traffic) in rows {
        let primary_domain = deployment(&emulator, case, &control, label, enabled, traffic).await;
        for headers in [&[][..], &[(ROUTING_HEADER, ROUTING_VALUE)][..]] {
            let actual =
                served_by(&emulator.router, case, &primary_domain, "/index.html", headers).await;
            assert_eq!(
                actual,
                ServedBy::Primary,
                "case {case:?} (headers {headers:?}): expected the addressed distribution to \
                 serve the request, got {actual:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// SingleWeight
// ---------------------------------------------------------------------------

/// How many viewer requests each weighted row sends. Small enough to stay quick, large enough
/// that the share the smallest published weight names is a whole request.
const WEIGHTED_REQUESTS: u32 = 20;

/// A `SingleWeight` policy sends the share of the requests it covers that its `Weight` names to
/// the staging distribution, and the rest to the primary.
#[tokio::test]
async fn a_single_weight_policy_sends_the_share_of_traffic_its_weight_names_to_staging() {
    let emulator = Emulator::new();
    let control = given_origins(&emulator).await;

    // (case, the label, the weight) — the two published bounds of "a decimal number between 0
    // and 0.15", and one inside them.
    let rows: [(&str, &str, f32); 3] = [
        ("the smallest published weight", "weight-min", MIN_TRAFFIC_WEIGHT),
        ("a weight inside the published range", "weight-mid", 0.05),
        ("the largest published weight", "weight-max", MAX_TRAFFIC_WEIGHT),
    ];

    for (case, label, weight) in rows {
        let primary_domain = deployment(
            &emulator,
            case,
            &control,
            label,
            true,
            Traffic::SingleWeight {
                weight,
                stickiness: None,
            },
        )
        .await;
        let mut staged = 0u32;
        for _ in 0..WEIGHTED_REQUESTS {
            if served_by(&emulator.router, case, &primary_domain, "/index.html", &[]).await
                == ServedBy::Staging
            {
                staged += 1;
            }
        }
        let expected = f64::from(weight) * f64::from(WEIGHTED_REQUESTS);
        let difference = (f64::from(staged) - expected).abs();
        assert!(
            difference <= 1.0,
            "case {case:?}: expected about {expected} of {WEIGHTED_REQUESTS} requests to be \
             served by the staging distribution at weight {weight}, got {staged}"
        );
    }
}

/// SESSION STICKINESS holds one viewer to one distribution: every request of a viewer whose
/// session is open is served by the side that session opened on, while the same weight without
/// stickiness splits that viewer's own requests across both distributions.
#[tokio::test]
async fn session_stickiness_holds_one_viewer_to_the_distribution_its_session_opened_on() {
    let emulator = Emulator::new();
    let control = given_origins(&emulator).await;

    // (case, the label, the session stickiness the weight configuration names, whether one
    // viewer's requests all land on one distribution)
    let rows: [StickinessRow<'_>; 2] = [
        (
            "a weight configuration naming session stickiness",
            "sticky",
            Some((MIN_SESSION_STICKINESS_TTL, MIN_SESSION_STICKINESS_TTL)),
            true,
        ),
        (
            "the same weight with no session stickiness",
            "unsticky",
            None,
            false,
        ),
    ];

    for (case, label, stickiness, one_side) in rows {
        let primary_domain = deployment(
            &emulator,
            case,
            &control,
            label,
            true,
            Traffic::SingleWeight {
                weight: MAX_TRAFFIC_WEIGHT,
                stickiness,
            },
        )
        .await;
        // ONE viewer: every request states the same address, which is what makes these
        // requests one viewer's session.
        let headers: &[(&str, &str)] = &[("x-forwarded-for", "203.0.113.7")];
        let mut sides = ::std::collections::BTreeSet::new();
        for _ in 0..WEIGHTED_REQUESTS {
            sides.insert(
                served_by(&emulator.router, case, &primary_domain, "/index.html", headers).await,
            );
        }
        assert_eq!(
            sides.len() == 1,
            one_side,
            "case {case:?}: expected one viewer's {WEIGHTED_REQUESTS} requests to be served by \
             {} distribution, got {sides:?}",
            if one_side { "one" } else { "either" }
        );
    }
}
