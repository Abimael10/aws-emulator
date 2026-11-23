// SPDX-License-Identifier: CC0-1.0

//! The origin a stack UPDATE repoints a distribution to is the origin its EDGE then fetches
//! from, end to end across the three services on one engine.
//!
//! RULE: a stack update applies the changed declaration to the LIVE resource — an
//! `AWS::CloudFront::Distribution` whose origin `DomainName` is resolved from a stack
//! parameter is updated in place when that parameter changes — so what a viewer is served
//! afterwards comes from the origin the revised declaration names. This is the blue/green
//! origin cutover a client performs by template, and only a cross-service suite can pin it:
//! CloudFormation renders and sends the configuration, CloudFront holds it and fetches the
//! origin, and Amazon S3 is what answers that fetch.
//!
//! The declaration is [`super::cloudformation_cloudfront_handler_supplied_config_members`]'s
//! repointing template, reused rather than repeated — that suite states what the stack and
//! CloudFront must then HOLD, and this one what the edge SERVES — and the viewer and origin
//! halves of the harness are [`super::cloudfront_oac_s3_origin_edge`]'s, so what is new here
//! is the cutover, not the way the services are reached.
//!
//! Each bucket holds an object of its OWN key, and the object each viewer request asks for is
//! one no earlier request asked for: an answer the edge has already cached is held under the
//! behaviour's TTL, so a request for a key the edge has never fetched is what says which
//! origin it is fetching from NOW.

use bytes::Bytes;

use super::cloudformation_cloudfront_handler_supplied_config_members::{
    cloudformation, repointed_origin_template, resource_reasons, router_with_authentication,
    settled_stack, ACTIVE_BUCKET_PARAMETER, BUCKET_A, BUCKET_B, COMMENT_ON_CREATE,
    COMMENT_PARAMETER, DISTRIBUTION_LOGICAL_ID,
};
use super::cloudfront_oac_s3_origin_edge::{
    awaited_deployment, call, policy, put_bucket_policy, put_object, view, API_VERSION,
    CLOUDFRONT_HOST, CLOUDFRONT_PRINCIPAL,
};
use super::query_wire::{opt_element, physical_ids_of};

/// The object each bucket holds of its own, and the bytes it holds there: the key says which
/// bucket served the answer, and the body says it again.
const BUCKET_A_KEY: &str = "only-in-a.html";
const BUCKET_B_KEY: &str = "only-in-b.html";
const BUCKET_A_BODY: &str = "the blue estate";
const BUCKET_B_BODY: &str = "the green estate";

/// RULE: the viewer is served the object of the bucket the stack's CURRENT declaration names
/// — bucket A's object while the stack names bucket A, and bucket B's once an update has
/// repointed the origin at bucket B.
#[tokio::test]
async fn the_edge_fetches_from_the_origin_a_stack_update_repointed_it_to() {
    let case = "an S3 origin repointed by a stack parameter";
    let router = router_with_authentication(false);
    let stack = "cloudfront-origin-cutover";
    let bucket_a = "cloudfront-origin-cutover-a";
    let bucket_b = "cloudfront-origin-cutover-b";
    let template = repointed_origin_template(bucket_a, bucket_b);

    let (status, created) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template),
            ("Parameters.member.1.ParameterKey", ACTIVE_BUCKET_PARAMETER),
            ("Parameters.member.1.ParameterValue", BUCKET_A),
            ("Parameters.member.2.ParameterKey", COMMENT_PARAMETER),
            ("Parameters.member.2.ParameterValue", COMMENT_ON_CREATE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
    );
    let described = settled_stack(&router, stack, case).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {:?} \u{2014} {}",
        opt_element(&described, "StackStatus"),
        resource_reasons(&router, stack).await
    );

    let (_, resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    let distribution = physical_ids_of(&resources)
        .get(DISTRIBUTION_LOGICAL_ID)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {DISTRIBUTION_LOGICAL_ID} in {resources}"));

    // Each bucket holds its own object, and each grants the CloudFront service principal the
    // read the edge's signed origin fetch makes — the grant an origin access control's fetch
    // is answered under.
    for (bucket, key, body) in [
        (bucket_a, BUCKET_A_KEY, BUCKET_A_BODY),
        (bucket_b, BUCKET_B_KEY, BUCKET_B_BODY),
    ] {
        put_object(&router, case, bucket, key, body, &[]).await;
        put_bucket_policy(
            &router,
            case,
            bucket,
            &policy(
                CLOUDFRONT_PRINCIPAL,
                "s3:GetObject",
                &format!("arn:aws:s3:::{bucket}/*"),
                None,
            ),
        )
        .await;
    }

    let domain = published_domain(&router, case, &distribution).await;
    awaited_deployment(&router, case, &distribution).await;

    let served = view(&router, &domain, &format!("/{BUCKET_A_KEY}")).await;
    assert_eq!(
        (served.status, served.text()),
        (200, BUCKET_A_BODY.to_string()),
        "case {case:?}: expected the edge to serve bucket A's object while the stack names \
         bucket A, got {} with {:?}",
        served.status,
        served.text()
    );

    // The cutover: the same template, with the parameter the origin's domain name is
    // resolved from moved onto the other bucket.
    let (status, updated) = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", stack),
            ("UsePreviousTemplate", "true"),
            ("Parameters.member.1.ParameterKey", ACTIVE_BUCKET_PARAMETER),
            ("Parameters.member.1.ParameterValue", BUCKET_B),
            ("Parameters.member.2.ParameterKey", COMMENT_PARAMETER),
            ("Parameters.member.2.ParameterValue", COMMENT_ON_CREATE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {updated}"
    );
    let described = settled_stack(&router, stack, case).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("UPDATE_COMPLETE"),
        "case {case:?}: expected the stack to reach UPDATE_COMPLETE, got {:?} \u{2014} {}",
        opt_element(&described, "StackStatus"),
        resource_reasons(&router, stack).await
    );

    // The distribution keeps the address it published, which is what makes this a cutover
    // rather than a replacement: the same viewer keeps using the same domain name.
    let after = published_domain(&router, case, &distribution).await;
    assert_eq!(
        after, domain,
        "case {case:?}: expected the updated distribution to keep its published domain name"
    );
    awaited_deployment(&router, case, &distribution).await;

    let served = view(&router, &domain, &format!("/{BUCKET_B_KEY}")).await;
    assert_eq!(
        (served.status, served.text()),
        (200, BUCKET_B_BODY.to_string()),
        "case {case:?}: expected the edge to serve bucket B's object once the update \
         repointed the origin, got {} with {:?}",
        served.status,
        served.text()
    );
}

/// The `DomainName` CloudFront publishes for the distribution: the address a viewer is handed
/// and dials, read back through that service's own `GetDistribution`.
async fn published_domain(
    router: &::smithy_server_runtime::MultiServiceRouter,
    case: &str,
    distribution: &str,
) -> String {
    let read = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "GET",
        &format!("/{API_VERSION}/distribution/{distribution}"),
        &[],
        Bytes::new(),
    )
    .await;
    let domain = read.element("DomainName");
    assert!(
        !domain.is_empty(),
        "case {case:?}: the distribution publishes no domain name: {:?}",
        read.text()
    );
    domain
}
