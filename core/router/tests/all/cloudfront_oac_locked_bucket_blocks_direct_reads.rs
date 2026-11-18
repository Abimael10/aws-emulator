// SPDX-License-Identifier: CC0-1.0

//! An Amazon S3 bucket LOCKED to one CloudFront distribution: both halves of what an origin
//! access control is for, across CloudFront and Amazon S3 on one engine.
//!
//! RULE: a bucket whose only grant of `s3:GetObject` is to the `cloudfront.amazonaws.com`
//! service principal, bound by `aws:SourceArn` to one distribution, serves that
//! distribution's edge — a viewer request at the distribution's own `DomainName` is answered
//! `200` with the object's exact bytes, because the edge's origin fetch is signed as that
//! principal acting for that distribution — and refuses a credential-free direct read of the
//! same object at the S3 REST endpoint with HTTP `403` and an `<Error>` document whose
//! `<Code>` is `AccessDenied` and whose `<Message>` is `Access Denied`, carrying none of the
//! object's bytes. Neither half depends on the bucket's Block Public Access configuration:
//! the edge's fetch is signed, and the anonymous read is refused because nothing grants it to
//! everyone rather than because a switch blocked it. The owner's own credentialled read of
//! the same object stays served throughout, which is what tells an authorization refusal from
//! an object that is not there.
//!
//! Only a cross-service suite can state this pair: the distribution, its published domain
//! name and the origin access control are CloudFront's records, while the object, the
//! document that locks it and the public-access switches are Amazon S3's — and the subject is
//! that exactly one of the two readers is served.
//!
//! The sibling `cloudfront_oac_s3_origin_edge` suite tables WHICH documents admit the edge's
//! fetch and says nothing about a direct reader; the S3 crate's own `anonymous_object_reads`
//! suite tables what an unsigned caller is served and knows nothing of a distribution.
//! Neither can state that the one bucket serves the edge and refuses everyone else.
//!
//! The VIEWER and the ANONYMOUS reads are driven through a router with request authentication
//! ON, which is what makes the signature the subject: neither request carries a credential of
//! any kind, so the only thing that can make S3 serve the edge's fetch is the signature
//! CloudFront puts on it, and the direct reader must still be answered in S3's own words. The
//! setup calls go through a second router over the SAME engine with authentication off, so
//! each one carries a credential scope (which is what names the Region) without a real
//! signature.
//!
//! The rows span what the rule generalises over: the Block Public Access configuration in
//! force (every switch on, the two policy switches alone, every switch off, none ever put),
//! the clause that binds the grant to the distribution (`StringEquals` on either spelling AWS
//! publishes for the condition key, an `ArnLike` pattern), and the object's key form and
//! bytes. Each row's anonymous read is driven at both addressings of the REST endpoint,
//! path-style and virtual-hosted-style, because where a read was addressed is not part of
//! what authorizes it.

use std::time::Duration;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_oac_s3_origin_edge::{
    call, create_bucket, create_distribution, create_origin_access_control, emulator,
    origin_domain, policy, put_bucket_policy, put_object, regional_endpoint, view, Answer,
    API_VERSION, CLOUDFRONT_HOST, CLOUDFRONT_PRINCIPAL,
};

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each row's answer is that fetch's own rather
/// than one held at the edge.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The code and the message Amazon S3 refuses a read nothing granted with (Amazon S3 "Error
/// responses"). Both are entirely fixed text: an unsigned read names no principal, so there
/// is nothing of the request in either.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_MESSAGE: &str = "Access Denied";

/// What the edge reports as the outcome of an answer the ORIGIN produced for this request.
const MISS: &str = "Miss from cloudfront";

/// Every Block Public Access switch, in the model's own member order
/// (`smithy_query(query="shape",shape="PublicAccessBlockConfiguration")`).
const SETTINGS: [&str; 4] = [
    "BlockPublicAcls",
    "IgnorePublicAcls",
    "BlockPublicPolicy",
    "RestrictPublicBuckets",
];

/// The XML namespace an S3 request payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// How long the suite waits for the clock to deploy a distribution, and how often it looks.
/// The wait is on the CONDITION — the status CloudFront reports — with a deadline far past the
/// propagation any configuration is owed.
const POLL: Duration = Duration::from_millis(25);
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// Poll `GetDistribution` until CloudFront first reports `id` deployed, which is the state a
/// viewer request is served in.
async fn awaited_deployment(router: &MultiServiceRouter, case: &str, id: &str) {
    let polls = POLL_DEADLINE.as_millis() / POLL.as_millis().max(1);
    for _ in 0..polls {
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
    panic!(
        "case {case:?}: CloudFront never reported the distribution {id} deployed within \
         {POLL_DEADLINE:?}"
    );
}

/// `s3:PutPublicAccessBlock` with the four switches of [`SETTINGS`] set to `flags`.
async fn put_public_access_block(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    flags: [bool; 4],
) {
    let mut body = format!("<PublicAccessBlockConfiguration xmlns=\"{S3_XMLNS}\">");
    for (setting, value) in SETTINGS.iter().zip(flags) {
        body.push_str(&format!("<{setting}>{value}</{setting}>"));
    }
    body.push_str("</PublicAccessBlockConfiguration>");
    let put = call(
        router,
        "s3",
        &regional_endpoint(),
        "PUT",
        &format!("/{bucket}?publicAccessBlock"),
        &[("content-type", "application/xml")],
        Bytes::from(body),
    )
    .await;
    assert!(
        (200..300).contains(&put.status),
        "case {case:?}: PutPublicAccessBlock({flags:?}) must succeed, got {} with {:?}",
        put.status,
        put.text()
    );
}

/// `s3:GetObject` carrying the owner's own credential scope — the positive control that says
/// the object is there and readable, so a refusal of the unsigned read below is an
/// authorization answer rather than a missing object.
async fn credentialled_read(
    router: &MultiServiceRouter,
    bucket: &str,
    key: &str,
) -> Answer {
    call(
        router,
        "s3",
        &regional_endpoint(),
        "GET",
        &format!("/{bucket}/{key}"),
        &[],
        Bytes::new(),
    )
    .await
}

/// One row: a bucket locked to one distribution, how its public access is configured, and
/// which clause binds the grant to that distribution.
struct Row {
    case: &'static str,
    /// The bucket, which is this row's own so one row's grants cannot admit another's read.
    bucket: &'static str,
    key: &'static str,
    body: &'static str,
    /// The four [`SETTINGS`] switches to put on the bucket, or `None` for a bucket whose
    /// public access configuration is never put at all.
    blocks: Option<[bool; 4]>,
    /// The `Condition` clause of the bucket's grant, built from the distribution's own ARN
    /// once CloudFront has minted it.
    condition: fn(&str) -> String,
}

/// The table: one row per public-access posture and per form of the clause that binds the
/// grant to the distribution.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "every public access switch on, bound by StringEquals on AWS:SourceArn",
            bucket: "oac-locked-all-switches-on",
            key: "asset.txt",
            body: "the locked object",
            blocks: Some([true, true, true, true]),
            condition: |arn| format!("{{\"StringEquals\":{{\"AWS:SourceArn\":\"{arn}\"}}}}"),
        },
        Row {
            case: "every public access switch on, bound by the aws:SourceArn spelling, under \
                   a nested prefix",
            bucket: "oac-locked-lower-case-key",
            key: "reports/2026/q1.csv",
            body: "quarter,revenue\n1,41\n",
            blocks: Some([true, true, true, true]),
            condition: |arn| format!("{{\"StringEquals\":{{\"aws:SourceArn\":\"{arn}\"}}}}"),
        },
        Row {
            case: "only the two policy switches on, bound by an ArnLike pattern",
            bucket: "oac-locked-policy-switches",
            key: "index.html",
            body: "<p>locked behind the edge</p>",
            blocks: Some([false, false, true, true]),
            condition: |_arn| {
                "{\"ArnLike\":{\"aws:SourceArn\":\"arn:aws:cloudfront::*:distribution/*\"}}"
                    .to_string()
            },
        },
        Row {
            case: "every public access switch off, bound by StringEquals on AWS:SourceArn",
            bucket: "oac-locked-all-switches-off",
            key: "deep/nested/page.txt",
            body: "nothing is blocked and nothing is public",
            blocks: Some([false, false, false, false]),
            condition: |arn| format!("{{\"StringEquals\":{{\"AWS:SourceArn\":\"{arn}\"}}}}"),
        },
        Row {
            case: "a bucket whose public access configuration was never put",
            bucket: "oac-locked-never-configured",
            key: "unconfigured.bin",
            body: "\u{1}\u{2}no public access call",
            blocks: None,
            condition: |arn| format!("{{\"StringEquals\":{{\"aws:SourceArn\":\"{arn}\"}}}}"),
        },
    ]
}

/// What one row left behind for the reading phase.
struct Prepared {
    case: &'static str,
    bucket: &'static str,
    key: &'static str,
    body: &'static str,
    domain: String,
    id: String,
}

#[tokio::test]
async fn an_oac_locked_bucket_serves_the_edge_and_refuses_every_direct_reader() {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    // The setup plane: authentication off, so each call carries a credential scope without a
    // computed signature. The viewer and the anonymous reads below go through the
    // authenticating plane.
    let setup = emulator(&engine, false);
    let readers = emulator(&engine, true);

    let mut prepared: Vec<Prepared> = Vec::new();
    for (index, row) in rows().into_iter().enumerate() {
        create_bucket(&setup, row.case, row.bucket).await;
        if let Some(flags) = row.blocks {
            put_public_access_block(&setup, row.case, row.bucket, flags).await;
        }
        put_object(&setup, row.case, row.bucket, row.key, row.body, &[]).await;
        let control = create_origin_access_control(
            &setup,
            row.case,
            &format!("oac-locked-{index}"),
            "always",
        )
        .await;
        let (id, domain, arn) = create_distribution(
            &setup,
            row.case,
            &format!("oac-locked-reference-{index}"),
            row.bucket,
            &control,
            CACHING_DISABLED,
        )
        .await;
        // The document is written AFTER the distribution exists, because it is bound to that
        // distribution's own ARN — the order a client has to follow.
        put_bucket_policy(
            &setup,
            row.case,
            row.bucket,
            &policy(
                CLOUDFRONT_PRINCIPAL,
                "s3:GetObject",
                &format!("arn:aws:s3:::{}/*", row.bucket),
                Some(&(row.condition)(&arn)),
            ),
        )
        .await;
        prepared.push(Prepared {
            case: row.case,
            bucket: row.bucket,
            key: row.key,
            body: row.body,
            domain,
            id,
        });
    }

    for row in &prepared {
        awaited_deployment(&setup, row.case, &row.id).await;

        // The edge half: the viewer presents nothing, and is served the origin's own bytes
        // because CloudFront signed the fetch as the principal the document grants.
        let viewed = view(&readers, &row.domain, &format!("/{}", row.key)).await;
        assert_eq!(
            viewed.status,
            200,
            "case {:?}: expected the locked object through the edge, got {} with {:?}",
            row.case,
            viewed.status,
            viewed.text()
        );
        assert_eq!(
            viewed.text(),
            row.body,
            "case {:?}: expected the ORIGIN's own bytes, got {:?}",
            row.case,
            viewed.text()
        );
        assert_eq!(
            viewed.header("x-cache"),
            MISS,
            "case {:?}: expected the answer to be the origin's, got X-Cache {:?}",
            row.case,
            viewed.header("x-cache")
        );

        // The positive control: the owner's own credentialled read is served the same bytes,
        // so the refusals below are authorization answers and not a missing object.
        let owner = credentialled_read(&setup, row.bucket, row.key).await;
        assert_eq!(
            owner.status,
            200,
            "case {:?}: the owner's own read must be served, got {} with {:?}",
            row.case,
            owner.status,
            owner.text()
        );
        assert_eq!(
            owner.text(),
            row.body,
            "case {:?}: the owner's own read must serve the stored bytes, got {:?}",
            row.case,
            owner.text()
        );

        // The direct half, at both addressings of the REST endpoint: a credential-free read
        // is refused in S3's own vocabulary, and carries none of the object.
        for (addressing, host, path) in [
            (
                "path-style",
                regional_endpoint(),
                format!("/{}/{}", row.bucket, row.key),
            ),
            (
                "virtual-hosted-style",
                origin_domain(row.bucket),
                format!("/{}", row.key),
            ),
        ] {
            let direct = view(&readers, &host, &path).await;
            assert_eq!(
                direct.status,
                403,
                "case {:?} ({addressing}): expected HTTP 403 from a credential-free read, got \
                 {} with {:?}",
                row.case,
                direct.status,
                direct.text()
            );
            assert_eq!(
                direct.element("Code"),
                ACCESS_DENIED,
                "case {:?} ({addressing}): expected {ACCESS_DENIED}, got {:?}",
                row.case,
                direct.text()
            );
            assert_eq!(
                direct.element("Message"),
                ACCESS_DENIED_MESSAGE,
                "case {:?} ({addressing}): expected {ACCESS_DENIED_MESSAGE:?}, got {:?}",
                row.case,
                direct.element("Message")
            );
            assert!(
                !direct.text().contains(row.body),
                "case {:?} ({addressing}): a refused read must not carry the object's bytes, \
                 got {:?}",
                row.case,
                direct.text()
            );
        }
    }
}
