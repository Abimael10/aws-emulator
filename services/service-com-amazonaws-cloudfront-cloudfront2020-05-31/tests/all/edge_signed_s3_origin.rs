// SPDX-License-Identifier: CC0-1.0

//! A cache behaviour that requires TRUSTED KEY GROUPS in front of a PRIVATE Amazon S3 origin
//! fronted by an ORIGIN ACCESS CONTROL, end to end over HTTP with Amazon S3 mounted beside
//! CloudFront on one engine.
//!
//! RULE: a cache behaviour whose `TrustedKeyGroups` are enabled requires every viewer request
//! that matches it to carry a CloudFront signature made with a private key whose public key is
//! in one of those groups, and CloudFront itself answers `403` to a request that carries none,
//! names a key the groups do not hold, or carries a policy whose conditions do not hold —
//! making no origin request at all; a request the groups DO admit is served by the behaviour's
//! own origin fetch, so for a private S3 origin under an origin access control the viewer
//! receives the stored object's bytes exactly as the bucket's grant to CloudFront lets the edge
//! read them.
//!
//! Only a suite that mounts the object store can state the second half: the crate's own
//! `edge_signed_access` suite decides admission against an origin that needs no credential,
//! and `edge_data_plane` fetches an origin that grants everyone — neither can say that the
//! bytes an admitted signed URL serves are an object in a private bucket, nor that the
//! signature check happens BEFORE the origin is asked, which is what separates CloudFront's own
//! refusal from the origin's.
//!
//! The rows span what the rule generalises over: the form of the policy the signature was made
//! over (a canned policy's `Expires`, a custom policy's `Policy`), the carrier it travels in
//! (the query string of a signed URL, the `CloudFront-`-prefixed signed cookies), the key's
//! form and the object's bytes (a bare name, a nested prefix, a document), and whether the
//! origin bucket has granted CloudFront the read at all — the last row being the one that shows
//! an admitted viewer request reaching the origin and being answered by the ORIGIN's own
//! vocabulary rather than CloudFront's.
//!
//! The suite lives in THIS crate, beside `edge_signed_access`, rather than beside the
//! cross-service `cloudfront_oac_s3_origin_edge` harness it resembles, because signing a URL
//! needs the key material and the RSA parts `num-bigint`/`pkcs1`/`pkcs8` read out of a generated
//! key — dev-dependencies this crate has and the router's test target does not. The signer and
//! the policy builders come from `edge_signed_access`, so there is one implementation of
//! CloudFront's signing procedure here; the small staging harness below is this crate's own
//! convention (`trust_store_lifecycle`, `standard_logging_bucket`), and a third copy of it
//! should not be added.
//!
//! Every viewer request goes through a router with request authentication ON, which is what
//! makes the origin's half real: a viewer presents no credential of any kind, so the only thing
//! that can make Amazon S3 serve the origin fetch is the signature the edge puts on it for the
//! CloudFront service principal. The setup calls go through a second router over the SAME
//! engine with authentication off, so each carries a credential scope (which is what names the
//! Region and the service) without a computed signature.

use std::sync::Arc;

use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_signed_access::{
    canned_policy, canned_query, create_key_group, custom_policy, custom_query, far_future,
    far_past, upload_public_key, url_safe_base64, view, Signer, Viewed, CACHING_DISABLED, ERROR,
    MISS,
};

/// The Region the origin buckets are created in, and so the Region the S3 origin's
/// `DomainName` addresses them at.
const REGION: &str = "us-east-1";

/// The service principal an origin access control makes CloudFront's origin request as, and
/// so the principal an origin bucket's policy grants the read to.
const CLOUDFRONT_PRINCIPAL: &str = "cloudfront.amazonaws.com";

/// The code Amazon S3 refuses a read nothing granted with (Amazon S3 "Error responses"), which
/// is what a viewer sees when the signature was admitted and the ORIGIN refused the fetch.
const ACCESS_DENIED: &str = "AccessDenied";

/// The `Message` that refusal carries, which is Amazon S3's own wording for it ("Access
/// Denied") and NOT CloudFront's for the same code ("Access denied"): a viewer served this
/// sentence was answered by the origin.
const ORIGIN_ACCESS_DENIED_MESSAGE: &str = "<Message>Access Denied</Message>";

/// A member Amazon S3's error document carries and CloudFront's own refusal body — `Code` and
/// `Message` alone — does not, so a refusal carrying it came from the origin rather than from
/// the edge.
const ORIGIN_ENVELOPE_MEMBER: &str = "<RequestId>";

/// CloudFront's code for a request that names no key at all, and the one for a key the
/// behaviour's trusted key groups do not hold.
pub(crate) const MISSING_KEY: &str = "MissingKey";
const INVALID_KEY: &str = "InvalidKey";

// ---------------------------------------------------------------------------
// The emulator: CloudFront plus the object store its origin lives in
// ---------------------------------------------------------------------------

/// CloudFront and Amazon S3 over ONE shared engine, as two planes: the SETUP plane, whose
/// calls carry a credential scope without a computed signature, and the VIEWER plane, which
/// authenticates — so what makes the origin fetch succeed is the signature the edge applies.
///
/// `pub(crate)`: a sibling suite that needs the same two planes over one engine
/// ([`super::edge_signed_cookie_prefix_policy`]) stages through this one harness rather than
/// growing a second copy of it.
pub(crate) struct Emulator {
    pub(crate) setup: MultiServiceRouter,
    pub(crate) viewers: MultiServiceRouter,
    callback: EmbeddedEmulator,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let setup = mounted(&engine, false);
        let viewers = mounted(&engine, true);
        let callback = EmbeddedEmulator::from_router(setup.clone());
        Self {
            setup,
            viewers,
            callback,
        }
    }

    /// The Amazon S3 client, built by the one shared constructor over the in-process callback,
    /// so the bucket this suite stages is the one the edge's origin fetch reads.
    fn s3(&self) -> ::aws_sdk_s3::Client {
        service_common::s3_client::s3_client(&self.callback, REGION)
    }

    /// Create `bucket` and put `body` at `key` in it. The bucket carries no public access of
    /// any kind: what reaches the object is the policy each row writes, or nothing.
    pub(crate) async fn staged_object(&self, case: &str, bucket: &str, key: &str, body: &str) {
        self.s3()
            .create_bucket()
            .bucket(bucket)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: creating {bucket} must succeed, got {error:?}")
            });
        self.s3()
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(::aws_sdk_s3::primitives::ByteStream::from(
                body.as_bytes().to_vec(),
            ))
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: putting {bucket}/{key} must succeed, got {error:?}")
            });
    }

    /// Put `document` on `bucket` as its resource policy.
    pub(crate) async fn staged_policy(&self, case: &str, bucket: &str, document: &str) {
        self.s3()
            .put_bucket_policy()
            .bucket(bucket)
            .policy(document)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the policy of {bucket} must be stored, got {error:?}")
            });
    }
}

/// Both services on `engine`, with request authentication as `authenticated` says.
///
/// `pub(crate)`: every suite in this target that needs CloudFront and the object store on ONE
/// engine ([`super::edge_website_endpoint_origin`] among them) mounts them through this one
/// constructor rather than carrying its own copy.
pub(crate) fn mounted(
    engine: &Arc<engine::InMemoryEngine>,
    authenticated: bool,
) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(authenticated);
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(engine),
    ));
    router
}

/// The host a bucket of this Region is addressed at virtual-hosted style, which is what an S3
/// origin's `DomainName` carries.
pub(crate) fn origin_domain(bucket: &str) -> String {
    format!("{bucket}.s3.{REGION}.amazonaws.com")
}

/// A bucket policy granting the CloudFront service principal `s3:GetObject` on every object of
/// `bucket`, scoped by `AWS:SourceArn` to the one distribution `arn` names — the document AWS
/// publishes for an origin access control.
pub(crate) fn grant(bucket: &str, arn: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\
         \"Sid\":\"AllowCloudFrontServicePrincipal\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{CLOUDFRONT_PRINCIPAL}\"}},\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{bucket}/*\",\
         \"Condition\":{{\"StringEquals\":{{\"AWS:SourceArn\":\"{arn}\"}}}}}}]}}"
    )
}

// ---------------------------------------------------------------------------
// Staging the distribution
// ---------------------------------------------------------------------------

/// `CreateOriginAccessControl` for an `s3` origin CloudFront signs every request to, answering
/// the control's id.
pub(crate) async fn create_origin_access_control(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> String {
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
        "case {case:?}: CreateOriginAccessControl answered {}: {}",
        created.status, created.raw
    );
    created
        .root()
        .find_text("Id")
        .expect("the created origin access control reports an id")
        .to_string()
}

/// `CreateDistribution` over the S3 origin `bucket` fronted by the origin access control
/// `control`, whose DEFAULT behaviour requires the key group `key_group`; answers the
/// distribution's `(Id, DomainName, ARN)`.
pub(crate) async fn create_distribution(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    bucket: &str,
    control: &str,
    key_group: &str,
) -> (String, String, String) {
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{caller_reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items><Origin>\
             <Id>origin-bucket</Id><DomainName>{}</DomainName>\
             <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
             <OriginAccessControlId>{control}</OriginAccessControlId>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
             <TrustedKeyGroups><Enabled>true</Enabled><Quantity>1</Quantity>\
             <Items><KeyGroup>{key_group}</KeyGroup></Items></TrustedKeyGroups>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             <AllowedMethods><Quantity>2</Quantity>\
             <Items><Method>GET</Method><Method>HEAD</Method></Items></AllowedMethods>\
             <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
             </DefaultCacheBehavior>\
             <Comment>signed access over a private s3 origin</Comment>\
             <Enabled>true</Enabled>\
             </DistributionConfig>",
            origin_domain(bucket)
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
    (read("Id"), read("DomainName"), read("ARN"))
}

// ---------------------------------------------------------------------------
// The cases
// ---------------------------------------------------------------------------

/// Which policy form a row's signature is made over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// A canned policy, whose expiry travels as `Expires` and whose statement CloudFront
    /// rebuilds from the URL.
    Canned,
    /// A custom policy, whose whole statement travels base64-encoded as `Policy`.
    Custom,
}

/// Where a row's signature travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Carrier {
    /// The query string of a signed URL.
    Query,
    /// The `CloudFront-`-prefixed signed cookies.
    Cookies,
}

/// One row: a private bucket holding one object, a distribution over it under an origin access
/// control whose default behaviour requires the suite's key group, and the signature form and
/// carrier a viewer presents.
struct Row {
    case: &'static str,
    bucket: &'static str,
    key: &'static str,
    body: &'static str,
    form: Form,
    carrier: Carrier,
    /// Whether the bucket grants CloudFront the read. A row that does not is the one where the
    /// ORIGIN refuses an admitted viewer request.
    granted: bool,
}

/// The table: one row per policy form, carrier, key form and grant.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a bare key, a canned policy in the query string",
            bucket: "signed-s3-canned",
            key: "protected.txt",
            body: "the protected object",
            form: Form::Canned,
            carrier: Carrier::Query,
            granted: true,
        },
        Row {
            case: "a nested key, a custom policy in the query string",
            bucket: "signed-s3-custom",
            key: "media/2026/clip.txt",
            body: "another object, other bytes",
            form: Form::Custom,
            carrier: Carrier::Query,
            granted: true,
        },
        Row {
            case: "signed cookies carrying a custom policy",
            bucket: "signed-s3-cookies-custom",
            key: "held/document.json",
            body: "{\"held\":true}",
            form: Form::Custom,
            carrier: Carrier::Cookies,
            granted: true,
        },
        Row {
            case: "signed cookies carrying a canned policy's expiry",
            bucket: "signed-s3-cookies-canned",
            key: "deep/nested/page.html",
            body: "<p>deep and nested</p>",
            form: Form::Canned,
            carrier: Carrier::Cookies,
            granted: true,
        },
        Row {
            case: "a bucket that granted CloudFront nothing",
            bucket: "signed-s3-ungranted",
            key: "protected.txt",
            body: "nothing granted",
            form: Form::Custom,
            carrier: Carrier::Query,
            granted: false,
        },
    ]
}

/// What one row left behind for the viewer phase.
struct Prepared {
    case: &'static str,
    key: &'static str,
    body: &'static str,
    form: Form,
    carrier: Carrier,
    granted: bool,
    domain: String,
    id: String,
}

/// One viewer request, as a row presents it: the signature `signature` over `statement`, in the
/// row's form and carrier, naming the public key `key_pair_id`.
fn presented(
    row_key: &str,
    form: Form,
    carrier: Carrier,
    statement_expiry: u64,
    policy: &str,
    signature: &str,
    key_pair_id: &str,
) -> (String, Option<String>) {
    let query = match form {
        Form::Canned => canned_query(statement_expiry, signature, key_pair_id),
        Form::Custom => custom_query(policy, signature, key_pair_id),
    };
    match carrier {
        Carrier::Query => (format!("/{row_key}?{query}"), None),
        Carrier::Cookies => {
            let value = match form {
                Form::Canned => format!(
                    "CloudFront-Expires={statement_expiry}; \
                     CloudFront-Signature={signature}; \
                     CloudFront-Key-Pair-Id={key_pair_id}"
                ),
                Form::Custom => format!(
                    "CloudFront-Policy={}; CloudFront-Signature={signature}; \
                     CloudFront-Key-Pair-Id={key_pair_id}",
                    url_safe_base64(policy.as_bytes())
                ),
            };
            (format!("/{row_key}"), Some(value))
        }
    }
}

/// Send one viewer request built by [`presented`].
async fn viewed(
    router: &MultiServiceRouter,
    domain: &str,
    path_and_query: &str,
    cookies: Option<&str>,
) -> Viewed {
    match cookies {
        Some(value) => view(router, domain, path_and_query, &[("cookie", value)]).await,
        None => view(router, domain, path_and_query, &[]).await,
    }
}

/// Assert that `answer` is CloudFront's own `403` under `code`, carrying none of the object's
/// bytes — the origin was never asked.
fn assert_cloudfront_refused(answer: &Viewed, case: &str, leg: &str, code: &str, body: &str) {
    assert_eq!(
        (answer.status, answer.x_cache.as_str()),
        (403, ERROR),
        "case {case:?} ({leg}): expected CloudFront's own 403 with X-Cache {ERROR:?}, got {} \
         with {:?} (body {:?})",
        answer.status,
        answer.x_cache,
        answer.body
    );
    assert!(
        answer.carries_code(code),
        "case {case:?} ({leg}): expected the error code {code:?}, got {:?}",
        answer.body
    );
    assert!(
        !answer.body.contains(body),
        "case {case:?} ({leg}): the refusal carries the object's bytes, so the origin was \
         asked: {:?}",
        answer.body
    );
}

/// The whole rule, over the table: what a trusted key group admits in front of a private S3
/// origin, and what it refuses — with the admitted request's answer being the ORIGIN's.
#[tokio::test]
async fn a_trusted_key_group_guards_a_private_s3_origin_through_the_edge() {
    let emulator = Emulator::new();
    let signer = Signer::generate();
    let stranger = Signer::generate();
    let trusted = upload_public_key(&emulator.setup, "signed-s3-trusted", &signer).await;
    let untrusted = upload_public_key(&emulator.setup, "signed-s3-untrusted", &stranger).await;
    let (group, _etag) = create_key_group(&emulator.setup, "signed-s3", &[&trusted]).await;

    let mut prepared: Vec<Prepared> = Vec::new();
    for (index, row) in rows().into_iter().enumerate() {
        emulator
            .staged_object(row.case, row.bucket, row.key, row.body)
            .await;
        let control =
            create_origin_access_control(&emulator.setup, row.case, &format!("signed-s3-{index}"))
                .await;
        let (id, domain, arn) = create_distribution(
            &emulator.setup,
            row.case,
            &format!("signed-s3-reference-{index}"),
            row.bucket,
            &control,
            &group,
        )
        .await;
        // The grant is written AFTER the distribution exists, because it is scoped to that
        // distribution's own ARN — the order a client has to follow.
        if row.granted {
            emulator
                .staged_policy(row.case, row.bucket, &grant(row.bucket, &arn))
                .await;
        }
        prepared.push(Prepared {
            case: row.case,
            key: row.key,
            body: row.body,
            form: row.form,
            carrier: row.carrier,
            granted: row.granted,
            domain,
            id,
        });
    }

    for row in &prepared {
        // A configuration that has not reached the edge locations serves no viewer, and the
        // CLOCK owes that propagation: the condition is polled for, never a span waited out.
        awaited_deployment(&emulator.setup, row.case, &row.id).await;
        let resource = format!("https://{}/{}", row.domain, row.key);

        // 1. No signature at all: CloudFront cannot be told whose request it is, and the origin
        //    is never asked.
        let unsigned = viewed(
            &emulator.viewers,
            &row.domain,
            &format!("/{}", row.key),
            None,
        )
        .await;
        assert_cloudfront_refused(&unsigned, row.case, "unsigned", MISSING_KEY, row.body);

        // 2. A signature made with a key the group HOLDS, over a policy whose conditions hold:
        //    admitted, so the behaviour's own origin fetch decides what the viewer gets.
        let expires = far_future();
        let policy = match row.form {
            Form::Canned => canned_policy(&resource, expires),
            Form::Custom => custom_policy(&resource, expires, None, None),
        };
        let (path_and_query, cookies) = presented(
            row.key,
            row.form,
            row.carrier,
            expires,
            &policy,
            &signer.sign(&policy),
            &trusted,
        );
        let admitted = viewed(
            &emulator.viewers,
            &row.domain,
            &path_and_query,
            cookies.as_deref(),
        )
        .await;
        if row.granted {
            assert_eq!(
                (admitted.status, admitted.x_cache.as_str()),
                (200, MISS),
                "case {:?} (signed by a trusted key): expected the origin's 200 with X-Cache \
                 {MISS:?}, got {} with {:?} (body {:?})",
                row.case,
                admitted.status,
                admitted.x_cache,
                admitted.body
            );
            assert_eq!(
                admitted.body, row.body,
                "case {:?} (signed by a trusted key): expected the S3 object's own bytes, got \
                 {:?}",
                row.case, admitted.body
            );
        } else {
            assert_eq!(
                admitted.status, 403,
                "case {:?} (signed by a trusted key): expected the ORIGIN's refusal through the \
                 edge, got {} with {:?}",
                row.case, admitted.status, admitted.body
            );
            assert!(
                admitted
                    .body
                    .contains(&format!("<Code>{ACCESS_DENIED}</Code>")),
                "case {:?} (signed by a trusted key): expected the origin's own \
                 {ACCESS_DENIED} document, got {:?}",
                row.case,
                admitted.body
            );
            // The document is the ORIGIN'S, not CloudFront's own denial of the same code: it
            // carries Amazon S3's wording of the sentence and a member CloudFront's refusal
            // body does not have at all. Without this, the leg would pass against an edge that
            // refused the admitted request itself and never asked the origin.
            for marker in [ORIGIN_ACCESS_DENIED_MESSAGE, ORIGIN_ENVELOPE_MEMBER] {
                assert!(
                    admitted.body.contains(marker),
                    "case {:?} (signed by a trusted key): expected the ORIGIN's own refusal, \
                     which carries {marker:?}, got {:?}",
                    row.case,
                    admitted.body
                );
            }
            assert!(
                !admitted.body.contains(row.body),
                "case {:?} (signed by a trusted key): a refused read must not carry the \
                 object's bytes, got {:?}",
                row.case,
                admitted.body
            );
        }

        // 3. A signature made with a key the group does NOT hold: an unknown key, whatever the
        //    policy says and whoever granted the bucket.
        let (path_and_query, cookies) = presented(
            row.key,
            row.form,
            row.carrier,
            expires,
            &policy,
            &stranger.sign(&policy),
            &untrusted,
        );
        let foreign = viewed(
            &emulator.viewers,
            &row.domain,
            &path_and_query,
            cookies.as_deref(),
        )
        .await;
        assert_cloudfront_refused(
            &foreign,
            row.case,
            "signed by a key the group does not hold",
            INVALID_KEY,
            row.body,
        );

        // 4. The trusted key over a policy that has EXPIRED: a condition that does not hold is
        //    CloudFront's own denial, and the origin is still never asked.
        let past = far_past();
        let expired = match row.form {
            Form::Canned => canned_policy(&resource, past),
            Form::Custom => custom_policy(&resource, past, None, None),
        };
        let (path_and_query, cookies) = presented(
            row.key,
            row.form,
            row.carrier,
            past,
            &expired,
            &signer.sign(&expired),
            &trusted,
        );
        let stale = viewed(
            &emulator.viewers,
            &row.domain,
            &path_and_query,
            cookies.as_deref(),
        )
        .await;
        assert_cloudfront_refused(
            &stale,
            row.case,
            "signed over a policy that has expired",
            ACCESS_DENIED,
            row.body,
        );
    }
}
