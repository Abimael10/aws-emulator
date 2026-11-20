// SPDX-License-Identifier: CC0-1.0

//! WHEN a CloudFront origin fetch of a private Amazon S3 origin is decided, and by WHICH
//! document — across the two services on one engine.
//!
//! RULE: an origin fetch under an origin access mechanism is decided, at the moment it is made,
//! against the document the ORIGIN carries THEN — for the principal that mechanism names (the
//! `cloudfront.amazonaws.com` service principal for an origin access control, the identity
//! itself for a legacy origin access identity) and with `aws:SourceArn` naming the distribution
//! the fetch is made for. So a document in force that grants that principal the action over
//! that object is answered with the object's bytes at the edge, and one that grants it nothing
//! — a grant scoped to another distribution, a grant to another principal, an explicit refusal,
//! no document at all — is relayed to the viewer as the origin's own refusal. Rewriting or
//! clearing the document under a distribution that is already deployed therefore changes what
//! the NEXT viewer is served, with no second deployment and no wait for one: the decision
//! belongs to the fetch, not to the deployment.
//!
//! That is the half the sibling suites cannot state. `cloudfront_oac_s3_origin_edge`,
//! `cloudfront_oai_s3_origin_edge` and `cloudfront_s3_origin_edge_authorization` each give a
//! row its OWN bucket and its own document, written once before the first viewer arrives, so a
//! decision pinned at deployment time and a decision taken per fetch are indistinguishable in
//! them. Here ONE bucket holding ONE object is read through THREE distributions while the one
//! document over it is rewritten between reads, so each observation is of the document in force
//! at that instant and of the mechanism that fetch was made under.
//!
//! The steps span what the rule generalises over: whether a document exists at all, which
//! principal it names (the service principal, the legacy identity's canonical user), how the
//! grant is scoped (`StringEquals` on one distribution's ARN, an `ArnLike` pattern over the
//! account's distributions, no condition at all), the form of the resource it is stated over
//! (one object's ARN, a wildcard under the bucket), and whether the document also carries an
//! explicit refusal. Each step is observed through all three distributions, so one step's
//! answer is three: what the document grants the first origin access control, the second, and
//! the legacy identity.
//!
//! The VIEWER requests go through a router with request authentication ON, which is what makes
//! the identity of the fetch the subject: a viewer presents no credential, so the only thing
//! that can make S3 serve the fetch is the credential CloudFront minted for it. The setup calls
//! go through a second router over the SAME engine with authentication off, so each carries a
//! credential scope (which names the Region) without a computed signature.

use std::sync::Arc;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_oac_s3_origin_edge::{
    awaited_deployment, call, create_bucket, create_distribution, create_origin_access_control,
    emulator, put_bucket_policy, put_object, regional_endpoint, view, CLOUDFRONT_PRINCIPAL,
};
use super::cloudfront_oai_s3_origin_edge::{
    create_distribution as create_legacy_distribution, create_origin_access_identity,
};

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so every observation below is of a fetch that
/// was actually made and decided rather than of an answer the edge was still holding.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The code and the message Amazon S3 refuses a read nothing granted with (Amazon S3 "Error
/// responses"): the refusal the edge relays to the viewer.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_MESSAGE: &str = "Access Denied";

/// What the edge reports as the outcome of an answer the ORIGIN produced for this request.
const MISS: &str = "Miss from cloudfront";

/// The one bucket every step's document is written on, the one object every fetch asks for and
/// the bytes it was stored with.
const BUCKET: &str = "live-policy-origin-bucket";
const KEY: &str = "assets/site/index.html";
const OBJECT_BODY: &str = "the one object three distributions read";

// The wait for a submitted configuration to deploy is the shared harness's `awaited_deployment`,
// which polls the status CloudFront reports rather than sleeping for a span.

/// `s3:DeleteBucketPolicy` (`DELETE /{Bucket}?policy`) — what leaves the bucket carrying no
/// document at all.
async fn delete_bucket_policy(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let cleared = call(
        router,
        "s3",
        &regional_endpoint(),
        "DELETE",
        &format!("/{bucket}?policy"),
        &[],
        Bytes::new(),
    )
    .await;
    assert!(
        (200..300).contains(&cleared.status),
        "case {case:?}: DeleteBucketPolicy must succeed, got {} with {:?}",
        cleared.status,
        cleared.text()
    );
}

/// The names a step's document is written over, every one of them read back from what the two
/// services minted rather than spelled out here.
struct Scope {
    /// The ARN of the distribution the FIRST origin access control fronts.
    first_arn: String,
    /// The ARN of the distribution the SECOND origin access control fronts.
    second_arn: String,
    /// The `ArnLike` pattern that names every distribution of this account: the first ARN with
    /// its own identifier replaced by IAM's wildcard.
    any_distribution: String,
    /// The `S3CanonicalUserId` of the legacy origin access identity.
    canonical_user: String,
    /// The ARN of the one object every fetch asks for.
    object: String,
    /// The ARN of every object under the bucket.
    any_object: String,
}

/// One statement of a bucket policy: `effect` over `resource` for the principal `principal`
/// names, conditioned by `condition` when the step states one.
fn statement(effect: &str, principal: &str, resource: &str, condition: Option<&str>) -> String {
    let condition = condition
        .map(|clause| format!(",\"Condition\":{clause}"))
        .unwrap_or_default();
    format!(
        "{{\"Effect\":\"{effect}\",\"Principal\":{{{principal}}},\
         \"Action\":\"s3:GetObject\",\"Resource\":\"{resource}\"{condition}}}"
    )
}

/// A bucket policy carrying `statements`.
fn document(statements: &[String]) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{}]}}",
        statements.join(",")
    )
}

/// The `Principal` element naming the service principal an ORIGIN ACCESS CONTROL makes the
/// fetch as.
fn as_the_service_principal() -> String {
    format!("\"Service\":\"{CLOUDFRONT_PRINCIPAL}\"")
}

/// The `Principal` element naming a LEGACY origin access identity by the canonical user id it
/// was minted with — the spelling the console writes for one.
fn as_the_canonical_user(canonical_user: &str) -> String {
    format!("\"CanonicalUser\":\"{canonical_user}\"")
}

/// Which mechanism's fetch an expectation is about: the two origin access controls and the
/// legacy identity all read the SAME object through the SAME document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The origin served the fetch, so the viewer receives the object's own bytes.
    TheObject,
    /// The origin refused the fetch, so the viewer receives its refusal.
    OriginRefusal,
}

/// One step: the document the bucket carries from now on, and what each of the three
/// distributions then serves.
struct Step {
    case: &'static str,
    /// The document to install, or `None` to clear the bucket's policy entirely.
    document: fn(&Scope) -> Option<String>,
    first: Expected,
    second: Expected,
    legacy: Expected,
}

/// The steps, in the order they are applied to the one bucket.
const STEPS: &[Step] = &[
    Step {
        case: "a bucket carrying no document at all",
        document: |_scope| None,
        first: Expected::OriginRefusal,
        second: Expected::OriginRefusal,
        legacy: Expected::OriginRefusal,
    },
    Step {
        case: "a grant to the service principal scoped by StringEquals to the first \
               distribution's ARN",
        document: |scope| {
            Some(document(&[statement(
                "Allow",
                &as_the_service_principal(),
                &scope.object,
                Some(&format!(
                    "{{\"StringEquals\":{{\"AWS:SourceArn\":\"{}\"}}}}",
                    scope.first_arn
                )),
            )]))
        },
        first: Expected::TheObject,
        second: Expected::OriginRefusal,
        legacy: Expected::OriginRefusal,
    },
    Step {
        case: "the same grant rewritten to name the SECOND distribution's ARN",
        document: |scope| {
            Some(document(&[statement(
                "Allow",
                &as_the_service_principal(),
                &scope.object,
                Some(&format!(
                    "{{\"StringEquals\":{{\"AWS:SourceArn\":\"{}\"}}}}",
                    scope.second_arn
                )),
            )]))
        },
        first: Expected::OriginRefusal,
        second: Expected::TheObject,
        legacy: Expected::OriginRefusal,
    },
    Step {
        case: "a grant to the service principal under an ArnLike pattern over every \
               distribution of the account, stated over every object of the bucket",
        document: |scope| {
            Some(document(&[statement(
                "Allow",
                &as_the_service_principal(),
                &scope.any_object,
                Some(&format!(
                    "{{\"ArnLike\":{{\"aws:SourceArn\":\"{}\"}}}}",
                    scope.any_distribution
                )),
            )]))
        },
        first: Expected::TheObject,
        second: Expected::TheObject,
        legacy: Expected::OriginRefusal,
    },
    Step {
        case: "a grant to the service principal carrying no condition at all",
        document: |scope| {
            Some(document(&[statement(
                "Allow",
                &as_the_service_principal(),
                &scope.any_object,
                None,
            )]))
        },
        first: Expected::TheObject,
        second: Expected::TheObject,
        legacy: Expected::OriginRefusal,
    },
    Step {
        case: "a grant to the legacy identity's canonical user alone",
        document: |scope| {
            Some(document(&[statement(
                "Allow",
                &as_the_canonical_user(&scope.canonical_user),
                &scope.object,
                None,
            )]))
        },
        first: Expected::OriginRefusal,
        second: Expected::OriginRefusal,
        legacy: Expected::TheObject,
    },
    Step {
        case: "both principals granted, with an explicit Deny about the service principal \
               beside the grants",
        document: |scope| {
            Some(document(&[
                statement(
                    "Allow",
                    &as_the_service_principal(),
                    &scope.any_object,
                    None,
                ),
                statement(
                    "Allow",
                    &as_the_canonical_user(&scope.canonical_user),
                    &scope.any_object,
                    None,
                ),
                statement(
                    "Deny",
                    &as_the_service_principal(),
                    &scope.any_object,
                    None,
                ),
            ]))
        },
        first: Expected::OriginRefusal,
        second: Expected::OriginRefusal,
        legacy: Expected::TheObject,
    },
    Step {
        case: "the document cleared again, leaving the bucket granting nothing",
        document: |_scope| None,
        first: Expected::OriginRefusal,
        second: Expected::OriginRefusal,
        legacy: Expected::OriginRefusal,
    },
];

/// Read the one object through `domain` and assert the answer is the one `expected` names.
///
/// `mechanism` names WHICH of the three fetches this is, so a failure says which mechanism the
/// document in force decided differently from the rule.
async fn assert_edge_answer(
    viewers: &MultiServiceRouter,
    case: &str,
    mechanism: &str,
    domain: &str,
    expected: Expected,
) {
    let viewed = view(viewers, domain, &format!("/{KEY}")).await;
    match expected {
        Expected::TheObject => {
            assert_eq!(
                viewed.status,
                200,
                "case {case:?} ({mechanism}): expected the origin's object through the edge, \
                 got {} with {:?}",
                viewed.status,
                viewed.text()
            );
            assert_eq!(
                viewed.text(),
                OBJECT_BODY,
                "case {case:?} ({mechanism}): expected the ORIGIN's own bytes, got {:?}",
                viewed.text()
            );
            assert_eq!(
                viewed.header("x-cache"),
                MISS,
                "case {case:?} ({mechanism}): expected the answer to be the origin's, got \
                 X-Cache {:?}",
                viewed.header("x-cache")
            );
        }
        Expected::OriginRefusal => {
            assert_eq!(
                viewed.status,
                403,
                "case {case:?} ({mechanism}): expected the origin's refusal through the edge, \
                 got {} with {:?}",
                viewed.status,
                viewed.text()
            );
            assert_eq!(
                viewed.element("Code"),
                ACCESS_DENIED,
                "case {case:?} ({mechanism}): expected the origin's own {ACCESS_DENIED} \
                 document, got {:?}",
                viewed.text()
            );
            assert_eq!(
                viewed.element("Message"),
                ACCESS_DENIED_MESSAGE,
                "case {case:?} ({mechanism}): expected the origin's own refusal wording, got \
                 {:?}",
                viewed.text()
            );
            assert!(
                !viewed.text().contains(OBJECT_BODY),
                "case {case:?} ({mechanism}): a refused fetch must carry none of the object's \
                 bytes, got {:?}",
                viewed.text()
            );
        }
    }
    // Every edge answer, served or refused, identifies the request it answers.
    assert!(
        !viewed.header("x-amz-cf-id").is_empty(),
        "case {case:?} ({mechanism}): the edge must stamp an X-Amz-Cf-Id, got none (status {})",
        viewed.status
    );
}

#[tokio::test]
async fn an_origin_fetch_is_decided_by_the_document_the_bucket_carries_when_it_is_made() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // The setup plane: authentication off, so each call carries a credential scope without a
    // computed signature. The VIEWER plane is the authenticating one, which is what makes the
    // credential the fetch carries the thing S3 serves it for.
    let setup = emulator(&engine, false);
    let viewers = emulator(&engine, true);

    let case = "the shared private origin";
    create_bucket(&setup, case, BUCKET).await;
    put_object(&setup, case, BUCKET, KEY, OBJECT_BODY, &[]).await;

    // Two distributions fronted by an origin access control each, and one fronted by the legacy
    // origin access identity — all three over the SAME bucket and the same object, so what
    // separates their answers is only the principal their mechanism names and the ARN their
    // fetch acts for.
    let first_control = create_origin_access_control(&setup, case, "live-policy-first", "always")
        .await;
    let second_control =
        create_origin_access_control(&setup, case, "live-policy-second", "always").await;
    let (first_id, first_domain, first_arn) = create_distribution(
        &setup,
        case,
        "live-policy-first-reference",
        BUCKET,
        &first_control,
        CACHING_DISABLED,
    )
    .await;
    let (second_id, second_domain, second_arn) = create_distribution(
        &setup,
        case,
        "live-policy-second-reference",
        BUCKET,
        &second_control,
        CACHING_DISABLED,
    )
    .await;
    let (identity, canonical_user) =
        create_origin_access_identity(&setup, case, "live-policy-identity").await;
    let (legacy_id, legacy_domain) = create_legacy_distribution(
        &setup,
        case,
        "live-policy-legacy-reference",
        BUCKET,
        &identity,
    )
    .await;

    for id in [&first_id, &second_id, &legacy_id] {
        awaited_deployment(&setup, case, id).await;
    }

    // The ARN pattern naming every distribution of this account: the first distribution's own
    // ARN with its identifier replaced by IAM's wildcard, so the pattern is the one the two
    // services minted rather than one spelled out here.
    let (arn_prefix, _id) = first_arn.rsplit_once('/').unwrap_or_else(|| {
        panic!("case {case:?}: expected a distribution ARN naming its id, got {first_arn:?}")
    });
    let any_distribution = format!("{arn_prefix}/*");
    let scope = Scope {
        first_arn,
        second_arn,
        any_distribution,
        canonical_user,
        // An S3 object's ARN leaves the Region and the account fields empty, which is how a
        // bucket policy names one.
        object: format!("arn:aws:s3:::{BUCKET}/{KEY}"),
        any_object: format!("arn:aws:s3:::{BUCKET}/*"),
    };

    for step in STEPS {
        match (step.document)(&scope) {
            Some(policy) => put_bucket_policy(&setup, step.case, BUCKET, &policy).await,
            None => delete_bucket_policy(&setup, step.case, BUCKET).await,
        }
        for (mechanism, domain, expected) in [
            (
                "the first origin access control",
                &first_domain,
                step.first,
            ),
            (
                "the second origin access control",
                &second_domain,
                step.second,
            ),
            (
                "the legacy origin access identity",
                &legacy_domain,
                step.legacy,
            ),
        ] {
            assert_edge_answer(&viewers, step.case, mechanism, domain, expected).await;
        }
    }
}
