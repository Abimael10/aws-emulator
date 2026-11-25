// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ORDER the EXPECTED BUCKET OWNER guard is judged in: whether the
//! bucket a request addresses exists is settled before anything about who owns it is
//! reported.
//!
//! Every assertion drives a real HTTP request through the router (the harness the
//! bucket-lifecycle suite in this crate owns), so what is pinned is the wire behaviour —
//! status, error code, message — rather than any handler's internals.
//!
//! The rule under test, with its source:
//!
//! * `ExpectedBucketOwner` binds `x-amz-expected-bucket-owner` on every bucket and object
//!   operation of this model that carries it
//!   (`smithy_query(query="operation",operation="GetBucketOwnershipControls")`), and what it
//!   asks for is the confused-deputy guard: "if the account ID that you provide does not
//!   match the actual owner of the bucket, the request fails with the HTTP status code 403
//!   Forbidden (access denied)". A bucket has an owner once it EXISTS, so that comparison is
//!   a statement about a bucket that resolved: a request naming a bucket that is not there is
//!   answered about its ABSENCE — `NoSuchBucket`, HTTP 404, "The specified bucket does not
//!   exist" — whatever account the header names, and the error class a caller sees never
//!   becomes a function of the account id it guessed.
//! * The rule belongs to the PARAMETER rather than to one operation, so the rows below span
//!   the surface that carries it: bucket subresource reads, two listings, object reads, a
//!   delete, two writes and a `HEAD` — each answering its own bucket-absent refusal.

use super::bucket_lifecycle::{create_bucket, router, send, Answer};

/// The code, status and message S3 answers a request naming a bucket that does not exist
/// with (Amazon S3 "Error responses": "NoSuchBucket — The specified bucket does not exist",
/// HTTP 404).
///
/// Written out here rather than imported so the service and this suite cannot agree on a
/// wrong wording.
const ABSENT_CODE: &str = "NoSuchBucket";
const ABSENT_STATUS: u16 = 404;
const ABSENT_MESSAGE: &str = "The specified bucket does not exist";

/// The code, status and message the guard itself answers a well-formed account id that is not
/// the owner of a bucket this emulator HOLDS with ("AccessDenied — Access Denied", HTTP 403).
const DENIED_CODE: &str = "AccessDenied";
const DENIED_STATUS: u16 = 403;
const DENIED_MESSAGE: &str = "Access Denied";

/// A document setting the one Object Ownership value a bucket of this emulator reports, for
/// the `PUT` row below: what matters is that the write is refused for the ABSENT bucket, so
/// the document itself has to be one the operation would otherwise accept.
const OWNERSHIP_CONTROLS: &str = "<OwnershipControls><Rule><ObjectOwnership>\
                                  BucketOwnerEnforced</ObjectOwnership></Rule>\
                                  </OwnershipControls>";

/// One operation the parameter is carried on: the case name, the method, what follows the
/// bucket in the request's path, and the body it carries.
type Row = (&'static str, &'static str, &'static str, &'static str);

/// The operations the rows below drive, spread across the surface `x-amz-expected-bucket-owner`
/// appears on — four bucket subresource reads, two listings, two object reads, a delete and
/// two writes — because the rule belongs to the parameter rather than to one operation.
const OPERATIONS: [Row; 11] = [
    ("GetBucketOwnershipControls", "GET", "?ownershipControls", ""),
    ("GetBucketPolicyStatus", "GET", "?policyStatus", ""),
    ("GetBucketAcl", "GET", "?acl", ""),
    ("GetBucketVersioning", "GET", "?versioning", ""),
    ("ListObjectsV2", "GET", "?list-type=2", ""),
    ("ListMultipartUploads", "GET", "?uploads", ""),
    ("GetObject", "GET", "/reports/january.pdf", ""),
    ("GetObjectAcl", "GET", "/reports/january.pdf?acl", ""),
    ("DeleteBucketTagging", "DELETE", "?tagging", ""),
    ("PutObject", "PUT", "/reports/january.pdf", "january"),
    (
        "PutBucketOwnershipControls",
        "PUT",
        "?ownershipControls",
        OWNERSHIP_CONTROLS,
    ),
];

/// The values the expected-owner header carries, row by row: no header at all (the control
/// the other three must answer exactly as), the account this emulator serves, and two
/// well-formed account ids of other accounts.
///
/// A well-formed account id is twelve decimal digits, so the foreign ones differ from each
/// other and from the served account in their digits alone.
fn owner_cases() -> [(&'static str, Option<String>); 4] {
    [
        ("no expected owner at all", None),
        (
            "the account this emulator serves",
            Some(engine::emulator_account_id().to_string()),
        ),
        ("a foreign account", Some("111122223333".to_string())),
        (
            "another foreign account",
            Some("999988887777".to_string()),
        ),
    ]
}

/// One request against `bucket`, carrying `owner` as the expected bucket owner when the case
/// names one.
async fn request(
    router: &smithy_server_runtime::MultiServiceRouter,
    bucket: &str,
    (_, method, suffix, body): Row,
    owner: Option<&str>,
) -> Answer {
    let headers: Vec<(&str, &str)> = owner
        .map(|value| vec![("x-amz-expected-bucket-owner", value)])
        .unwrap_or_default();
    send(
        router,
        method,
        &format!("/{bucket}{suffix}"),
        &headers,
        body,
    )
    .await
}

#[tokio::test]
async fn a_bucket_that_was_never_created_is_absent_whatever_owner_the_request_expects() {
    let router = router();
    // Nothing is created: every request below names a bucket this emulator does not hold.
    let bucket = "owner-guard-never-created";

    for row in OPERATIONS {
        let (operation, ..) = row;
        for (shape, owner) in owner_cases() {
            let case = format!("{operation} with {shape}");
            let answer = request(&router, bucket, row, owner.as_deref()).await;
            assert_eq!(
                answer.status, ABSENT_STATUS,
                "case {case:?}: expected HTTP {ABSENT_STATUS}, got {} with body {:?}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                answer.error_code(),
                ABSENT_CODE,
                "case {case:?}: expected error code {ABSENT_CODE}, got {:?}",
                answer.error_code()
            );
            assert_eq!(
                answer.error_message(),
                ABSENT_MESSAGE,
                "case {case:?}: expected the published sentence, got {:?}",
                answer.error_message()
            );
        }
    }
}

#[tokio::test]
async fn a_head_of_an_absent_bucket_keeps_its_own_status_whatever_owner_is_expected() {
    let router = router();
    let bucket = "owner-guard-never-created-head";

    // A `HEAD` carries no body, so the STATUS is the whole of what tells a caller which
    // refusal it was ("A message body isn't included, so you can't determine the exception
    // beyond these HTTP response codes" — `HeadBucket`'s own documentation). Of the three it
    // documents, 404 is the one that means the bucket is not there, and it must not turn into
    // the guard's 403 because of the header's value.
    for (shape, owner) in owner_cases() {
        let case = format!("HeadBucket with {shape}");
        let answer = request(&router, bucket, ("HeadBucket", "HEAD", "", ""), owner.as_deref())
            .await;
        assert_eq!(
            answer.status, ABSENT_STATUS,
            "case {case:?}: expected HTTP {ABSENT_STATUS}, got {}",
            answer.status
        );
    }
}

#[tokio::test]
async fn a_bucket_that_exists_still_refuses_a_foreign_owner_and_serves_its_own_account() {
    let router = router();
    let bucket = "owner-guard-created";
    let created = create_bucket(&router, bucket, None, None).await;
    assert_eq!(
        created.status, 200,
        "the bucket must be created first, got {} with body {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );

    for row in OPERATIONS {
        let (operation, ..) = row;
        // A well-formed id that is not the owner of a bucket that DOES resolve is the
        // ownership refusal, unchanged: that is the comparison the guard exists for.
        for account in ["111122223333", "999988887777"] {
            let case = format!("{operation} on an existing bucket naming {account}");
            let answer = request(&router, bucket, row, Some(account)).await;
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

        // And naming the account that owns it leaves the operation to answer for itself:
        // whatever that answer is, it is neither guard refusal.
        let case = format!("{operation} on an existing bucket naming its owner");
        let answer = request(
            &router,
            bucket,
            row,
            Some(engine::emulator_account_id()),
        )
        .await;
        let code = answer.error_code();
        assert!(
            code != DENIED_CODE && code != ABSENT_CODE,
            "case {case:?}: expected the operation's own answer, got {} with code {code:?}",
            answer.status
        );
    }

    // The positive control the ownership-controls read itself answers: the bucket resolves,
    // the header names its owner, so the read is SERVED — the one Object Ownership value a
    // bucket of this emulator is created with.
    let served = request(
        &router,
        bucket,
        OPERATIONS[0],
        Some(engine::emulator_account_id()),
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case \"GetBucketOwnershipControls on the existing bucket\": expected HTTP 200, got {} \
         with body {:?}",
        served.status,
        String::from_utf8_lossy(&served.body)
    );
    assert_eq!(
        served.elements("ObjectOwnership"),
        vec!["BucketOwnerEnforced".to_string()],
        "case \"GetBucketOwnershipControls on the existing bucket\": expected the bucket's own \
         rules, got {:?}",
        String::from_utf8_lossy(&served.body)
    );
}

#[tokio::test]
async fn a_copy_judges_each_expected_owner_against_its_own_bucket() {
    let router = router();
    let destination = "owner-guard-copy-destination";
    let source = "owner-guard-copy-source";
    let created = create_bucket(&router, destination, None, None).await;
    assert_eq!(
        created.status, 200,
        "the destination bucket must be created first, got {} with body {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );

    // A copy names TWO buckets — the destination in its address and the source in
    // `x-amz-copy-source` — and states an expected owner for each
    // (`x-amz-expected-bucket-owner`, `x-amz-source-expected-bucket-owner`). Each guard is a
    // statement about ITS OWN bucket, so each is judged against that bucket's existence.
    let foreign = "111122223333";
    let copy = |source_bucket: &'static str, into: &'static str, header: &'static str| {
        let router = &router;
        async move {
            send(
                router,
                "PUT",
                &format!("/{into}/copied.txt"),
                &[
                    (
                        "x-amz-copy-source",
                        &format!("/{source_bucket}/original.txt") as &str,
                    ),
                    (header, foreign),
                ],
                "",
            )
            .await
        }
    };

    // The SOURCE bucket does not exist yet: the copy is answered about that absence, not
    // about who was expected to own it.
    let absent_source = copy(source, destination, "x-amz-source-expected-bucket-owner").await;
    assert_eq!(
        absent_source.status, ABSENT_STATUS,
        "case \"a foreign source owner for a source bucket that is not there\": expected HTTP \
         {ABSENT_STATUS}, got {} with body {:?}",
        absent_source.status,
        String::from_utf8_lossy(&absent_source.body)
    );
    assert_eq!(
        absent_source.error_code(),
        ABSENT_CODE,
        "case \"a foreign source owner for a source bucket that is not there\": expected error \
         code {ABSENT_CODE}, got {:?}",
        absent_source.error_code()
    );

    // Once that bucket exists, the same request is the ownership refusal: the guard has a
    // bucket to compare an owner for.
    let created_source = create_bucket(&router, source, None, None).await;
    assert_eq!(
        created_source.status, 200,
        "the source bucket must be created, got {} with body {:?}",
        created_source.status,
        String::from_utf8_lossy(&created_source.body)
    );
    let present_source = copy(source, destination, "x-amz-source-expected-bucket-owner").await;
    assert_eq!(
        present_source.status, DENIED_STATUS,
        "case \"a foreign source owner for a source bucket that exists\": expected HTTP \
         {DENIED_STATUS}, got {} with body {:?}",
        present_source.status,
        String::from_utf8_lossy(&present_source.body)
    );
    assert_eq!(
        present_source.error_code(),
        DENIED_CODE,
        "case \"a foreign source owner for a source bucket that exists\": expected error code \
         {DENIED_CODE}, got {:?}",
        present_source.error_code()
    );

    // And the DESTINATION half answers the same way about its own bucket.
    let absent_destination =
        copy(source, "owner-guard-copy-absent", "x-amz-expected-bucket-owner").await;
    assert_eq!(
        absent_destination.status, ABSENT_STATUS,
        "case \"a foreign destination owner for a destination that is not there\": expected \
         HTTP {ABSENT_STATUS}, got {} with body {:?}",
        absent_destination.status,
        String::from_utf8_lossy(&absent_destination.body)
    );
    assert_eq!(
        absent_destination.error_code(),
        ABSENT_CODE,
        "case \"a foreign destination owner for a destination that is not there\": expected \
         error code {ABSENT_CODE}, got {:?}",
        absent_destination.error_code()
    );
    let present_destination =
        copy(source, destination, "x-amz-expected-bucket-owner").await;
    assert_eq!(
        present_destination.status, DENIED_STATUS,
        "case \"a foreign destination owner for a destination that exists\": expected HTTP \
         {DENIED_STATUS}, got {} with body {:?}",
        present_destination.status,
        String::from_utf8_lossy(&present_destination.body)
    );
    assert_eq!(
        present_destination.error_code(),
        DENIED_CODE,
        "case \"a foreign destination owner for a destination that exists\": expected error \
         code {DENIED_CODE}, got {:?}",
        present_destination.error_code()
    );
}

#[tokio::test]
async fn the_absent_bucket_answer_is_the_same_document_whatever_owner_is_expected() {
    let router = router();
    let bucket = "owner-guard-invariance";

    // The INVARIANT the rule is: the answer a request about an absent bucket earns does not
    // depend on the account its header named, so every case answers the document the request
    // carrying no header at all answers.
    for row in OPERATIONS {
        let (operation, ..) = row;
        let control = request(&router, bucket, row, None).await;
        let expected = (control.status, control.error_code());
        for (shape, owner) in owner_cases() {
            let case = format!("{operation} with {shape}");
            let answer = request(&router, bucket, row, owner.as_deref()).await;
            assert_eq!(
                (answer.status, answer.error_code()),
                expected,
                "case {case:?}: expected the same answer as the request naming no owner \
                 ({expected:?}), got {:?}",
                (answer.status, answer.error_code())
            );
        }
    }
}
