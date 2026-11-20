// SPDX-License-Identifier: CC0-1.0

//! WHICH answer a CloudFront distribution over a PRIVATE Amazon S3 origin gives a viewer for a
//! key the bucket does not hold — over every shape an absent key takes, and over a BURST of
//! repeats of the same request.
//!
//! RULE: a read of a key a bucket does not hold is answered by what the READING PRINCIPAL may
//! learn about that bucket's contents at all — "if you have the `s3:ListBucket` permission on the
//! bucket, Amazon S3 returns an HTTP status code 404 Not Found error. If you don't have the
//! `s3:ListBucket` permission, Amazon S3 returns an HTTP status code 403 Access Denied error"
//! (`GetObject`'s own documentation). So the principal an origin access control makes the fetch
//! as — granted the object read alone by the origin's own document — is refused `403`
//! `AccessDenied` for every key the bucket does not hold, learning nothing about whether any of
//! them is there, and is answered `404` `NoSuchKey` naming the key only once the same document
//! grants it the bucket's listing too. The decision is the policies' and nothing else's, so
//! repeating the request cannot change it.
//!
//! Only a cross-service suite can state this through the EDGE: the distribution, its origin
//! access control and its cache policy are CloudFront's records, the document that scopes the
//! grant and the object it guards are Amazon S3's, and the viewer's answer is what the one made
//! the other produce. The sibling `cloudfront_s3_origin_edge_authorization` suite spans the
//! MECHANISM (an origin access control, a legacy origin access identity) and where the grant
//! comes from, with one absent-key path each; this one spans what the refusal must NOT depend on:
//! the shape of the absent key, and how many times it is asked for.
//!
//! The rows differ along the form of the key the viewer asks for and the bucket does not hold: a
//! bare name, a name under a prefix, a directory-style path, a name a held key EXTENDS, a name
//! differing from a held key in case alone, and a deeply nested path. Each row is asked for
//! [`BURST`] times over, under the managed cache policy whose TTLs are all zero, so every request
//! of a burst is answered by the origin rather than from the edge. Beside them runs a SECOND
//! distribution over the same bucket, named by the same grant, whose read of the key that IS
//! there must keep being served while the bursts run.

use std::sync::Arc;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::MANAGED_CACHE_POLICIES;
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_oac_s3_origin_edge::{
    awaited_deployment, create_bucket, create_distribution, create_origin_access_control, emulator,
    put_bucket_policy, put_object, view, Answer, CLOUDFRONT_PRINCIPAL,
};

/// The code and message Amazon S3 refuses a read nothing granted with, and the code and message
/// it answers a read of an absent key to a caller holding the bucket's listing with (Amazon S3
/// "Error responses").
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_MESSAGE: &str = "Access Denied";
const NO_SUCH_KEY: &str = "NoSuchKey";
const NO_SUCH_KEY_MESSAGE: &str = "The specified key does not exist.";

/// How many times one absent key is asked for. More than one, because what is under test is that
/// the answer belongs to the policies in force rather than to the first fetch; small, because the
/// rule does not count requests.
const BURST: usize = 4;

/// The object both buckets DO hold: the read that must keep being served while the absent keys are
/// asked for.
const HELD: (&str, &str) = ("held.txt", "the object that is there");

/// The bucket whose document grants the origin's principal the object READ alone.
const READ_ONLY_BUCKET: &str = "edge-absent-key-read-only";

/// The bucket whose document grants that principal the read AND the bucket's listing.
const LISTED_BUCKET: &str = "edge-absent-key-listed";

/// The identifier of the AWS-managed cache policy whose TTLs are all zero, read out of the
/// catalog CloudFront publishes rather than written here: every viewer request under it reaches
/// the origin, which is what makes each answer of a burst the origin's own.
fn caching_disabled() -> &'static str {
    MANAGED_CACHE_POLICIES
        .iter()
        .find(|policy| policy.name == "Managed-CachingDisabled")
        .expect("the published catalog lists the caching-disabled policy")
        .id
}

/// One statement of a bucket policy: the actions granted to the CloudFront service principal, the
/// resource they are granted on, and the distribution ARNs the grant is scoped to under
/// `aws:SourceArn` — the condition AWS's own origin-access-control document carries.
fn statement(actions: &[&str], resource: &str, source_arns: &[&str]) -> String {
    let actions = actions
        .iter()
        .map(|action| format!("\"{action}\""))
        .collect::<Vec<String>>()
        .join(",");
    let arns = source_arns
        .iter()
        .map(|arn| format!("\"{arn}\""))
        .collect::<Vec<String>>()
        .join(",");
    format!(
        "{{\"Effect\":\"Allow\",\"Principal\":{{\"Service\":\"{CLOUDFRONT_PRINCIPAL}\"}},\
         \"Action\":[{actions}],\"Resource\":\"{resource}\",\
         \"Condition\":{{\"StringEquals\":{{\"AWS:SourceArn\":[{arns}]}}}}}}"
    )
}

/// A bucket policy carrying `statements`.
fn document(statements: &[String]) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{}]}}",
        statements.join(",")
    )
}

/// One absent key: the case, the path a viewer asks for and the key that path names.
struct AbsentKey {
    case: &'static str,
    path: &'static str,
    key: &'static str,
}

/// The table: one row per form an absent key takes. None of these keys is written to either
/// bucket, and each is a different shape of name — which is what the rule must not depend on.
const ABSENT_KEYS: &[AbsentKey] = &[
    AbsentKey {
        case: "a bare name",
        path: "/absent.txt",
        key: "absent.txt",
    },
    AbsentKey {
        case: "a name under a prefix",
        path: "/assets/absent.js",
        key: "assets/absent.js",
    },
    AbsentKey {
        case: "a directory-style path",
        path: "/tenant/",
        key: "tenant/",
    },
    AbsentKey {
        case: "a name the held key extends",
        path: "/held",
        key: "held",
    },
    AbsentKey {
        case: "a name differing from the held key in case alone",
        path: "/HELD.TXT",
        key: "HELD.TXT",
    },
    AbsentKey {
        case: "a deeply nested path",
        path: "/a/b/c/d/e/absent.json",
        key: "a/b/c/d/e/absent.json",
    },
];

/// What a distribution's viewer must be served for an absent key, which follows from what its
/// origin's document grants the principal the fetch is made as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The refusal of a principal that may not learn the bucket's contents.
    AccessDenied,
    /// The report of an absent key, which only a principal holding the listing is given.
    NoSuchKey,
}

#[tokio::test]
async fn an_absent_key_is_answered_by_what_the_origins_document_grants() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // The setup plane carries a credential scope without a computed signature; the viewer plane
    // authenticates, which is what leaves the signature CloudFront puts on the origin fetch as the
    // only thing that can make the private bucket answer it at all.
    let setup = emulator(&engine, false);
    let viewers = emulator(&engine, true);

    for bucket in [READ_ONLY_BUCKET, LISTED_BUCKET] {
        create_bucket(&setup, "the origin buckets", bucket).await;
        put_object(&setup, "the held object", bucket, HELD.0, HELD.1, &[]).await;
    }

    // Three distributions over two buckets: the one the bursts are driven through, a second one
    // over the SAME bucket that is left serving the key that is there, and one over the bucket
    // whose document grants the listing too.
    let mut created: Vec<(&str, String, String, String)> = Vec::new();
    for (index, (case, bucket)) in [
        ("the read-only grant", READ_ONLY_BUCKET),
        ("the untouched second distribution", READ_ONLY_BUCKET),
        ("the grant that adds the listing", LISTED_BUCKET),
    ]
    .into_iter()
    .enumerate()
    {
        let control = create_origin_access_control(
            &setup,
            case,
            &format!("edge-absent-key-control-{index}"),
            "always",
        )
        .await;
        let (id, domain, arn) = create_distribution(
            &setup,
            case,
            &format!("edge-absent-key-reference-{index}"),
            bucket,
            &control,
            caching_disabled(),
        )
        .await;
        created.push((case, id, domain, arn));
    }

    // The documents are written once every ARN they are scoped to is known — the order a client
    // has to follow for a grant conditioned on the distribution it is made for. The read-only
    // bucket names BOTH of its distributions in one grant, so the second one is admitted by the
    // same statement rather than by one of its own.
    let read_only_arns: Vec<&str> = created
        .iter()
        .take(2)
        .map(|(_case, _id, _domain, arn)| arn.as_str())
        .collect();
    put_bucket_policy(
        &setup,
        "the read-only grant",
        READ_ONLY_BUCKET,
        &document(&[statement(
            &["s3:GetObject"],
            &format!("arn:aws:s3:::{READ_ONLY_BUCKET}/*"),
            &read_only_arns,
        )]),
    )
    .await;
    let listed_arn = created[2].3.clone();
    put_bucket_policy(
        &setup,
        "the grant that adds the listing",
        LISTED_BUCKET,
        &document(&[
            statement(
                &["s3:GetObject"],
                &format!("arn:aws:s3:::{LISTED_BUCKET}/*"),
                &[listed_arn.as_str()],
            ),
            statement(
                &["s3:ListBucket"],
                &format!("arn:aws:s3:::{LISTED_BUCKET}"),
                &[listed_arn.as_str()],
            ),
        ]),
    )
    .await;

    for (case, id, _domain, _arn) in &created {
        awaited_deployment(&setup, case, id).await;
    }
    let read_only_domain = created[0].2.clone();
    let second_domain = created[1].2.clone();
    let listed_domain = created[2].2.clone();

    // The key that IS there, before anything is asked about the keys that are not: both
    // distributions over the read-only bucket serve it, which is the grant the bursts below are
    // judged against.
    for (case, domain) in [
        ("the read-only grant", &read_only_domain),
        ("the untouched second distribution", &second_domain),
    ] {
        assert_held_object(&viewers, case, domain, "before the bursts").await;
    }

    for row in ABSENT_KEYS {
        for (grant, domain, expected) in [
            (
                "the read-only grant",
                &read_only_domain,
                Expected::AccessDenied,
            ),
            (
                "the grant that adds the listing",
                &listed_domain,
                Expected::NoSuchKey,
            ),
        ] {
            let case = format!("{grant} / {}", row.case);
            let mut answers: Vec<Answer> = Vec::new();
            for _ in 0..BURST {
                answers.push(view(&viewers, domain, row.path).await);
            }
            let statuses: Vec<u16> = answers.iter().map(|answer| answer.status).collect();
            let wanted = match expected {
                Expected::AccessDenied => 403,
                Expected::NoSuchKey => 404,
            };
            assert!(
                statuses.iter().all(|status| *status == wanted),
                "case {case:?}: expected every one of {BURST} requests for an absent key to \
                 answer {wanted}, got {statuses:?}"
            );
            for answer in &answers {
                assert_refusal(answer, &case, expected, row.key);
            }
        }
    }

    // And the second distribution is untouched by the bursts: the key that is there is still
    // served through it, so nothing the refusals did changed what the grant admits.
    for (case, domain) in [
        ("the read-only grant", &read_only_domain),
        ("the untouched second distribution", &second_domain),
    ] {
        assert_held_object(&viewers, case, domain, "after the bursts").await;
    }
}

/// Assert that `domain` serves the object the bucket holds, naming the case and the phase.
async fn assert_held_object(
    viewers: &MultiServiceRouter,
    case: &str,
    domain: &str,
    phase: &str,
) {
    let viewed = view(viewers, domain, &format!("/{}", HELD.0)).await;
    assert_eq!(
        (viewed.status, viewed.text().as_str()),
        (200, HELD.1),
        "case {case:?} {phase}: expected the origin's own bytes for the key it holds, got {} \
         with {:?}",
        viewed.status,
        viewed.text()
    );
}

/// Assert that one answer to an absent key is the one `expected` names, naming the case and what
/// came back.
fn assert_refusal(viewed: &Answer, case: &str, expected: Expected, key: &str) {
    match expected {
        Expected::AccessDenied => {
            assert_eq!(
                (
                    viewed.element("Code").as_str(),
                    viewed.element("Message").as_str()
                ),
                (ACCESS_DENIED, ACCESS_DENIED_MESSAGE),
                "case {case:?}: expected the {ACCESS_DENIED} document through the edge, got {:?}",
                viewed.text()
            );
            // A refusal of a principal that may not learn the bucket's contents says nothing
            // about whether the key is one of them.
            assert!(
                !viewed.text().contains(NO_SUCH_KEY) && viewed.element("Key").is_empty(),
                "case {case:?}: a refusal must disclose nothing about the key, got {:?}",
                viewed.text()
            );
            assert!(
                !viewed.text().contains(HELD.1),
                "case {case:?}: a refused read must carry none of the bucket's objects, got {:?}",
                viewed.text()
            );
        }
        Expected::NoSuchKey => {
            assert_eq!(
                (
                    viewed.element("Code").as_str(),
                    viewed.element("Message").as_str(),
                    viewed.element("Key").as_str()
                ),
                (NO_SUCH_KEY, NO_SUCH_KEY_MESSAGE, key),
                "case {case:?}: expected {NO_SUCH_KEY} naming the key asked for, got {:?}",
                viewed.text()
            );
        }
    }
}
