// SPDX-License-Identifier: CC0-1.0

//! A distribution whose origin is an Amazon S3 WEBSITE ENDPOINT, end to end over HTTP with
//! Amazon S3 mounted beside CloudFront on one engine.
//!
//! RULE: the edge makes its origin request over the scheme the origin's own
//! `OriginProtocolPolicy` names, and that request reaches the origin exactly when the
//! endpoint it addresses SERVES that scheme — an Amazon S3 website endpoint serves plain
//! HTTP and no HTTPS at all, so a `http-only` origin over such an endpoint is served
//! whatever that endpoint answers for the viewer's path (the addressed directory's index
//! document, an object's bytes, the error document under the status the request earned), and
//! one addressed over TLS is the `502` an origin CloudFront could not connect to is reported
//! with.
//!
//! Only a suite that mounts the object store can state this: the website endpoint is a
//! surface of Amazon S3's own, so the bytes a viewer receives here are the ones a client
//! really put in a bucket whose website configuration a client really wrote, read back
//! through the endpoint S3 serves them on.
//!
//! The rows span what the rule generalises over: the FORM of the website host (the older
//! hyphenated `<bucket>.s3-website-<Region>.amazonaws.com` and the dotted
//! `<bucket>.s3-website.<Region>.amazonaws.com`, both of which Amazon S3 publishes), the
//! viewer path (the root, a nested directory, an object, a key that is not there, a key the
//! bucket keeps private), the origin's protocol policy (`http-only`, `https-only`,
//! `match-viewer`) and the scheme the viewer itself dialled, which is what `match-viewer`
//! reads.

use std::sync::Arc;

use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_signed_access::{view, Viewed, CACHING_DISABLED, ERROR, MISS};
use crate::edge_signed_s3_origin::mounted;

/// The Region the origin bucket is created in, and so the Region its website endpoint host
/// addresses.
const REGION: &str = "us-east-1";

/// The bucket every row below serves from: one static site, so what the rows differ in is
/// how the distribution addresses it and what the viewer asks for.
const BUCKET: &str = "cloudfront-website-origin";

/// The suffix the bucket's `IndexDocument` names — "a suffix that is appended to a request
/// that is for a directory on the website endpoint".
const INDEX_SUFFIX: &str = "index.html";

/// The key the bucket's `ErrorDocument` names — "the object key name to use when a 4XX class
/// error occurs".
const ERROR_KEY: &str = "error.html";

/// The media type the site's objects are stored under, and so the one the website endpoint
/// serves them with.
const HTML: &str = "text/html";

/// The bodies of the objects the site holds.
const INDEX_BODY: &str = "<html>the site index</html>";
/// See [`INDEX_BODY`].
const SUB_INDEX_BODY: &str = "<html>the nested index</html>";
/// See [`INDEX_BODY`].
const PAGE_BODY: &str = "<html>a page of the site</html>";
/// See [`INDEX_BODY`].
const ERROR_BODY: &str = "<html>the site error page</html>";
/// See [`INDEX_BODY`].
const PRIVATE_BODY: &str = "<html>not published</html>";

/// The one key the bucket's policy keeps private, so a row can state what the ORIGIN refuses.
const PRIVATE_KEY: &str = "private.html";

/// The sentence CloudFront's own `502` carries for an origin it could not connect to.
const NO_ORIGIN_DETAIL: &str = "CloudFront wasn't able to connect to the origin.";

/// The `OriginProtocolPolicy` values a row addresses its origin with: "`http-only` –
/// CloudFront uses only HTTP to access the origin", `https-only` only HTTPS, and
/// `match-viewer` the protocol of the viewer request.
const HTTP_ONLY: &str = "http-only";
/// See [`HTTP_ONLY`].
const HTTPS_ONLY: &str = "https-only";
/// See [`HTTP_ONLY`].
const MATCH_VIEWER: &str = "match-viewer";

// ---------------------------------------------------------------------------
// The emulator: CloudFront plus the object store the website endpoint is a surface of
// ---------------------------------------------------------------------------

/// CloudFront and Amazon S3 over ONE shared engine: the object store is where the website
/// surface this suite's origins address lives, so the bytes a viewer receives are the ones a
/// client really put in a bucket whose website configuration a client really wrote. Mounted
/// through the one two-service constructor this target has
/// ([`crate::edge_signed_s3_origin::mounted`]); a viewer presents no credential, so this
/// suite's plane is the unauthenticated one.
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

    /// The Amazon S3 client, built by the one shared constructor over the in-process
    /// callback, so the site this suite stages is the one the edge's origin fetch reads.
    fn s3(&self) -> ::aws_sdk_s3::Client {
        service_common::s3_client::s3_client(&self.callback, REGION)
    }

    /// Stand up the static site: the objects, the public-access settings a published bucket
    /// needs, the policy that makes all but [`PRIVATE_KEY`] public, and the website
    /// configuration naming the index and error documents.
    async fn staged_site(&self) {
        let s3 = self.s3();
        s3.create_bucket()
            .bucket(BUCKET)
            .send()
            .await
            .unwrap_or_else(|error| panic!("creating {BUCKET} must succeed, got {error:?}"));
        for (key, body) in [
            (INDEX_SUFFIX, INDEX_BODY),
            ("sub/index.html", SUB_INDEX_BODY),
            ("page.html", PAGE_BODY),
            (ERROR_KEY, ERROR_BODY),
            (PRIVATE_KEY, PRIVATE_BODY),
        ] {
            s3.put_object()
                .bucket(BUCKET)
                .key(key)
                .content_type(HTML)
                .body(::aws_sdk_s3::primitives::ByteStream::from(
                    body.as_bytes().to_vec(),
                ))
                .send()
                .await
                .unwrap_or_else(|error| panic!("putting {BUCKET}/{key} must succeed, got {error:?}"));
        }
        // A public bucket policy is prevented by the bucket's own block-public-access
        // settings until they are turned off, which is the step a published site takes.
        s3.put_public_access_block()
            .bucket(BUCKET)
            .public_access_block_configuration(
                ::aws_sdk_s3::types::PublicAccessBlockConfiguration::builder()
                    .block_public_acls(false)
                    .ignore_public_acls(false)
                    .block_public_policy(false)
                    .restrict_public_buckets(false)
                    .build(),
            )
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("the public access settings of {BUCKET} must be stored, got {error:?}")
            });
        s3.put_bucket_policy()
            .bucket(BUCKET)
            .policy(site_policy())
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("the policy of {BUCKET} must be stored, got {error:?}")
            });
        let website = ::aws_sdk_s3::types::WebsiteConfiguration::builder()
            .index_document(
                ::aws_sdk_s3::types::IndexDocument::builder()
                    .suffix(INDEX_SUFFIX)
                    .build()
                    .expect("the index document names its suffix"),
            )
            .error_document(
                ::aws_sdk_s3::types::ErrorDocument::builder()
                    .key(ERROR_KEY)
                    .build()
                    .expect("the error document names its key"),
            )
            .build();
        s3.put_bucket_website()
            .bucket(BUCKET)
            .website_configuration(website)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("the website configuration of {BUCKET} must be stored, got {error:?}")
            });
    }
}

/// The policy that publishes the site: `s3:GetObject` on every object to every principal,
/// with [`PRIVATE_KEY`] held back by an explicit `Deny` — so the table spans both directions
/// of the decision the ORIGIN makes about a website read.
fn site_policy() -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[\
         {{\"Sid\":\"PublicRead\",\"Effect\":\"Allow\",\"Principal\":\"*\",\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{BUCKET}/*\"}},\
         {{\"Sid\":\"KeepOnePrivate\",\"Effect\":\"Deny\",\"Principal\":\"*\",\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{BUCKET}/{PRIVATE_KEY}\"}}]}}"
    )
}

/// Which spelling of the website endpoint a distribution addresses its origin at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostForm {
    /// The older form, whose Region is inside the endpoint label
    /// (`<bucket>.s3-website-<Region>.amazonaws.com`).
    Hyphenated,
    /// The dotted form (`<bucket>.s3-website.<Region>.amazonaws.com`).
    Dotted,
}

impl HostForm {
    /// The `Origin$DomainName` this form spells for [`BUCKET`].
    fn domain_name(self) -> String {
        match self {
            Self::Hyphenated => format!("{BUCKET}.s3-website-{REGION}.amazonaws.com"),
            Self::Dotted => format!("{BUCKET}.s3-website.{REGION}.amazonaws.com"),
        }
    }
}

// ---------------------------------------------------------------------------
// Staging the distribution
// ---------------------------------------------------------------------------

/// `CreateDistribution` over the website endpoint `host` as a CUSTOM origin — which is what a
/// website-endpoint origin is, since it is not the bucket's REST endpoint — whose protocol
/// policy is `policy`; answers the distribution's `(Id, DomainName)`.
async fn create_distribution(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    host: HostForm,
    policy: &str,
) -> (String, String) {
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{caller_reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items><Origin>\
             <Id>website-origin</Id><DomainName>{}</DomainName>\
             <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
             <OriginProtocolPolicy>{policy}</OriginProtocolPolicy></CustomOriginConfig>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>website-origin</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             <AllowedMethods><Quantity>2</Quantity>\
             <Items><Method>GET</Method><Method>HEAD</Method></Items></AllowedMethods>\
             <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
             </DefaultCacheBehavior>\
             <Comment>a static site served from an s3 website endpoint</Comment>\
             <Enabled>true</Enabled>\
             </DistributionConfig>",
            host.domain_name()
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateDistribution answered {}: {}",
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

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// Which scheme the VIEWER dialled, which is the only thing `match-viewer` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialled {
    /// A plaintext viewer, which addresses the distribution with an absolute `http` URI.
    Plaintext,
    /// A viewer over TLS, which is what an origin-form request to this emulator's own
    /// endpoints is.
    Tls,
}

/// One row: a distribution over the website endpoint spelled `host` with the protocol policy
/// `policy`, and one viewer request to it.
struct Row {
    case: &'static str,
    host: HostForm,
    policy: &'static str,
    dialled: Dialled,
    /// The path the viewer asks for.
    path: &'static str,
    status: u16,
    /// A fragment the answer's body must carry.
    body: &'static str,
    /// The `X-Cache` the answer must carry, where the row pins one.
    x_cache: Option<&'static str>,
}

/// The table: one row per host form, viewer path, protocol policy and dialled scheme.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the root of the site, over http-only",
            host: HostForm::Hyphenated,
            policy: HTTP_ONLY,
            dialled: Dialled::Tls,
            path: "/",
            status: 200,
            body: INDEX_BODY,
            x_cache: Some(MISS),
        },
        Row {
            case: "a nested directory, whose own index document is served",
            host: HostForm::Hyphenated,
            policy: HTTP_ONLY,
            dialled: Dialled::Tls,
            path: "/sub/",
            status: 200,
            body: SUB_INDEX_BODY,
            x_cache: Some(MISS),
        },
        Row {
            case: "an object of the site, addressed by key",
            host: HostForm::Hyphenated,
            policy: HTTP_ONLY,
            dialled: Dialled::Tls,
            path: "/page.html",
            status: 200,
            body: PAGE_BODY,
            x_cache: Some(MISS),
        },
        Row {
            case: "a key the site does not hold, which is the error document under 404",
            host: HostForm::Hyphenated,
            policy: HTTP_ONLY,
            dialled: Dialled::Tls,
            path: "/missing.html",
            status: 404,
            body: ERROR_BODY,
            x_cache: None,
        },
        Row {
            case: "a key the bucket keeps private, which the ORIGIN refuses",
            host: HostForm::Hyphenated,
            policy: HTTP_ONLY,
            dialled: Dialled::Tls,
            path: "/private.html",
            status: 403,
            body: ERROR_BODY,
            x_cache: None,
        },
        Row {
            case: "the dotted spelling of the same website endpoint",
            host: HostForm::Dotted,
            policy: HTTP_ONLY,
            dialled: Dialled::Tls,
            path: "/",
            status: 200,
            body: INDEX_BODY,
            x_cache: Some(MISS),
        },
        Row {
            case: "match-viewer with a plaintext viewer, which addresses the origin over http",
            host: HostForm::Hyphenated,
            policy: MATCH_VIEWER,
            dialled: Dialled::Plaintext,
            path: "/",
            status: 200,
            body: INDEX_BODY,
            x_cache: Some(MISS),
        },
        Row {
            case: "https-only, a scheme the website endpoint does not serve",
            host: HostForm::Hyphenated,
            policy: HTTPS_ONLY,
            dialled: Dialled::Tls,
            path: "/",
            status: 502,
            body: NO_ORIGIN_DETAIL,
            x_cache: Some(ERROR),
        },
        Row {
            case: "match-viewer with a viewer over TLS, which addresses the origin over https",
            host: HostForm::Hyphenated,
            policy: MATCH_VIEWER,
            dialled: Dialled::Tls,
            path: "/",
            status: 502,
            body: NO_ORIGIN_DETAIL,
            x_cache: Some(ERROR),
        },
    ]
}

/// Send the row's viewer request to `domain`: a plaintext viewer addresses the distribution
/// with an absolute `http` URI, and one over TLS with the origin-form request this
/// emulator's own TLS endpoints are reached with.
async fn viewed(router: &MultiServiceRouter, row: &Row, domain: &str) -> Viewed {
    let addressed = match row.dialled {
        Dialled::Plaintext => format!("http://{domain}{}", row.path),
        Dialled::Tls => row.path.to_string(),
    };
    view(router, domain, &addressed, &[]).await
}

/// The whole rule, over the table.
#[tokio::test]
async fn an_s3_website_endpoint_origin_is_served_over_the_scheme_that_endpoint_serves() {
    let emulator = Emulator::new();
    emulator.staged_site().await;

    for (index, row) in rows().into_iter().enumerate() {
        let (id, domain) = create_distribution(
            &emulator.router,
            row.case,
            &format!("website-origin-{index}"),
            row.host,
            row.policy,
        )
        .await;
        // A configuration that has not reached the edge locations serves no viewer, and the
        // CLOCK owes that propagation: the condition is polled for, never a span waited out.
        awaited_deployment(&emulator.router, row.case, &id).await;

        let answer = viewed(&emulator.router, &row, &domain).await;
        assert_eq!(
            answer.status, row.status,
            "case {:?}: expected {} from the distribution, got {} (body {:?})",
            row.case, row.status, answer.status, answer.body
        );
        assert!(
            answer.body.contains(row.body),
            "case {:?}: expected an answer carrying {:?}, got {:?}",
            row.case,
            row.body,
            answer.body
        );
        if let Some(x_cache) = row.x_cache {
            assert_eq!(
                answer.x_cache, x_cache,
                "case {:?}: expected X-Cache {x_cache:?}, got {:?}",
                row.case, answer.x_cache
            );
        }
        // Nothing a refused or unreachable origin answers carries the site's bytes, and
        // nothing the site serves carries the private object's.
        assert!(
            !answer.body.contains(PRIVATE_BODY),
            "case {:?}: the answer carries the private object's bytes: {:?}",
            row.case,
            answer.body
        );
    }
}
