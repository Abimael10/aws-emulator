// SPDX-License-Identifier: CC0-1.0

//! The FORM a bucket policy reads back in: what `PutBucketPolicy` stores and
//! `GetBucketPolicy` reports for a document a caller submitted.
//!
//! Every assertion drives a real HTTP request through the router, so what is pinned is the
//! wire behaviour (the bytes of the policy payload) rather than any handler's internals.
//!
//! THE RULE: Amazon S3 holds the POLICY a submitted document expresses, not the bytes it
//! arrived as, so a read reports the policy language's canonical rendering of it — the
//! whitespace BETWEEN the JSON tokens dropped, an element the language types "String or
//! Array of strings" (`Action`/`NotAction`, `Resource`/`NotResource`, and the principal
//! lists inside `Principal`/`NotPrincipal`) written as that string where the caller
//! submitted a one-element array of one string, and an ACCOUNT principal named by its
//! account id alone written as that account's root ARN, which is the other spelling of the
//! very same principal. Everything else is the caller's own data and survives untouched:
//! member order, statement count, the `Statement` element's own form, the type and
//! spelling of every other value, and every byte inside a string literal. The rendering is
//! a FIXED POINT, which is what lets a client put back the document it read and store the
//! same bytes again.
//!
//! The table below spans what the rule generalises over — WHICH element carries the
//! one-value list, how many values it carries, which spelling an account principal arrives
//! in, whether the document is indented, and whether its `Statement` is a list or a bare
//! object — rather than the one document any single client submits. Each row is asserted
//! twice: once for the document the read reports, and once for the re-put of that document,
//! which must store the same bytes.

use super::bucket_lifecycle::{create_bucket, get_bucket_policy, put_bucket_policy, router};
use smithy_server_runtime::MultiServiceRouter;

/// The status a successful `PutBucketPolicy` answers with, from the operation itself rather
/// than written down twice.
const PUT_STATUS: u16 =
    service_com_amazonaws_s3_amazons3::ops::put_bucket_policy::PUT_BUCKET_POLICY_SUCCESS_STATUS;

/// The object ARN every document below grants over. Fixed, because WHICH resource a
/// statement names is not what the rendering rule is about — only how the element carrying
/// it is written.
const RESOURCE: &str = "arn:aws:s3:::ledger/obj/*";

/// Another account, for the row whose principal list names two: a 12-digit account id is an
/// account principal whichever account it is.
const ANOTHER_ACCOUNT: &str = "210987654321";

/// A canonical user id: "the canonical user ID is a 64-character obfuscated identifier"
/// (Amazon S3 User Guide), so this is 64 alphanumerics.
const CANONICAL_USER: &str = "7a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9";

/// One row: what a failing assertion calls it, the document the caller submits, and the
/// document every later read of the bucket's policy must report, byte for byte.
struct Row {
    case: &'static str,
    submitted: String,
    reported: String,
}

/// The account whose policies these are — this emulator's own, read from the one place that
/// states it.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// That account's root ARN: the spelling a policy store writes an account principal in.
fn account_root() -> String {
    format!("arn:aws:iam::{}:root", account())
}

/// The whole table — see the module docs for what it spans.
fn rows() -> Vec<Row> {
    let account = account();
    let root = account_root();
    let another_root = format!("arn:aws:iam::{ANOTHER_ACCOUNT}:root");
    vec![
        Row {
            case: "an account principal named by its account id alone",
            submitted: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"ById\",\"Effect\":\
                 \"Allow\",\"Principal\":{{\"AWS\":\"{account}\"}},\"Action\":\"s3:GetObject\",\
                 \"Resource\":\"{RESOURCE}\"}}]}}"
            ),
            reported: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"ById\",\"Effect\":\
                 \"Allow\",\"Principal\":{{\"AWS\":\"{root}\"}},\"Action\":\"s3:GetObject\",\
                 \"Resource\":\"{RESOURCE}\"}}]}}"
            ),
        },
        Row {
            case: "an account principal already named by its root ARN",
            submitted: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"ByArn\",\"Effect\":\
                 \"Allow\",\"Principal\":{{\"AWS\":\"{root}\"}},\"Action\":\"s3:GetObject\",\
                 \"Resource\":\"{RESOURCE}\"}}]}}"
            ),
            reported: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"ByArn\",\"Effect\":\
                 \"Allow\",\"Principal\":{{\"AWS\":\"{root}\"}},\"Action\":\"s3:GetObject\",\
                 \"Resource\":\"{RESOURCE}\"}}]}}"
            ),
        },
        Row {
            case: "an Action submitted as a one-element list",
            submitted: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":\"{root}\"}},\"Action\":[\"s3:GetObject\"],\
                 \"Resource\":\"{RESOURCE}\"}}]}}"
            ),
            reported: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":\"{root}\"}},\"Action\":\"s3:GetObject\",\
                 \"Resource\":\"{RESOURCE}\"}}]}}"
            ),
        },
        Row {
            case: "an Action naming two values, which is no one-value list",
            submitted: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":\"{root}\"}},\"Action\":[\"s3:GetObject\",\
                 \"s3:PutObject\"],\"Resource\":\"{RESOURCE}\"}}]}}"
            ),
            reported: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":\"{root}\"}},\"Action\":[\"s3:GetObject\",\
                 \"s3:PutObject\"],\"Resource\":\"{RESOURCE}\"}}]}}"
            ),
        },
        Row {
            case: "a Resource submitted as a one-element list",
            submitted: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":\"{root}\"}},\"Action\":\"s3:GetObject\",\
                 \"Resource\":[\"{RESOURCE}\"]}}]}}"
            ),
            reported: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":\"{root}\"}},\"Action\":\"s3:GetObject\",\
                 \"Resource\":\"{RESOURCE}\"}}]}}"
            ),
        },
        Row {
            case: "an AWS principal list holding one account id",
            submitted: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":[\"{account}\"]}},\"Action\":\"s3:GetObject\",\
                 \"Resource\":\"{RESOURCE}\"}}]}}"
            ),
            reported: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":\"{root}\"}},\"Action\":\"s3:GetObject\",\
                 \"Resource\":\"{RESOURCE}\"}}]}}"
            ),
        },
        Row {
            case: "an AWS principal list holding two accounts, one in each spelling",
            submitted: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":[\"{ANOTHER_ACCOUNT}\",\"{root}\"]}},\"Action\":\
                 \"s3:GetObject\",\"Resource\":\"{RESOURCE}\"}}]}}"
            ),
            reported: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":[\"{another_root}\",\"{root}\"]}},\"Action\":\
                 \"s3:GetObject\",\"Resource\":\"{RESOURCE}\"}}]}}"
            ),
        },
        Row {
            case: "a Service principal list holding one service",
            submitted: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"Service\":[\"cloudfront.amazonaws.com\"]}},\"Action\":\
                 \"s3:GetObject\",\"Resource\":\"{RESOURCE}\"}}]}}"
            ),
            reported: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"Service\":\"cloudfront.amazonaws.com\"}},\"Action\":\
                 \"s3:GetObject\",\"Resource\":\"{RESOURCE}\"}}]}}"
            ),
        },
        Row {
            case: "a CanonicalUser principal list holding one id, which is no account id",
            submitted: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"CanonicalUser\":[\"{CANONICAL_USER}\"]}},\"Action\":\
                 \"s3:GetObject\",\"Resource\":\"{RESOURCE}\"}}]}}"
            ),
            reported: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"CanonicalUser\":\"{CANONICAL_USER}\"}},\"Action\":\
                 \"s3:GetObject\",\"Resource\":\"{RESOURCE}\"}}]}}"
            ),
        },
        Row {
            case: "a Federated principal list holding one provider",
            submitted: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"Federated\":[\"accounts.google.com\"]}},\"Action\":\
                 \"s3:GetObject\",\"Resource\":\"{RESOURCE}\"}}]}}"
            ),
            reported: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"Federated\":\"accounts.google.com\"}},\"Action\":\
                 \"s3:GetObject\",\"Resource\":\"{RESOURCE}\"}}]}}"
            ),
        },
        Row {
            // The complements of the three elements above, in a `Deny` — the effect a
            // `NotPrincipal` is written under. The statement also states an `Action`
            // because that is the action element this emulator's grammar requires of every
            // statement (`state::is_valid_policy_document`).
            case: "a Deny whose NotPrincipal, NotAction and NotResource each hold one value",
            submitted: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Deny\",\
                 \"NotPrincipal\":{{\"AWS\":[\"{account}\"]}},\"Action\":\"s3:*\",\
                 \"NotAction\":[\"s3:ListBucket\"],\"NotResource\":[\"{RESOURCE}\"]}}]}}"
            ),
            reported: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Deny\",\
                 \"NotPrincipal\":{{\"AWS\":\"{root}\"}},\"Action\":\"s3:*\",\
                 \"NotAction\":\"s3:ListBucket\",\"NotResource\":\"{RESOURCE}\"}}]}}"
            ),
        },
        Row {
            case: "a document indented the way a client writes one by hand",
            submitted: format!(
                "{{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": [\n    {{\n      \
                 \"Sid\": \"two words\",\n      \"Effect\": \"Allow\",\n      \"Principal\": \
                 {{\n        \"AWS\": \"{account}\"\n      }},\n      \"Action\": [\n        \
                 \"s3:GetObject\"\n      ],\n      \"Resource\": \"{RESOURCE}\"\n    }}\n  \
                 ]\n}}\n"
            ),
            reported: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"two words\",\
                 \"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{root}\"}},\"Action\":\
                 \"s3:GetObject\",\"Resource\":\"{RESOURCE}\"}}]}}"
            ),
        },
        Row {
            // Member ORDER is the caller's, and so is every byte inside a string literal —
            // including one that spells a JSON list.
            case: "members in the caller's own order, and a Sid literal that spells a list",
            submitted: format!(
                "{{\"Statement\":[{{\"Sid\":\"grants [\\\"s3:GetObject\\\"] to the \
                 account\",\"Action\":\"s3:GetObject\",\"Effect\":\"Allow\",\"Resource\":\
                 \"{RESOURCE}\",\"Principal\":{{\"AWS\":\"{account}\"}}}}],\"Version\":\
                 \"2012-10-17\"}}"
            ),
            reported: format!(
                "{{\"Statement\":[{{\"Sid\":\"grants [\\\"s3:GetObject\\\"] to the \
                 account\",\"Action\":\"s3:GetObject\",\"Effect\":\"Allow\",\"Resource\":\
                 \"{RESOURCE}\",\"Principal\":{{\"AWS\":\"{root}\"}}}}],\"Version\":\
                 \"2012-10-17\"}}"
            ),
        },
        Row {
            // `Statement` is not a string-or-array element: a document that states one
            // statement as a bare object keeps it, and a one-element list keeps its list
            // (every row above).
            case: "a Statement submitted as a bare object rather than a list",
            submitted: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":\"{account}\"}},\"Action\":[\"s3:GetObject\"],\
                 \"Resource\":\"{RESOURCE}\"}}}}"
            ),
            reported: format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":\"{root}\"}},\"Action\":\"s3:GetObject\",\
                 \"Resource\":\"{RESOURCE}\"}}}}"
            ),
        },
    ]
}

/// The policy `bucket` carries, as the payload bytes `GetBucketPolicy` answered.
async fn reported_policy(router: &MultiServiceRouter, case: &str, bucket: &str) -> String {
    let read = get_bucket_policy(router, bucket).await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetBucketPolicy must serve the policy, got {} with body {:?}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    String::from_utf8_lossy(&read.body).to_string()
}

#[tokio::test]
async fn a_bucket_policy_reads_back_in_the_rendering_of_the_policy_it_expresses() {
    let router = router();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let bucket = format!("policy-rendering-{index}");
        let created = create_bucket(&router, &bucket, None, None).await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: the bucket must be created, got {} with body {:?}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );

        let put = put_bucket_policy(&router, &bucket, &row.submitted).await;
        assert_eq!(
            put.status,
            PUT_STATUS,
            "case {case:?}: PutBucketPolicy must accept the document, got {} with body {:?}",
            put.status,
            String::from_utf8_lossy(&put.body)
        );
        let reported = reported_policy(&router, case, &bucket).await;
        assert_eq!(
            reported, row.reported,
            "case {case:?}: expected the policy to read back as {:?}, got {reported:?}",
            row.reported
        );

        // And the rendering is a FIXED POINT: putting back the document that was read
        // stores the same bytes, so a client that refreshes and re-applies sees no drift.
        let again = put_bucket_policy(&router, &bucket, &reported).await;
        assert_eq!(
            again.status,
            PUT_STATUS,
            "case {case:?}: a re-put of the reported document must be accepted, got {} with \
             body {:?}",
            again.status,
            String::from_utf8_lossy(&again.body)
        );
        let reread = reported_policy(&router, case, &bucket).await;
        assert_eq!(
            reread, row.reported,
            "case {case:?}: a re-put of the reported document must store the same bytes, got \
             {reread:?}"
        );
    }
}
