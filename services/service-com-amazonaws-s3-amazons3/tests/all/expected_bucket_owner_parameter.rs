// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the EXPECTED BUCKET OWNER parameter: what a request naming one is
//! answered when the value it names is not an AWS account id.
//!
//! Every assertion drives a real HTTP request through the router (the harness the
//! bucket-lifecycle suite in this crate owns), so what is pinned is the wire behaviour —
//! status, error code, message — rather than any handler's internals.
//!
//! The rule under test, with its source:
//!
//! * `ExpectedBucketOwner` (`x-amz-expected-bucket-owner`) and `ExpectedSourceBucketOwner`
//!   (`x-amz-source-expected-bucket-owner`) both target `AccountId`
//!   (`smithy_query(query="operation",operation="CopyObject")`), a plain string the model
//!   puts no pattern on. A value that is not an AWS account id — an account id is twelve
//!   decimal digits — makes the PARAMETER malformed, so the request is refused for its own
//!   value: HTTP 400 `InvalidBucketOwnerAWSAccountID`, naming the rejected text, before any
//!   owner is compared and with nothing of the addressed resource read or written.
//! * HTTP 403 `AccessDenied` is the OTHER refusal and stays reserved for a WELL-FORMED
//!   account id that is not the owner of the bucket addressed: "if the account ID that you
//!   provide does not match the actual owner of the bucket, the request fails with the HTTP
//!   status code 403 Forbidden (access denied)"
//!   (`GetBucketOwnershipControlsRequest$ExpectedBucketOwner`, the same sentence on every
//!   operation carrying the member).
//! * The rule is the PARAMETER's, not one operation's, so it holds for every operation that
//!   carries it — the rows below span bucket subresource reads, a listing and two deletes —
//!   and the account this emulator serves is refused by neither class.

use super::bucket_lifecycle::{
    create_bucket, get_bucket_policy, put_bucket_policy, router, send, Answer,
};

/// The code and status S3 answers a malformed expected-owner parameter with, and the fixed
/// text its message states before the rejected value.
///
/// Written out here rather than imported so the service and this suite cannot agree on a
/// wrong wording.
const MALFORMED_CODE: &str = "InvalidBucketOwnerAWSAccountID";
const MALFORMED_STATUS: u16 = 400;
const MALFORMED_SENTENCE: &str =
    "The value of the expected bucket owner parameter must be an AWS Account ID...";

/// The code, status and message S3 answers a well-formed account id that is not the bucket's
/// owner with (Amazon S3 "Error responses": "AccessDenied — Access Denied", HTTP 403).
const DENIED_CODE: &str = "AccessDenied";
const DENIED_STATUS: u16 = 403;
const DENIED_MESSAGE: &str = "Access Denied";

/// `value` as it must appear inside the error document's character data: the three
/// characters XML spells as entity references, written as those references.
fn as_character_data(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The message the refusal of `supplied` carries: the fixed sentence, then the rejected value
/// in brackets, taken verbatim from the request.
fn malformed_message(supplied: &str) -> String {
    format!("{MALFORMED_SENTENCE} [{supplied}]")
}

/// Values that are not an AWS account id, differing along HOW they miss the form: the wrong
/// alphabet, one digit too few, one digit too many, a single non-digit among twelve
/// characters, an ARN of the account, and no value at all.
const MALFORMED_VALUES: [(&str, &str); 7] = [
    ("a value of another alphabet entirely", "not-an-account"),
    ("one digit too few", "12345678901"),
    ("one digit too many", "1234567890123"),
    ("twelve characters, one of them not a digit", "12345678901x"),
    (
        "the account's ARN pasted where its id belongs",
        "arn:aws:iam::111122223333:root",
    ),
    ("the parameter left empty", ""),
    // The value is quoted back inside an XML document, so one made of the characters that
    // document is written in must come back as itself and leave the envelope readable.
    ("a value carrying XML metacharacters", "<a&b>"),
];

/// Well-formed account ids that are not the account this emulator serves — the ownership
/// refusal's own class, and the control that the format check did not swallow it.
const FOREIGN_ACCOUNTS: [&str; 3] = ["111122223333", "999988887777", "209876543210"];

/// The operations the parameter is carried on by the rows below: the case name, the method,
/// and the subresource appended to the bucket's own path.
///
/// Spread across the surface the parameter appears on — four bucket subresource reads, a
/// listing of a bucket's contents, and two deletes — because the rule belongs to the
/// parameter rather than to one operation.
const OPERATIONS: [(&str, &str, &str); 7] = [
    ("GetBucketOwnershipControls", "GET", "?ownershipControls"),
    ("GetBucketPolicyStatus", "GET", "?policyStatus"),
    ("GetBucketAcl", "GET", "?acl"),
    ("GetBucketVersioning", "GET", "?versioning"),
    ("ListObjectsV2", "GET", "?list-type=2"),
    ("DeleteBucketPolicy", "DELETE", "?policy"),
    ("DeleteBucketTagging", "DELETE", "?tagging"),
];

/// A policy document granting one named account a read of the bucket's objects — a document
/// that is not public, so it is accepted on a bucket whose Block Public Access switches are
/// the ones every bucket is created with.
fn account_policy(bucket: &str) -> String {
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{}:root", engine::emulator_account_id())},
            "Action": "s3:GetObject",
            "Resource": format!("arn:aws:s3:::{bucket}/*"),
        }],
    })
    .to_string()
}

/// One request carrying `value` as the expected bucket owner.
async fn with_expected_owner(
    router: &smithy_server_runtime::MultiServiceRouter,
    method: &str,
    uri: &str,
    header: &str,
    value: &str,
) -> Answer {
    send(router, method, uri, &[(header, value)], "").await
}

#[tokio::test]
async fn a_malformed_expected_bucket_owner_is_refused_as_a_bad_parameter() {
    let router = router();
    let bucket = "owner-guard-subject";
    let created = create_bucket(&router, bucket, None, None).await;
    assert_eq!(
        created.status, 200,
        "the bucket must be created first, got {} with body {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    // A policy on the bucket, so the two DELETE rows below have something they COULD have
    // removed: a refusal for the parameter must leave it exactly where it was.
    let document = account_policy(bucket);
    let put = put_bucket_policy(&router, bucket, &document).await;
    assert_eq!(
        put.status, 204,
        "the policy must be stored first, got {} with body {:?}",
        put.status,
        String::from_utf8_lossy(&put.body)
    );

    for (operation, method, subresource) in OPERATIONS {
        for (shape, value) in MALFORMED_VALUES {
            let case = format!("{operation} with {shape}");
            let answer = with_expected_owner(
                &router,
                method,
                &format!("/{bucket}{subresource}"),
                "x-amz-expected-bucket-owner",
                value,
            )
            .await;
            assert_eq!(
                answer.status, MALFORMED_STATUS,
                "case {case:?}: expected HTTP {MALFORMED_STATUS}, got {} with body {:?}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                answer.error_code(),
                MALFORMED_CODE,
                "case {case:?}: expected error code {MALFORMED_CODE}, got {:?}",
                answer.error_code()
            );
            assert_eq!(
                answer.error_message(),
                malformed_message(value),
                "case {case:?}: expected the message to name the rejected value, got {:?}",
                answer.error_message()
            );
            // And the document carries that value as character data: the refusal names the
            // rejected text with XML's own characters written as entity references, so the
            // envelope a client parses stays the one envelope.
            let body = String::from_utf8_lossy(&answer.body).into_owned();
            assert!(
                body.contains(&format!("[{}]", as_character_data(value))),
                "case {case:?}: expected the body to carry the rejected value as character \
                 data, got {body:?}"
            );
        }
    }

    // Nothing the refusals touched: the policy is still there, byte for byte.
    let read = get_bucket_policy(&router, bucket).await;
    assert_eq!(
        String::from_utf8_lossy(&read.body),
        document.as_str(),
        "case \"the stored policy after every refusal\": a refused request must write nothing"
    );
}

#[tokio::test]
async fn a_well_formed_account_that_is_not_the_owner_stays_the_ownership_refusal() {
    let router = router();
    let bucket = "owner-guard-foreign";
    let created = create_bucket(&router, bucket, None, None).await;
    assert_eq!(
        created.status, 200,
        "the bucket must be created first, got {} with body {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );

    for (operation, method, subresource) in OPERATIONS {
        for account in FOREIGN_ACCOUNTS {
            let case = format!("{operation} naming the foreign account {account}");
            let answer = with_expected_owner(
                &router,
                method,
                &format!("/{bucket}{subresource}"),
                "x-amz-expected-bucket-owner",
                account,
            )
            .await;
            assert_eq!(
                answer.status, DENIED_STATUS,
                "case {case:?}: expected HTTP {DENIED_STATUS}, got {} with body {:?}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                answer.error_code(),
                DENIED_CODE,
                "case {case:?}: expected error code {DENIED_CODE}, got {:?}",
                answer.error_code()
            );
            assert_eq!(
                answer.error_message(),
                DENIED_MESSAGE,
                "case {case:?}: expected the published sentence, got {:?}",
                answer.error_message()
            );
        }

        // And the account this emulator serves is refused by NEITHER class: whatever each
        // operation then answers about the bucket, it is not one of the two guard refusals.
        let case = format!("{operation} naming the account that owns the bucket");
        let answer = with_expected_owner(
            &router,
            method,
            &format!("/{bucket}{subresource}"),
            "x-amz-expected-bucket-owner",
            engine::emulator_account_id(),
        )
        .await;
        let code = answer.error_code();
        assert!(
            code != MALFORMED_CODE && code != DENIED_CODE,
            "case {case:?}: expected the operation's own answer, got {} with code {code:?}",
            answer.status
        );
    }
}

#[tokio::test]
async fn the_source_expected_bucket_owner_is_judged_by_the_same_rule() {
    let router = router();
    for bucket in ["copy-owner-source", "copy-owner-destination"] {
        let created = create_bucket(&router, bucket, None, None).await;
        assert_eq!(
            created.status, 200,
            "the bucket {bucket} must be created first, got {} with body {:?}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
    }

    // The copy's SOURCE owner carries an account id too, so the same two classes apply to it:
    // the rows differ along the form of the value and along which of the two headers the copy
    // names it in.
    let rows: [(&str, &str, &str, u16, &str); 4] = [
        (
            "a malformed source owner",
            "x-amz-source-expected-bucket-owner",
            "not-an-account",
            MALFORMED_STATUS,
            MALFORMED_CODE,
        ),
        (
            "a malformed destination owner",
            "x-amz-expected-bucket-owner",
            "arn:aws:s3:::copy-owner-destination",
            MALFORMED_STATUS,
            MALFORMED_CODE,
        ),
        (
            "a well-formed foreign source owner",
            "x-amz-source-expected-bucket-owner",
            "111122223333",
            DENIED_STATUS,
            DENIED_CODE,
        ),
        (
            "a well-formed foreign destination owner",
            "x-amz-expected-bucket-owner",
            "111122223333",
            DENIED_STATUS,
            DENIED_CODE,
        ),
    ];
    for (case, header, value, status, code) in rows {
        let answer = send(
            &router,
            "PUT",
            "/copy-owner-destination/copied.txt",
            &[
                ("x-amz-copy-source", "/copy-owner-source/original.txt"),
                (header, value),
            ],
            "",
        )
        .await;
        assert_eq!(
            answer.status, status,
            "case {case:?}: expected HTTP {status}, got {} with body {:?}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            answer.error_code(),
            code,
            "case {case:?}: expected error code {code}, got {:?}",
            answer.error_code()
        );
        if code == MALFORMED_CODE {
            assert_eq!(
                answer.error_message(),
                malformed_message(value),
                "case {case:?}: expected the message to name the rejected value, got {:?}",
                answer.error_message()
            );
        }
    }
}
