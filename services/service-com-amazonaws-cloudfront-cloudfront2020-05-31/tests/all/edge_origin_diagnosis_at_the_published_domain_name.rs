// SPDX-License-Identifier: CC0-1.0

//! Diagnosing — and repairing — an ORIGIN FAULT from where a client observes one: a socket
//! dialled at the address a deployed distribution published as its `DomainName`, with Amazon S3
//! mounted beside CloudFront on one engine so the answer that comes back is a real object
//! store's.
//!
//! RULE: `Distribution$DomainName` is "the distribution's CloudFront domain name. For example:
//! d111111abcdef8.cloudfront.net" — the value a client DIALS — so what a distribution publishes
//! has to be an address this emulator ANSWERS that distribution's edge on, reported identically
//! by every read of the distribution; and a viewer that dials it is served the response of the
//! matching cache behaviour's ORIGIN FETCH for the path it asked for: the origin's object when
//! the origin serves it, and the origin's own error response — its status, its code and its
//! envelope — when the origin refuses it or holds nothing there. Repairing the origin therefore
//! changes what that same address serves, with nothing said to CloudFront at all.
//!
//! That pair is the whole of an origin-error lifecycle: a client cannot diagnose a fault it
//! cannot observe, and it cannot confirm a repair it cannot re-observe. A published name in a
//! zone Amazon Web Services serves its own endpoints in resolves to nothing on the machine this
//! emulator runs on, so a client handed one opens no connection at all and every edge-visible
//! symptom — the origin's refusal among them — is invisible while the control plane reports the
//! distribution deployed.
//!
//! The rows span what the rule generalises over: WHAT the origin answers the first viewer
//! request with — the object; the refusal of a read nothing granted; the refusal of a read of a
//! key the bucket does not hold, which is what a grant of the read ALONE earns ("if the object
//! that you request doesn't exist, the error that Amazon S3 returns depends on whether you also
//! have the `s3:ListBucket` permission", `GetObject`); the not-found the same absent key earns
//! where the grant DOES carry that permission; and the not-found of a bucket that does not exist
//! — crossed with the form of the key (a tenant prefix, a bare name) and the grant the bucket's
//! policy makes. Each faulted row is then REPAIRED through the origin's own API and re-observed
//! at the same address, so the row states both halves of the lifecycle.
//!
//! Both schemes a viewer may dial are observed: the `https://<DomainName>/<key>` a client
//! composes from the published value, through a stock TLS client, and the same request in the
//! clear — where the answer's `X-Cache` says the edge served it and WHICH of the origin and the
//! cache produced it.
//!
//! Every error leg reads the ORIGIN's own envelope off the answer, which CloudFront's own
//! refusal body does not carry: without that, a leg would pass against an edge that refused the
//! request itself and never asked the origin. The topology, the staging and the two dialling
//! clients are the siblings' own — [`crate::edge_signed_s3_origin`] owns the two services over
//! one engine, the bucket staging, the policy write and the origin access control,
//! [`crate::edge_origin_response_headers`] the create over an S3 origin, and
//! [`crate::edge_lambda_url_origin`] the viewer clients — reused rather than copied. What is new
//! here is the origin FAULT observed through the published address, and its repair.

use crate::distribution_lifecycle::{
    awaited_deployment, get_distribution, is_published_edge_address,
};
use crate::edge_lambda_url_origin::{dial_over_tls, Answered, Edge};
use crate::edge_origin_response_headers::{create_distribution_over_bucket, MISS};
use crate::edge_signed_access::{CACHING_DISABLED, ERROR};
use crate::edge_signed_s3_origin::{
    create_origin_access_control, grant, Emulator, ORIGIN_ENVELOPE_MEMBER,
};

/// The service principal an origin access control makes CloudFront's origin request as, and so
/// the principal the bucket's policy grants the listing to in [`allowing_the_read_and_listing`].
/// The read-only document is [`grant`]'s, which names the same principal.
const CLOUDFRONT_PRINCIPAL: &str = "cloudfront.amazonaws.com";

/// The object a bucket holds while the key the viewer asks for is absent, so the bucket in such
/// a row EXISTS and holds content — the fault is the one key, not the bucket.
const DECOY: (&str, &str) = ("placeholder.html", "<p>another object entirely</p>");

/// What the origin bucket's policy grants the distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Granted {
    /// Nothing at all: the bucket carries no policy.
    Nothing,
    /// `s3:GetObject` on the bucket's objects, scoped to this distribution — the document AWS
    /// publishes for an origin access control, and the whole of what a client following it
    /// writes.
    TheRead,
    /// That read AND `s3:ListBucket` on the bucket, the pair that decides what a read of a key
    /// the bucket does not hold is answered with.
    TheReadAndTheListing,
}

/// The policy document for `granted` on `bucket`, scoped to the distribution `arn` names.
fn policy(granted: Granted, bucket: &str, arn: &str) -> Option<String> {
    match granted {
        Granted::Nothing => None,
        Granted::TheRead => Some(grant(bucket, arn)),
        Granted::TheReadAndTheListing => Some(allowing_the_read_and_listing(bucket, arn)),
    }
}

/// [`grant`]'s document with a second statement allowing `s3:ListBucket` on the bucket itself —
/// the permission `GetObject`'s documentation makes the answer to a read of an absent key depend
/// on. Both statements are scoped by `AWS:SourceArn` to the one distribution, as the published
/// origin-access-control document is.
fn allowing_the_read_and_listing(bucket: &str, arn: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\
         \"Sid\":\"AllowCloudFrontServicePrincipalRead\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{CLOUDFRONT_PRINCIPAL}\"}},\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{bucket}/*\",\
         \"Condition\":{{\"StringEquals\":{{\"AWS:SourceArn\":\"{arn}\"}}}}}},{{\
         \"Sid\":\"AllowCloudFrontServicePrincipalList\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{CLOUDFRONT_PRINCIPAL}\"}},\
         \"Action\":\"s3:ListBucket\",\"Resource\":\"arn:aws:s3:::{bucket}\",\
         \"Condition\":{{\"StringEquals\":{{\"AWS:SourceArn\":\"{arn}\"}}}}}}]}}"
    )
}

/// What the ORIGIN answers a relayed read while the row's fault stands: the status, the error
/// code, and the `X-Cache` the edge reports for an answer of that status.
///
/// The codes are Amazon S3's own, from its published error-response list — `AccessDenied` for a
/// read its policies leave to nobody, `NoSuchKey` for "the specified key does not exist",
/// `NoSuchBucket` for "the specified bucket does not exist" — so each leg is held to the
/// vocabulary of the service that produced the answer rather than to CloudFront's.
///
/// `X-Cache` differs with the status because CloudFront "caches ... responses with the following
/// status codes: 200, 203, 204, 206, 300, 301, 302, 303, 304, 307, 308, 404, 405, 410, 414, and
/// 501": an answer carrying one of them reports WHICH of the origin and the cache produced it,
/// and every other error answer reports only that it is an error.
#[derive(Debug, Clone, Copy)]
struct OriginError {
    status: u16,
    code: &'static str,
    x_cache: &'static str,
}

/// One row: an origin bucket, the object that belongs in it, what the bucket's policy grants,
/// and what the first viewer request therefore meets.
struct Row {
    case: &'static str,
    bucket: &'static str,
    /// The key every viewer request of this row asks for, and the bytes the object holds once
    /// the origin is whole.
    key: &'static str,
    body: &'static str,
    /// Whether anything holds the bucket the origin's `DomainName` addresses.
    bucket_exists: bool,
    /// Whether that bucket holds an object under [`Row::key`].
    holds_the_object: bool,
    granted: Granted,
    /// What the origin answers while the fault stands, or `None` for a row with no fault.
    origin_error: Option<OriginError>,
}

/// The table: every class of answer an S3 origin gives a relayed read, over both forms of key
/// and every grant a bucket's policy may make.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an object the origin serves, under a tenant prefix",
            bucket: "cf-origin-diagnosis-served",
            key: "tenant-alpha/index.html",
            body: "<p>tenant alpha, served by the origin</p>",
            bucket_exists: true,
            holds_the_object: true,
            granted: Granted::TheRead,
            origin_error: None,
        },
        Row {
            case: "an object the origin serves at a bare key",
            bucket: "cf-origin-diagnosis-bare",
            key: "index.html",
            body: "<p>the bare object, served by the origin</p>",
            bucket_exists: true,
            holds_the_object: true,
            granted: Granted::TheReadAndTheListing,
            origin_error: None,
        },
        Row {
            case: "a key the bucket does not hold, granted the read alone",
            bucket: "cf-origin-diagnosis-absent-key",
            key: "tenant-beta/index.html",
            body: "<p>tenant beta, put there by the repair</p>",
            bucket_exists: true,
            holds_the_object: false,
            granted: Granted::TheRead,
            origin_error: Some(OriginError {
                status: 403,
                code: "AccessDenied",
                x_cache: ERROR,
            }),
        },
        Row {
            case: "a key the bucket does not hold, granted the read and the listing",
            bucket: "cf-origin-diagnosis-absent-key-listed",
            key: "tenant-beta/index.html",
            body: "<p>tenant beta again, put there by the repair</p>",
            bucket_exists: true,
            holds_the_object: false,
            granted: Granted::TheReadAndTheListing,
            origin_error: Some(OriginError {
                status: 404,
                code: "NoSuchKey",
                x_cache: MISS,
            }),
        },
        Row {
            case: "an origin bucket that granted the distribution nothing",
            bucket: "cf-origin-diagnosis-ungranted",
            key: "tenant-gamma/index.html",
            body: "<p>tenant gamma, held behind a missing grant</p>",
            bucket_exists: true,
            holds_the_object: true,
            granted: Granted::Nothing,
            origin_error: Some(OriginError {
                status: 403,
                code: "AccessDenied",
                x_cache: ERROR,
            }),
        },
        Row {
            case: "an origin naming a bucket that does not exist",
            bucket: "cf-origin-diagnosis-absent-bucket",
            key: "tenant-delta/index.html",
            body: "<p>tenant delta, in a bucket the repair creates</p>",
            bucket_exists: false,
            holds_the_object: false,
            granted: Granted::Nothing,
            origin_error: Some(OriginError {
                status: 404,
                code: "NoSuchBucket",
                x_cache: MISS,
            }),
        },
    ]
}

/// What one row left behind for the viewer phase.
struct Prepared {
    case: &'static str,
    bucket: &'static str,
    key: &'static str,
    body: &'static str,
    origin_error: Option<OriginError>,
    /// The distribution's id, its published `DomainName` and its ARN — the ARN being what the
    /// bucket's grant is scoped to.
    id: String,
    domain: String,
    arn: String,
}

impl Prepared {
    /// The published name and the address it is dialled at. They are ONE value here: what this
    /// emulator publishes IS the address it answers that distribution's edge on, which is the
    /// first half of the rule and is asserted as such below.
    fn edge(&self) -> Edge {
        Edge {
            domain: self.domain.clone(),
            address: Some(self.domain.clone()),
        }
    }

    /// The viewer path every leg of this row asks for.
    fn viewer_path(&self) -> String {
        format!("/{}", self.key)
    }
}

/// Stage one row: the bucket and whatever it holds, the origin access control the origin is
/// fronted by, the distribution over it, and the grant the row's policy makes.
async fn prepare(emulator: &Emulator, index: usize, row: &Row) -> Prepared {
    let case = row.case;
    if row.bucket_exists {
        let (key, body) = if row.holds_the_object {
            (row.key, row.body)
        } else {
            DECOY
        };
        emulator.staged_object(case, row.bucket, key, body).await;
    }
    let control = create_origin_access_control(
        &emulator.setup,
        case,
        &format!("origin-diagnosis-{index}"),
    )
    .await;
    let (id, domain, arn) = create_distribution_over_bucket(
        &emulator.setup,
        case,
        &format!("origin-diagnosis-{index}"),
        row.bucket,
        &control,
        // Nothing is held in the edge cache under this policy, so each leg below observes what
        // the origin answers NOW — which is what makes a repair observable at the same address
        // without any further call to CloudFront.
        &format!("<CachePolicyId>{CACHING_DISABLED}</CachePolicyId>"),
    )
    .await;
    // The grant is written AFTER the distribution exists, because it is scoped to that
    // distribution's own ARN — the order a client has to follow — and only on a bucket that
    // exists, since nothing else can carry a policy.
    if row.bucket_exists {
        if let Some(document) = policy(row.granted, row.bucket, &arn) {
            emulator.staged_policy(case, row.bucket, &document).await;
        }
    }
    Prepared {
        case,
        bucket: row.bucket,
        key: row.key,
        body: row.body,
        origin_error: row.origin_error,
        id,
        domain,
        arn,
    }
}

/// Make the row's origin WHOLE through the ORIGIN's own API, with nothing said to CloudFront:
/// the object is put where the viewer asks for it — in the bucket, creating it where nothing
/// held one — and the bucket grants the distribution the read.
async fn repair(emulator: &Emulator, row: &Prepared) {
    emulator
        .staged_object(row.case, row.bucket, row.key, row.body)
        .await;
    emulator
        .staged_policy(row.case, row.bucket, &grant(row.bucket, &row.arn))
        .await;
}

/// Assert that `answer` is the object the origin holds, served by the edge for this request.
fn assert_serves_the_object(answer: &Answered, case: &str, leg: &str, body: &str) {
    assert_eq!(
        answer.status, 200,
        "case {case:?} ({leg}): expected the origin's object through the edge, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.body, body,
        "case {case:?} ({leg}): expected the object's own bytes {body:?}, got {:?}",
        answer.body
    );
    assert!(
        answer.reports_cache(MISS),
        "case {case:?} ({leg}): expected the edge to report {MISS:?} for an answer the origin \
         produced, got {:?}",
        answer.stated("x-cache")
    );
}

/// Assert that `answer` is the ORIGIN's own error for this request, relayed by the edge.
fn assert_relays_the_origin_error(
    answer: &Answered,
    case: &str,
    leg: &str,
    expected: OriginError,
    object_body: &str,
) {
    assert_eq!(
        answer.status, expected.status,
        "case {case:?} ({leg}): expected the origin's status {}, got {} with {:?}",
        expected.status, answer.status, answer.body
    );
    assert!(
        answer
            .body
            .contains(&format!("<Code>{}</Code>", expected.code)),
        "case {case:?} ({leg}): expected the origin's error code {:?}, got {:?}",
        expected.code,
        answer.body
    );
    // The document is the ORIGIN's, not CloudFront's own refusal of the same class: it carries a
    // member CloudFront's error body — a code and a message alone — does not have at all.
    assert!(
        answer.body.contains(ORIGIN_ENVELOPE_MEMBER),
        "case {case:?} ({leg}): expected the ORIGIN's own error document, which carries \
         {ORIGIN_ENVELOPE_MEMBER:?}, got {:?}",
        answer.body
    );
    assert!(
        !answer.body.contains(object_body),
        "case {case:?} ({leg}): a refused read must not carry the object's bytes, got {:?}",
        answer.body
    );
    assert!(
        answer.reports_cache(expected.x_cache),
        "case {case:?} ({leg}): expected the edge to report {:?} for an answer of status {}, got \
         {:?}",
        expected.x_cache,
        expected.status,
        answer.stated("x-cache")
    );
}

/// The whole lifecycle, over the table: a client holding only the published `DomainName` dials
/// it, observes what the origin answers — its object, or its own error — repairs the origin, and
/// observes the repair at that same address.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_published_domain_name_serves_the_origins_answer_and_its_errors() {
    let emulator = Emulator::new();

    // Every row's distribution is staged first, so the propagation each one is owed runs for all
    // of them at once rather than one row at a time.
    let mut prepared: Vec<Prepared> = Vec::new();
    for (index, row) in rows().iter().enumerate() {
        prepared.push(prepare(&emulator, index, row).await);
    }

    // The first half of the rule, before a viewer request is sent: the published value is an
    // address a client here can dial, and every read of the distribution reports that same
    // value.
    for row in &prepared {
        let case = row.case;
        assert!(
            is_published_edge_address(&row.domain),
            "case {case:?}: expected an address a client can dial (host:port), got {:?}",
            row.domain
        );
        let read = get_distribution(&emulator.setup, &row.id).await;
        let reported = read.root().find_text("DomainName").unwrap_or_default();
        assert_eq!(
            reported, row.domain,
            "case {case:?}: expected GetDistribution to report the published address {:?}, got \
             {reported:?}",
            row.domain
        );
    }

    for row in &prepared {
        let case = row.case;
        // A configuration that has not reached the edge locations serves no viewer, and the
        // CLOCK owes that propagation: the condition is polled for, never a span waited out.
        awaited_deployment(&emulator.setup, case, &row.id).await;
        let edge = row.edge();
        let path = row.viewer_path();

        // 1. The request a client composes from the published value, over TLS, as the viewer of
        //    a distribution does: `https://<DomainName>/<key>`.
        let (status, body) = dial_over_tls(
            case,
            &row.domain,
            format!("https://{}{path}", row.domain),
            None,
        )
        .await;
        match row.origin_error {
            None => {
                assert_eq!(
                    (status, body.as_str()),
                    (200, row.body),
                    "case {case:?} (dialled over TLS): expected the origin's object at the \
                     published address, got {status} with {body:?}"
                );
            }
            Some(expected) => {
                assert_eq!(
                    status, expected.status,
                    "case {case:?} (dialled over TLS): expected the origin's status {}, got \
                     {status} with {body:?}",
                    expected.status
                );
                assert!(
                    body.contains(&format!("<Code>{}</Code>", expected.code)),
                    "case {case:?} (dialled over TLS): expected the origin's error code {:?}, \
                     got {body:?}",
                    expected.code
                );
            }
        }

        // 2. The same request in the clear, where the answer's own headers say the edge served
        //    it and which of the origin and the cache produced it.
        let answered = Answered::of_stating(case, &edge, "GET", &path, &[]).await;
        match row.origin_error {
            None => assert_serves_the_object(&answered, case, "dialled in the clear", row.body),
            Some(expected) => assert_relays_the_origin_error(
                &answered,
                case,
                "dialled in the clear",
                expected,
                row.body,
            ),
        }

        // 3. The repair, made through the ORIGIN's own API with nothing said to CloudFront, and
        //    re-observed at the same address: the fault was the origin's, so fixing it is all
        //    that stands between the client and the object.
        if row.origin_error.is_some() {
            repair(&emulator, row).await;
            let repaired = Answered::of_stating(case, &edge, "GET", &path, &[]).await;
            assert_serves_the_object(&repaired, case, "after the origin was repaired", row.body);
        }
    }
}
