// SPDX-License-Identifier: CC0-1.0

//! `CreateDistributionWithTags` over a PRIVATE Amazon S3 origin fronted by a LEGACY ORIGIN
//! ACCESS IDENTITY, end to end across the two services on one engine.
//!
//! RULE: `CreateDistributionWithTags` takes "a distribution configuration and a list of tags
//! to be associated with the distribution" and runs the same create `CreateDistribution`
//! does — so the distribution it mints is deployed by the clock like any other, carries
//! exactly the submitted tag set for `ListTagsForResource`, and serves its origin by the
//! configuration alone: an `S3OriginConfig$OriginAccessIdentity` naming an identity in the
//! documented form `origin-access-identity/cloudfront/ID-of-origin-access-identity` is
//! served the object when the BUCKET's own policy grants the read to that identity, by
//! either spelling AWS publishes for it, and is answered the origin's own refusal when
//! nothing granted it.
//!
//! Only a cross-service suite can pin it: the identity, the tags and the distribution are
//! CloudFront's records, the document that grants the identity and the object it guards are
//! Amazon S3's. `cloudfront_oai_s3_origin_edge` beside it pins the identity rule for the
//! UNTAGGED create; this one pins that the tagged create — a different operation, with its
//! own input shape and its own tag handling before anything is stored — produces a
//! distribution that deploys and serves exactly the same way, with the tags readable back
//! off it.
//!
//! The rows span what the rule generalises over: the size of the tag set (two tags, one, and
//! none at all), which spelling the bucket policy names the identity by (the canonical user
//! id, the IAM principal), whether it is granted at all, and the key form of the object (a
//! nested prefix, a bare name).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_oac_s3_origin_edge::{
    call, create_bucket, emulator, origin_domain, put_bucket_policy, put_object, view, Answer,
    API_VERSION, CLOUDFRONT_HOST,
};

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: the
/// viewer request of every row reaches the origin.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The code Amazon S3 refuses a read nothing granted with (Amazon S3 "Error responses").
const ACCESS_DENIED: &str = "AccessDenied";

/// How long to wait between polls of the clock-owed deployment, and how many polls to make:
/// the wait is on the CONDITION, with a deadline far past the span the service owes it.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 400;

/// `cloudfront:CreateCloudFrontOriginAccessIdentity`, answering the identity's
/// `(Id, S3CanonicalUserId)`.
async fn create_origin_access_identity(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
) -> (String, String) {
    let body = format!(
        "<CloudFrontOriginAccessIdentityConfig \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Comment>an identity a tagged distribution fronts a bucket with</Comment>\
         </CloudFrontOriginAccessIdentityConfig>"
    );
    let created = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "POST",
        &format!("/{API_VERSION}/origin-access-identity/cloudfront"),
        &[("content-type", "application/xml")],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateCloudFrontOriginAccessIdentity must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, canonical) = (
        created.element("Id"),
        created.element("S3CanonicalUserId"),
    );
    assert!(
        !id.is_empty() && !canonical.is_empty(),
        "case {case:?}: the created identity reports no Id or S3CanonicalUserId: {:?}",
        created.text()
    );
    (id, canonical)
}

/// `cloudfront:CreateDistributionWithTags` over the S3 origin `bucket` fronted by the origin
/// access identity `identity`, carrying `tags`. Answers the distribution's
/// `(id, DomainName, ARN)`.
///
/// The payload is a `DistributionConfigWithTags`: the same `DistributionConfig` the untagged
/// create takes, with the tag set beside it.
async fn create_distribution_with_tags(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    bucket: &str,
    identity: &str,
    tags: &[(&str, &str)],
) -> (String, String, String) {
    let items: String = tags
        .iter()
        .map(|(key, value)| format!("<Tag><Key>{key}</Key><Value>{value}</Value></Tag>"))
        .collect();
    let tag_set = if tags.is_empty() {
        "<Tags></Tags>".to_string()
    } else {
        format!("<Tags><Items>{items}</Items></Tags>")
    };
    let body = format!(
        "<DistributionConfigWithTags \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <DistributionConfig>\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>origin-bucket</Id><DomainName>{}</DomainName>\
         <S3OriginConfig>\
         <OriginAccessIdentity>origin-access-identity/cloudfront/{identity}</OriginAccessIdentity>\
         </S3OriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>a tagged distribution over a private s3 origin</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>\
         {tag_set}\
         </DistributionConfigWithTags>",
        origin_domain(bucket)
    );
    let created = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "POST",
        &format!("/{API_VERSION}/distribution?WithTags"),
        &[("content-type", "application/xml")],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateDistributionWithTags must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, domain, arn) = (
        created.element("Id"),
        created.element("DomainName"),
        created.element("ARN"),
    );
    for (name, value) in [("Id", &id), ("DomainName", &domain), ("ARN", &arn)] {
        assert!(
            !value.is_empty(),
            "case {case:?}: the created distribution reports no {name}: {:?}",
            created.text()
        );
    }
    (id, domain, arn)
}

/// Poll `GetDistribution` until CloudFront first reports `id` `Deployed`, which is the state
/// a viewer request is served in.
async fn wait_until_deployed(router: &MultiServiceRouter, case: &str, id: &str) {
    for _ in 0..POLLS {
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
        if read.element("Status") == "Deployed" {
            return;
        }
        ::tokio::time::sleep(POLL).await;
    }
    panic!("case {case:?}: CloudFront never reported the distribution Deployed");
}

/// Every `<Tag>` of a tagging answer, as `(Key, Value)` in the order the answer lists them.
fn tags_of(answer: &Answer) -> Vec<(String, String)> {
    let body = answer.text();
    body.split("<Tag>")
        .skip(1)
        .filter_map(|tag| {
            let read = |name: &str| {
                tag.split_once(&format!("<{name}>"))
                    .and_then(|(_before, rest)| rest.split_once(&format!("</{name}>")))
                    .map(|(text, _after)| text.to_string())
            };
            read("Key").map(|key| (key, read("Value").unwrap_or_default()))
        })
        .collect()
}

/// `cloudfront:ListTagsForResource` over the distribution `arn`.
async fn list_tags(router: &MultiServiceRouter, case: &str, arn: &str) -> Vec<(String, String)> {
    let listed = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "GET",
        &format!("/{API_VERSION}/tagging?Resource={arn}"),
        &[],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "case {case:?}: ListTagsForResource must succeed, got {} with {:?}",
        listed.status,
        listed.text()
    );
    tags_of(&listed)
}

/// A bucket policy granting `s3:GetObject` on `resource` to the principal the row names,
/// under the principal KIND it names it by: `AWS` for the IAM principal of CloudFront's own
/// account, `CanonicalUser` for the `S3CanonicalUserId` the identity was minted with. Both
/// are spellings AWS publishes for an origin access identity.
fn identity_policy(kind: &str, principal: &str, resource: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\
         \"Sid\":\"AllowTheOriginAccessIdentity\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"{kind}\":\"{principal}\"}},\
         \"Action\":\"s3:GetObject\",\"Resource\":\"{resource}\"}}]}}"
    )
}

/// The IAM principal an origin access identity is named by in a bucket policy, as the Amazon
/// S3 User Guide writes it.
fn identity_principal(id: &str) -> String {
    format!("arn:aws:iam::cloudfront:user/CloudFront Origin Access Identity {id}")
}

/// Which spelling a row's bucket policy names the identity by, or no policy at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    CanonicalUser,
    IamPrincipal,
    NoPolicy,
}

/// One row: a private bucket holding one object, a TAGGED distribution over it fronted by an
/// origin access identity, the document the bucket carries — and what the viewer is served.
struct Row {
    case: &'static str,
    /// The bucket, this row's own so one row's grant cannot admit another's read.
    bucket: &'static str,
    key: &'static str,
    body: &'static str,
    tags: &'static [(&'static str, &'static str)],
    grant: Grant,
    /// Whether the viewer is served the object, or the origin's own refusal.
    served: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "two tags, a canonical-user grant and a nested key",
        bucket: "tagged-oai-canonical",
        key: "assets/site/index.html",
        body: "<p>the nested object a tagged distribution serves</p>",
        tags: &[("Environment", "staging"), ("Owner", "platform")],
        grant: Grant::CanonicalUser,
        served: true,
    },
    Row {
        case: "one tag, an IAM-principal grant and a bare key",
        bucket: "tagged-oai-iam-principal",
        key: "index.txt",
        body: "the object behind the identity",
        tags: &[("Environment", "production")],
        grant: Grant::IamPrincipal,
        served: true,
    },
    Row {
        case: "no tags at all, and nothing granted to the identity",
        bucket: "tagged-oai-ungranted",
        key: "index.txt",
        body: "the object nothing was granted",
        tags: &[],
        grant: Grant::NoPolicy,
        served: false,
    },
];

#[tokio::test]
async fn a_tagged_create_deploys_and_serves_its_legacy_origin_access_identity() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // The setup plane: authentication off, so each call carries a credential scope without a
    // computed signature. The VIEWER plane is the authenticating one, which is what makes the
    // credential the origin fetch carries the thing Amazon S3 serves it for.
    let setup = emulator(&engine, false);
    let viewers = emulator(&engine, true);

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        create_bucket(&setup, case, row.bucket).await;
        put_object(&setup, case, row.bucket, row.key, row.body, &[]).await;
        let (identity, canonical) =
            create_origin_access_identity(&setup, case, &format!("tagged-oai-identity-{index}"))
                .await;
        let (id, domain, arn) = create_distribution_with_tags(
            &setup,
            case,
            &format!("tagged-oai-reference-{index}"),
            row.bucket,
            &identity,
            row.tags,
        )
        .await;

        let resource = format!("arn:aws:s3:::{}/*", row.bucket);
        let document = match row.grant {
            Grant::CanonicalUser => Some(identity_policy("CanonicalUser", &canonical, &resource)),
            Grant::IamPrincipal => Some(identity_policy(
                "AWS",
                &identity_principal(&identity),
                &resource,
            )),
            Grant::NoPolicy => None,
        };
        if let Some(document) = &document {
            put_bucket_policy(&setup, case, row.bucket, document).await;
        }

        // The tag set the tagged create stored, read back off the distribution it created.
        let expected: Vec<(String, String)> = row
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        let listed = list_tags(&setup, case, &arn).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: expected the submitted tag set back off the distribution, got {listed:?}"
        );

        wait_until_deployed(&setup, case, &id).await;

        let served = view(&viewers, &domain, &format!("/{}", row.key)).await;
        if row.served {
            assert_eq!(
                (served.status, served.text().as_str()),
                (200, row.body),
                "case {case:?}: expected the object's own bytes through the edge, got {} with \
                 {:?}",
                served.status,
                served.text()
            );
        } else {
            assert_ne!(
                served.status, 200,
                "case {case:?}: a fetch the bucket granted this identity nothing for must not be \
                 served, got {:?}",
                served.text()
            );
            assert!(
                !served.text().contains(row.body),
                "case {case:?}: a refused fetch must carry none of the object, got {:?}",
                served.text()
            );
            assert!(
                served.text().contains(ACCESS_DENIED),
                "case {case:?}: expected the origin's own refusal, got {} with {:?}",
                served.status,
                served.text()
            );
        }
    }
}
