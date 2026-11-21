// SPDX-License-Identifier: CC0-1.0

//! An Amazon S3 WEBSITE ENDPOINT origin is served to a viewer whichever way this emulator's
//! own front door is configured — with request authentication ON as well as off — and what it
//! serves is what the bucket makes PUBLIC either way.
//!
//! RULE: an origin request is an IN-PROCESS call this emulator makes to another of its own
//! services, and the emulator verifies every request it serves, so a leg carrying no credential
//! at all is refused by its own front door before the origin's surface decides anything. The
//! edge therefore makes the leg to the one origin surface whose answer that identity cannot
//! change — an Amazon S3 website endpoint, which serves what the bucket makes public and
//! nothing else, to whoever asks — as the in-process call it is, and the viewer receives what
//! that surface answers: the index document of the addressed directory for public content, and
//! the bucket's own refusal for content it does not publish.
//!
//! Only a suite that mounts the object store AND drives the viewer through an authenticating
//! plane can state this. The sibling `edge_website_endpoint_origin` suite pins WHAT the website
//! surface answers for each viewer path and origin protocol policy, over a plane that
//! authenticates nothing — which cannot distinguish a leg the front door would have refused
//! from one it serves; `edge_signed_s3_origin` drives an authenticating plane, but over an
//! origin whose fetch an origin access control signs for a principal of its own.
//!
//! The rows differ along what the rule generalises over: whether the plane serving the request
//! authenticates, and what the bucket publishes about the object the viewer asks for (a public
//! object, the index document of a public directory, an object held back by an explicit deny,
//! and an object the bucket's policy says nothing at all about — which this emulator's own
//! credential WOULD be served through the REST API) — so the identity on the leg is shown to
//! decide whether the leg is SERVED, and the bucket's own grants to decide WHAT it serves.

use std::sync::Arc;

use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_signed_access::{view, CACHING_DISABLED, MISS};
use crate::edge_signed_s3_origin::mounted;

/// The Region the bucket is created in, and so the Region its website endpoint host addresses.
const REGION: &str = "us-east-1";

/// The bucket the static site lives in.
const BUCKET: &str = "edge-website-origin-auth";

/// The suffix the bucket's `IndexDocument` names.
const INDEX_SUFFIX: &str = "index.html";

/// The objects of the site, and what each is for.
const INDEX_BODY: &str = "<html>the published index</html>";
/// See [`INDEX_BODY`].
const NESTED_BODY: &str = "<html>the published nested index</html>";
/// See [`INDEX_BODY`].
const PRIVATE_KEY: &str = "held-back.html";
/// See [`INDEX_BODY`].
const PRIVATE_BODY: &str = "<html>never published</html>";
/// The object the site's policy says NOTHING about: its key is covered by neither grant the
/// policy states, so nothing makes it public and nothing denies it either — which is the case
/// that tells a surface serving only what is PUBLIC from one the identity on the leg widens,
/// since this emulator's own credential would be served such an object by the REST API.
const UNPUBLISHED_KEY: &str = "unlisted.txt";
/// See [`UNPUBLISHED_KEY`].
const UNPUBLISHED_BODY: &str = "<html>granted to nobody</html>";

/// The media type the site's objects are stored under.
const HTML: &str = "text/html";

/// The code Amazon S3 refuses a read nothing granted with, which a website endpoint reports in
/// the HTML page it answers an error with.
const ACCESS_DENIED: &str = "AccessDenied";

/// CloudFront and Amazon S3 over ONE shared engine, as two planes: one that AUTHENTICATES
/// every request it serves and one that does not, over the same records — so a row's only
/// difference is which plane served it.
struct Emulator {
    setup: MultiServiceRouter,
    authenticating: MultiServiceRouter,
    callback: EmbeddedEmulator,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let setup = mounted(&engine, false);
        let authenticating = mounted(&engine, true);
        let callback = EmbeddedEmulator::from_router(setup.clone());
        Self {
            setup,
            authenticating,
            callback,
        }
    }

    /// The Amazon S3 client, built by the one shared constructor over the in-process callback.
    fn s3(&self) -> ::aws_sdk_s3::Client {
        service_common::s3_client::s3_client(&self.callback, REGION)
    }

    /// Stand up the static site: its objects, the public-access settings a published bucket
    /// needs, the policy that publishes all but [`PRIVATE_KEY`], and the website configuration.
    async fn staged_site(&self) {
        let s3 = self.s3();
        s3.create_bucket()
            .bucket(BUCKET)
            .send()
            .await
            .unwrap_or_else(|error| panic!("creating {BUCKET} must succeed, got {error:?}"));
        for (key, body) in [
            (INDEX_SUFFIX, INDEX_BODY),
            ("pages/index.html", NESTED_BODY),
            (PRIVATE_KEY, PRIVATE_BODY),
            (UNPUBLISHED_KEY, UNPUBLISHED_BODY),
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
                .unwrap_or_else(|error| panic!("putting {key} must succeed, got {error:?}"));
        }
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
            .policy(format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[\
                 {{\"Sid\":\"PublicRead\",\"Effect\":\"Allow\",\"Principal\":\"*\",\
                 \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{BUCKET}/*.html\"}},\
                 {{\"Sid\":\"PublishTheNestedIndex\",\"Effect\":\"Allow\",\
                 \"Principal\":\"*\",\"Action\":\"s3:GetObject\",\
                 \"Resource\":\"arn:aws:s3:::{BUCKET}/pages/*\"}},\
                 {{\"Sid\":\"HoldOneBack\",\"Effect\":\"Deny\",\"Principal\":\"*\",\
                 \"Action\":\"s3:GetObject\",\
                 \"Resource\":\"arn:aws:s3:::{BUCKET}/{PRIVATE_KEY}\"}}]}}"
            ))
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

/// The host the bucket's WEBSITE surface is served on, which is what the origin addresses.
fn website_host() -> String {
    format!("{BUCKET}.s3-website-{REGION}.amazonaws.com")
}

/// `CreateDistribution` over that website endpoint as a CUSTOM origin addressed with HTTP Only
/// — the configuration AWS's own guidance gives a website-endpoint origin — answering the
/// distribution's `(Id, DomainName)`.
async fn create_distribution(router: &MultiServiceRouter, reference: &str) -> (String, String) {
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items><Origin>\
             <Id>website-origin</Id><DomainName>{}</DomainName>\
             <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
             <OriginProtocolPolicy>http-only</OriginProtocolPolicy></CustomOriginConfig>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>website-origin</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             <CachePolicyId>{CACHING_DISABLED}</CachePolicyId></DefaultCacheBehavior>\
             <Comment>a static site behind an authenticating front door</Comment>\
             <Enabled>true</Enabled></DistributionConfig>",
            website_host()
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "CreateDistribution answered {}: {}",
        created.status, created.raw
    );
    let read = |member: &str| {
        created
            .root()
            .find_text(member)
            .unwrap_or_else(|| panic!("the created distribution reports no {member}"))
            .to_string()
    };
    (read("Id"), read("DomainName"))
}

/// One row: which plane serves the viewer request, the path it asks for, and what the bucket
/// publishes about it.
struct Row {
    case: &'static str,
    /// Whether the plane serving the request authenticates every request it serves.
    authenticating: bool,
    path: &'static str,
    status: u16,
    /// A fragment the answer's body must carry.
    body: &'static str,
}

/// The rows.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the root of the site, on the authenticating plane",
            authenticating: true,
            path: "/",
            status: 200,
            body: INDEX_BODY,
        },
        Row {
            case: "the root of the site, on a plane that authenticates nothing",
            authenticating: false,
            path: "/",
            status: 200,
            body: INDEX_BODY,
        },
        Row {
            case: "a nested directory, on the authenticating plane",
            authenticating: true,
            path: "/pages/",
            status: 200,
            body: NESTED_BODY,
        },
        Row {
            case: "an object addressed by key, on the authenticating plane",
            authenticating: true,
            path: "/index.html",
            status: 200,
            body: INDEX_BODY,
        },
        Row {
            case: "an object the bucket holds back, on the authenticating plane",
            authenticating: true,
            path: "/held-back.html",
            status: 403,
            body: ACCESS_DENIED,
        },
        Row {
            case: "the same object, on a plane that authenticates nothing",
            authenticating: false,
            path: "/held-back.html",
            status: 403,
            body: ACCESS_DENIED,
        },
        Row {
            case: "an object nothing publishes, on the authenticating plane",
            authenticating: true,
            path: "/unlisted.txt",
            status: 403,
            body: ACCESS_DENIED,
        },
        Row {
            case: "the same unpublished object, on a plane that authenticates nothing",
            authenticating: false,
            path: "/unlisted.txt",
            status: 403,
            body: ACCESS_DENIED,
        },
    ]
}

/// The whole rule, over the table.
#[tokio::test]
async fn a_website_endpoint_origin_is_served_whatever_the_front_door_verifies() {
    let emulator = Emulator::new();
    emulator.staged_site().await;
    let (id, domain) = create_distribution(&emulator.setup, "website-origin-auth").await;
    awaited_deployment(&emulator.setup, "the website distribution", &id).await;

    for row in rows() {
        let router = if row.authenticating {
            &emulator.authenticating
        } else {
            &emulator.setup
        };
        let answer = view(router, &domain, row.path, &[]).await;
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
        if row.status == 200 {
            assert_eq!(
                answer.x_cache, MISS,
                "case {:?}: expected the origin's own answer, got X-Cache {:?}",
                row.case, answer.x_cache
            );
        }
        // Nothing the bucket holds back is ever served, whichever plane answered.
        assert!(
            !answer.body.contains(PRIVATE_BODY) && !answer.body.contains(UNPUBLISHED_BODY),
            "case {:?}: the answer carries bytes the bucket publishes to nobody: {:?}",
            row.case,
            answer.body
        );
    }
}
