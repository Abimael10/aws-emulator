// SPDX-License-Identifier: CC0-1.0

//! The SPA REWRITE a CloudFront function performs, observed by DIALLING the `DomainName` the
//! distribution published — on a real socket, with no credential of any kind — over a private
//! Amazon S3 origin.
//!
//! RULE: **the value a distribution publishes as its `DomainName` is an address this emulator
//! itself answers on, and a viewer request that arrives there is served by that distribution's
//! WHOLE stored configuration** — including the CloudFront function the matched cache behaviour
//! associates on the viewer-request event, whose returned request is the one the edge keys on
//! and fetches from the origin. `Distribution$DomainName` is "the distribution's CloudFront
//! domain name. For example: `d111111abcdef8.cloudfront.net`", and the only thing a viewer does
//! with it is issue HTTP(S) to it, so a published value a client on this machine cannot open a
//! connection to leaves the distribution unreachable however correct the control plane's record
//! of it is.
//!
//! Only a cross-service suite can state this: the published address, the edge and the function
//! are CloudFront's, and the bytes the rewrite ends up serving are Amazon S3's. The CloudFront
//! crate's own `edge_function_associations` suite runs an associated function against an edge
//! addressed by a `Host` header on a dispatched request — which says nothing about whether the
//! published name is an address at all — and the sibling
//! `cloudfront_dialled_edge_address_over_a_private_s3_origin` dials the address over a
//! distribution that associates no function, so neither states that the single-page
//! application's own entry point is reachable at the name a client is handed.
//!
//! The rows differ along what the rule generalises over: the FORM of the viewer path the
//! function decides from (the distribution root, a single extensionless segment, a nested
//! extensionless path, an extensionless path carrying a query string, a path whose last segment
//! names an extension) and whether the origin holds what the request ends up asking for — so
//! the one dialled address is shown serving the application shell for every route the
//! application owns, serving the asset for a path the function leaves alone, and surfacing the
//! origin's own answer for one the origin holds nothing at.
//!
//! Every expected value is computed here from what the row declares. The distribution's id, ARN
//! and published address and the function's ARN are CloudFront's to mint: what is asserted about
//! the address is its FORM (a `host:port` a client can dial), that `GetDistribution` reports the
//! same one, and what it answers.

use std::sync::Arc;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The viewer's own connection, and the one reading of "is this published value an address a
// client can dial": both the sibling dialled-address suite's, dialled here over a distribution
// that associates a function.
use super::cloudfront_dialled_edge_address_over_a_private_s3_origin::{dial, is_dialable};
use super::cloudfront_oac_s3_origin_edge::{
    awaited_deployment, call, create_bucket, create_origin_access_control, edge_listener_authority,
    emulator, is_published_edge_address, origin_domain, policy, put_bucket_policy, put_object,
    API_VERSION, CLOUDFRONT_HOST, CLOUDFRONT_PRINCIPAL,
};

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every viewer
/// request under it reaches the origin, so each row is served the origin's own answer rather
/// than one the edge held from an earlier row.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The bucket the application is served from, the origin id the distribution's one origin
/// carries, and the names every resource of this suite is created under.
const BUCKET: &str = "spa-rewrite-dialled-edge";
const ORIGIN_ID: &str = "shell-origin";
const FUNCTION_NAME: &str = "spa_rewrite_dialled_edge";

/// The object the distribution names as its `DefaultRootObject` and the function rewrites to,
/// and the nested asset beside it.
const ROOT_OBJECT: &str = "index.html";
const SHELL_BODY: &str = "<!doctype html><title>the application shell</title>";
const ASSET_KEY: &str = "static/app/main.js";
const ASSET_BODY: &str = "export const mounted = true;\n";

/// A path under the same prefix the origin holds NOTHING at: the row that asks for it is served
/// the origin's own answer, which is what says the function did not rewrite it.
const ABSENT_ASSET: &str = "/static/app/absent.js";

/// The runtime the function declares (`FunctionRuntime`'s later value), and the viewer event its
/// association names (`FunctionAssociation$EventType`, "either `viewer-request` or
/// `viewer-response`").
const RUNTIME: &str = "cloudfront-js-2.0";
const VIEWER_REQUEST: &str = "viewer-request";

/// The stage a function has to be in to be associated with a cache behaviour: "when a function
/// is in the `LIVE` stage, you can attach the function to a distribution's cache behavior, using
/// the function's ARN" (`FunctionMetadata$Stage`).
const LIVE: &str = "LIVE";

/// What the edge reports as the outcome of an answer the ORIGIN produced for this request.
const MISS: &str = "Miss from cloudfront";

/// The SPA rewrite itself: a viewer-request function that sends every path whose last segment
/// names no file extension to the application's entry point, and leaves every other path as the
/// viewer wrote it.
///
/// `request.uri` is the path alone — a CloudFront function reads the query string from
/// `request.querystring` — so a route carrying one is decided by its path exactly as the bare
/// route is.
const REWRITES_EXTENSIONLESS_PATHS: &str = "function handler(event) {\n\
     \x20   var request = event.request;\n\
     \x20   var uri = request.uri;\n\
     \x20   var last = uri.substring(uri.lastIndexOf('/') + 1);\n\
     \x20   if (last.indexOf('.') === -1) {\n\
     \x20       request.uri = '/index.html';\n\
     \x20   }\n\
     \x20   return request;\n\
     }\n";

/// What one row expects the viewer to be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The bytes of one of the origin's objects, byte for byte, with the edge reporting the
    /// answer as the origin's.
    Object(&'static str),
    /// The origin's OWN answer for something it holds nothing at: a client error that is not
    /// the application shell.
    OriginMiss,
}

/// One row: the path a viewer asks the published address for, and what the rule says it is then
/// served.
struct Row {
    case: &'static str,
    path: &'static str,
    expected: Expected,
}

/// The table: one row per form of viewer path the function decides from.
const ROWS: &[Row] = &[
    Row {
        case: "the distribution root, which the application's entry point answers",
        path: "/",
        expected: Expected::Object(SHELL_BODY),
    },
    Row {
        case: "a single extensionless segment, which is a route of the application",
        path: "/dashboard",
        expected: Expected::Object(SHELL_BODY),
    },
    Row {
        case: "a nested extensionless path, which is a route of the application too",
        path: "/settings/profile",
        expected: Expected::Object(SHELL_BODY),
    },
    Row {
        case: "an extensionless route carrying a query string, decided by its path alone",
        path: "/dashboard?tab=2",
        expected: Expected::Object(SHELL_BODY),
    },
    Row {
        case: "a path whose last segment names an extension, which the function leaves alone",
        path: "/static/app/main.js",
        expected: Expected::Object(ASSET_BODY),
    },
    Row {
        case: "a path with an extension the origin holds nothing at",
        path: ABSENT_ASSET,
        expected: Expected::OriginMiss,
    },
];

/// `cloudfront:CreateFunction` for [`REWRITES_EXTENSIONLESS_PATHS`], then
/// `cloudfront:PublishFunction` — answering the ARN of the published function, which is what a
/// cache behaviour associates.
async fn published_function(router: &MultiServiceRouter, case: &str) -> String {
    let code = ::aws_smithy_types::base64::encode(REWRITES_EXTENSIONLESS_PATHS.as_bytes());
    let body = format!(
        "<CreateFunctionRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>{FUNCTION_NAME}</Name>\
         <FunctionConfig><Comment>the single-page application's rewrite</Comment>\
         <Runtime>{RUNTIME}</Runtime></FunctionConfig>\
         <FunctionCode>{code}</FunctionCode>\
         </CreateFunctionRequest>"
    );
    let created = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "POST",
        &format!("/{API_VERSION}/function"),
        &[("content-type", "application/xml")],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let etag = created.header("etag");
    assert!(
        !etag.is_empty(),
        "case {case:?}: the created function reports no ETag to publish it with: {:?}",
        created.text()
    );
    let published = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "POST",
        &format!("/{API_VERSION}/function/{FUNCTION_NAME}/publish"),
        &[("if-match", &etag)],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        published.status,
        200,
        "case {case:?}: PublishFunction must succeed, got {} with {:?}",
        published.status,
        published.text()
    );
    assert_eq!(
        published.element("Stage"),
        LIVE,
        "case {case:?}: expected the published function in the {LIVE} stage, got {:?}",
        published.text()
    );
    let arn = published.element("FunctionARN");
    assert!(
        !arn.is_empty(),
        "case {case:?}: the published function reports no FunctionARN: {:?}",
        published.text()
    );
    arn
}

/// `cloudfront:CreateDistribution` over the S3 origin `BUCKET` with `control` attached, the
/// application's entry point as its `DefaultRootObject` and `function` associated on its default
/// behaviour's viewer-request event — answering the distribution's `(id, DomainName, ARN)`.
async fn create_distribution(
    router: &MultiServiceRouter,
    case: &str,
    control: &str,
    function: &str,
) -> (String, String, String) {
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{BUCKET}-reference</CallerReference>\
         <DefaultRootObject>{ROOT_OBJECT}</DefaultRootObject>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>{ORIGIN_ID}</Id><DomainName>{}</DomainName>\
         <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
         <OriginAccessControlId>{control}</OriginAccessControlId>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>{ORIGIN_ID}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         <FunctionAssociations><Quantity>1</Quantity><Items><FunctionAssociation>\
         <FunctionARN>{function}</FunctionARN><EventType>{VIEWER_REQUEST}</EventType>\
         </FunctionAssociation></Items></FunctionAssociations>\
         </DefaultCacheBehavior>\
         <Comment>a single-page application reached at the published address</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        origin_domain(BUCKET)
    );
    let created = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        &[("content-type", "application/xml")],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateDistribution must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, domain, arn) = (
        created.element("Id"),
        created.element("DomainName"),
        created.element("ARN"),
    );
    for (member, value) in [("Id", &id), ("DomainName", &domain), ("ARN", &arn)] {
        assert!(
            !value.is_empty(),
            "case {case:?}: the created distribution reports no {member}: {:?}",
            created.text()
        );
    }
    (id, domain, arn)
}

/// The `DomainName` `GetDistribution` reports for `id`.
async fn read_domain_name(router: &MultiServiceRouter, id: &str) -> String {
    let read = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "GET",
        &format!("/{API_VERSION}/distribution/{id}"),
        &[],
        Bytes::new(),
    )
    .await;
    read.element("DomainName")
}

/// Every row: the published address, dialled on a socket, serves the application the
/// distribution's own configuration describes.
#[tokio::test]
async fn the_published_address_serves_every_route_the_associated_function_rewrites() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // The setup plane: authentication off, so each call carries a credential scope (which is what
    // routes it and what names the Region) without a computed signature. Every create goes
    // through it, so the listener the distribution's edge is served on hands the requests it
    // accepts back to this same emulator.
    let setup = emulator(&engine, false);
    let case = "the application's own estate";

    create_bucket(&setup, case, BUCKET).await;
    put_object(&setup, case, BUCKET, ROOT_OBJECT, SHELL_BODY, &[]).await;
    put_object(&setup, case, BUCKET, ASSET_KEY, ASSET_BODY, &[]).await;

    let control = create_origin_access_control(&setup, case, FUNCTION_NAME, "always").await;
    let function = published_function(&setup, case).await;
    let (id, domain, arn) = create_distribution(&setup, case, &control, &function).await;

    // The published value is the address a client on this machine reaches the edge at — or there
    // is nothing for a viewer to dial at all.
    assert!(
        is_published_edge_address(&domain),
        "case {case:?}: expected an address this emulator answers on (host:port) as the published \
         DomainName, got {domain:?}"
    );
    let address = edge_listener_authority(&engine, &id).unwrap_or_else(|| {
        panic!("case {case:?}: this emulator opened no address for the distribution {id}")
    });
    assert!(
        is_dialable(&address),
        "case {case:?}: expected a host:port a client on this machine can dial, got {address:?}"
    );
    assert_eq!(
        address, domain,
        "case {case:?}: expected the published DomainName to be the address this emulator opened \
         ({address:?}), got {domain:?}"
    );

    // The document is written AFTER the distribution exists, because it is scoped to that
    // distribution's own ARN — the order a client has to follow.
    put_bucket_policy(
        &setup,
        case,
        BUCKET,
        &policy(
            CLOUDFRONT_PRINCIPAL,
            "s3:GetObject",
            &format!("arn:aws:s3:::{BUCKET}/*"),
            Some(&format!(
                "{{\"StringEquals\":{{\"AWS:SourceArn\":\"{arn}\"}}}}"
            )),
        ),
    )
    .await;

    // A configuration that has not reached the edge locations serves no viewer.
    awaited_deployment(&setup, case, &id).await;
    let read_back = read_domain_name(&setup, &id).await;
    assert_eq!(
        read_back, domain,
        "case {case:?}: expected GetDistribution to report the published name {domain:?}, got \
         {read_back:?}"
    );

    for row in ROWS {
        let case = row.case;
        let answer = dial(case, &address, &domain, row.path).await;
        // Whatever the answer is, it is the edge's: CloudFront identifies the request it served,
        // which is what says the dialled address is this distribution's data plane and not some
        // other listener that happened to accept the connection.
        assert!(
            !answer.header("x-amz-cf-id").is_empty(),
            "case {case:?}: expected the edge's own X-Amz-Cf-Id on the answer, got {:?}",
            answer.raw
        );
        match row.expected {
            Expected::Object(bytes) => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected the origin's object at the published address, got {} \
                     with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.body, bytes,
                    "case {case:?}: expected the origin's own bytes {bytes:?}, got {:?}",
                    answer.body
                );
                assert_eq!(
                    answer.header("x-cache"),
                    MISS,
                    "case {case:?}: expected the answer to be the origin's, got X-Cache {:?}",
                    answer.header("x-cache")
                );
            }
            Expected::OriginMiss => {
                assert!(
                    (400..500).contains(&answer.status),
                    "case {case:?}: expected the origin's own client error for {ABSENT_ASSET:?}, \
                     got {} with {:?}",
                    answer.status,
                    answer.body
                );
                assert!(
                    !answer.body.contains(SHELL_BODY),
                    "case {case:?}: expected no rewrite to the application's entry point, got \
                     {:?}",
                    answer.body
                );
            }
        }
    }
}
