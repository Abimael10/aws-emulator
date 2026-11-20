// SPDX-License-Identifier: CC0-1.0

//! The allow-list write and read loop, end to end over the wire.
//!
//! Every case drives the production `MultiServiceRouter` through the harness
//! `macie_session_and_custom_data_identifiers` builds, so what is pinned is the
//! HTTP behaviour and the response BODIES rather than a handler called
//! directly. The suite is the second one over this plane: where
//! `allow_list_criteria_and_status` pins one criterion's translation and the
//! status derived from it, these cases pin the RESOURCE — the identity a create
//! mints, what a read reports back of what was stored, and which region and
//! which session a list belongs to.
//!
//! The rules under test, each asserted over a SET of inputs rather than one
//! request:
//!
//! * `CreateAllowList` PERSISTS a list under a server-assigned id in the id
//!   grammar the model pins, so `GetAllowList` reports every member the request
//!   carried, and a retried `clientToken` replays the first list instead of
//!   minting a second one;
//! * criteria "must specify a regular expression (regex) or an S3 object
//!   (s3WordsList). It can't specify both", so criteria naming two alternatives
//!   or none are refused 400 `ValidationException` and nothing is stored, and so
//!   are criteria naming an S3 object the object store holds nothing at;
//! * `status` reports whether Macie can access and use the list's criteria: for
//!   a regex, whether the expression compiled, and for an S3 object whatever
//!   object storage answers for that object right now;
//! * an allow list belongs to the region its create addressed, and both
//!   operations are refused 403 until Macie is enabled there;
//! * `UpdateAllowList` REPLACES the settings of the list it addresses, and is
//!   refused when its criteria specify the wrong number of criteria, the other
//!   KIND of criterion from the stored one, or an S3 object nothing holds;
//! * `DeleteAllowList` is refused while classification jobs that are not done
//!   are configured to use the list, unless the request asks for those checks
//!   to be skipped, and once a list is deleted every later call addressing it
//!   reports it missing.

use std::sync::Arc;

use service_com_amazonaws_macie2_macie2 as macie2;
use smithy_server_runtime::MultiServiceRouter;

use super::classification_jobs::{
    create as create_job, describe as describe_job, scheduled as job_scheduled,
};
use super::macie_session_and_custom_data_identifiers::{
    call_in, emulator, emulator_with_engine, enable_in, error_code, error_message,
    reported_instant,
};

/// The collection's URI, from `CreateAllowList`'s `@http` binding.
const ALLOW_LISTS_URI: &str = "/allow-lists";

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The regions the cases sign for: a rule about an allow list is not a rule
/// about one region, so every table below runs in each of them.
const REGIONS: [&str; 2] = ["us-west-2", "eu-central-1"];

/// The region the object-storage cases sign for. A bucket created with no
/// `LocationConstraint` lands here, so Macie and the objects its criteria name
/// are in the same Region.
const OBJECT_STORAGE_REGION: &str = "us-east-1";

/// A SigV4 `Authorization` header in the `s3` scope — the routing signal that
/// picks the S3 service for a request to the bucket front door.
const S3_AUTH: &str = "AWS4-HMAC-SHA256 Credential=AKIDTESTS3/20260101/us-east-1/s3/\
                       aws4_request, SignedHeaders=host, Signature=abcdef";

/// The `id` grammar the model pins for an allow list:
/// `CreateAllowListResponse$id` is `^[a-z0-9]{22}$`.
const ID_LEN: usize = 22;

/// How the service refuses a call addressing an allow list the region holds
/// none of. Fixed text with no variable part, written out here rather than read
/// from the crate so this expectation can fail.
const NOT_FOUND_MESSAGE: &str =
    "Input validation failed. The request specifies an allow list that does not exist.";

/// The `JobStatus` of a recurring job awaiting its next scheduled run, which is
/// what a job that has started no run carries. An entry of the modelled
/// `JobStatus` enum (`smithy_query(query="shape",shape="JobStatus")`).
const JOB_IDLE: &str = "IDLE";

/// The `JobStatus` of a job the caller paused. An entry of that same enum.
const JOB_USER_PAUSED: &str = "USER_PAUSED";

/// The `JobStatus` of a job the caller cancelled. An entry of that same enum,
/// and one of the two statuses the delete check treats as done.
const JOB_CANCELLED: &str = "CANCELLED";

/// How the service refuses a delete while `jobs` classification jobs that are
/// not done are configured to use the list. Everything but the count is fixed
/// text; the count comes from the jobs the store holds.
fn in_use_message(jobs: usize) -> String {
    format!(
        "The allow list can't be deleted. {jobs} jobs are configured to use the list and \
         those jobs have a status other than Complete or Cancelled."
    )
}

/// A body carrying one `clientToken`, which `CreateAllowList` requires.
fn create_body(token: &str, name: &str, criteria: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "clientToken": token, "name": name, "criteria": criteria })
}

/// Criteria naming a regular expression.
fn regex_criteria(regex: &str) -> serde_json::Value {
    serde_json::json!({ "regex": regex })
}

/// Criteria naming an S3 object.
fn words_list_criteria(bucket: &str, key: &str) -> serde_json::Value {
    serde_json::json!({ "s3WordsList": { "bucketName": bucket, "objectKey": key } })
}

/// Create one allow list, requiring the documented 200, and hand back the
/// reported body.
async fn create(
    router: &MultiServiceRouter,
    region: &str,
    body: &serde_json::Value,
    case: &str,
) -> serde_json::Value {
    let (status, reply) = call_in(router, region, "POST", ALLOW_LISTS_URI, &body.to_string()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a create inside every bound must answer 200, got {status} with {reply:?}"
    );
    reply
}

/// Read one allow list back, requiring the documented 200.
async fn read(
    router: &MultiServiceRouter,
    region: &str,
    id: &str,
    case: &str,
) -> serde_json::Value {
    let (status, record) =
        call_in(router, region, "GET", &format!("{ALLOW_LISTS_URI}/{id}"), "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: a read of a stored allow list must answer 200, got {status} with \
         {record:?}"
    );
    record
}

/// Assert `id` is in the grammar the model pins for an allow list's id: 22
/// lowercase alphanumerics.
fn assert_id_grammar(id: &str, case: &str) {
    assert_eq!(
        id.chars().count(),
        ID_LEN,
        "case {case:?}: an allow list's id is {ID_LEN} characters, got {id:?}"
    );
    assert!(
        id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
        "case {case:?}: an allow list's id is lowercase alphanumeric, got {id:?}"
    );
}

/// A router holding Macie AND S3 over one shared engine — what the assembled
/// emulator does — so an `s3WordsList`'s status is grounded in S3's own answer
/// rather than in anything this suite seeded.
///
/// Macie is mounted by the harness every other case builds
/// (`emulator_with_engine`), so the two forms cannot drift apart on how this
/// service is mounted; only the object store beside it is added here.
fn emulator_with_object_storage() -> MultiServiceRouter {
    let (mut router, engine) = emulator_with_engine();
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// One request to the emulator's S3 front door, path-style because the emulator
/// serves one endpoint per Region. Answers the HTTP status.
async fn s3_call(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> u16 {
    let host = format!("s3.{OBJECT_STORAGE_REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", S3_AUTH)
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the S3 harness request is well formed");
    router.dispatch(request).await.status().as_u16()
}

/// `PUT /<bucket>[/<key>]` — `CreateBucket`, or `PutObject` with `body`.
async fn s3_put(router: &MultiServiceRouter, path: &str, body: &[u8]) -> u16 {
    s3_call(router, "PUT", path, body).await
}

/// `CreateAllowList` stores every member the request carried under an id in the
/// grammar the model pins, and `GetAllowList` reports them back. The rows span
/// what a create may carry: each kind of criteria, with and without the
/// optional `description` and `tags`.
#[tokio::test]
async fn a_created_allow_list_reads_back_with_every_stored_member() {
    let router = emulator();

    let cases: [(&str, serde_json::Value); 5] = [
        (
            "a regex, required members only",
            serde_json::json!({
                "clientToken": "token-regex-minimal",
                "name": "minimal",
                "criteria": regex_criteria("[0-9]{3}-[0-9]{2}-[0-9]{4}"),
            }),
        ),
        (
            "a regex with a description",
            serde_json::json!({
                "clientToken": "token-regex-described",
                "name": "described",
                "description": "ignores test account numbers",
                "criteria": regex_criteria("ACCT-[0-9]+"),
            }),
        ),
        (
            "a regex with every optional member",
            serde_json::json!({
                "clientToken": "token-regex-everything",
                "name": "everything",
                "description": "all members",
                "criteria": regex_criteria("^[a-z]+@example\\.com$"),
                "tags": { "team": "security", "env": "test" },
            }),
        ),
        (
            "an S3 object, required members only",
            serde_json::json!({
                "clientToken": "token-words-minimal",
                "name": "words",
                "criteria": words_list_criteria("macie-allow-lists", "lists/ignore.txt"),
            }),
        ),
        (
            "an S3 object with every optional member",
            serde_json::json!({
                "clientToken": "token-words-everything",
                "name": "words everything",
                "description": "ignores the words in an object",
                "criteria": words_list_criteria("macie-allow-lists", "nested/key with spaces.txt"),
                "tags": { "team": "security" },
            }),
        ),
    ];

    let mut minted: Vec<String> = Vec::new();
    for region in REGIONS {
        enable_in(&router, region, "{}").await;
        for (name, body) in &cases {
            let case = format!("{name} in {region}");
            let reply = create(&router, region, body, &case).await;

            let id = reply["id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a create must report an id, got {reply:?}"))
                .to_string();
            assert_id_grammar(&id, &case);
            assert!(
                !minted.contains(&id),
                "case {case:?}: {id:?} was already minted for another create, among {minted:?}"
            );
            let expected_arn = format!("arn:aws:macie2:{region}:{ACCOUNT}:allow-list/{id}");
            assert_eq!(
                reply["arn"].as_str(),
                Some(expected_arn.as_str()),
                "case {case:?}: expected the ARN {expected_arn:?}, got {:?}",
                reply["arn"]
            );

            let record = read(&router, region, &id, &case).await;
            for member in ["id", "arn"] {
                assert_eq!(
                    record[member], reply[member],
                    "case {case:?}: the read must report the {member} the create did, got {record:?}"
                );
            }
            for member in ["name", "description", "criteria", "tags"] {
                let expected = body.get(member).cloned();
                assert_eq!(
                    record.get(member).cloned().filter(|v| !v.is_null()),
                    expected,
                    "case {case:?}: expected {member} {expected:?}, got {:?}",
                    record.get(member)
                );
            }
            // Both instants are reported, in the wire form the model pins, and
            // a list nothing has changed since it was created carries the same
            // instant in each.
            let created = reported_instant(&case, "createdAt", &record);
            let updated = reported_instant(&case, "updatedAt", &record);
            assert_eq!(
                created, updated,
                "case {case:?}: a list nothing has changed reports one instant in both \
                 members, got {created} and {updated}"
            );

            // A second read reports the same record: what the create stored is
            // read back, not synthesised per call.
            let again = read(&router, region, &id, &case).await;
            for member in ["id", "arn", "name", "description", "criteria", "tags", "createdAt"] {
                assert_eq!(
                    again.get(member),
                    record.get(member),
                    "case {case:?}: two reads must report the same {member}, got {:?} then {:?}",
                    record.get(member),
                    again.get(member)
                );
            }
            minted.push(id);
        }
    }
}

/// The criteria's own rule: exactly one alternative. Criteria naming both, or
/// neither, are refused 400 `ValidationException` and leave nothing in the
/// store.
#[tokio::test]
async fn criteria_must_specify_exactly_one_alternative() {
    let (router, engine) = emulator_with_engine();
    let region = REGIONS[0];
    enable_in(&router, region, "{}").await;

    let refused: [(&str, serde_json::Value); 3] = [
        (
            "both alternatives",
            serde_json::json!({
                "regex": "[0-9]+",
                "s3WordsList": { "bucketName": "macie-lists", "objectKey": "ignore.txt" },
            }),
        ),
        (
            "both alternatives, the other way round",
            serde_json::json!({
                "s3WordsList": { "bucketName": "other-lists", "objectKey": "deep/ignore.txt" },
                "regex": "^$",
            }),
        ),
        ("neither alternative", serde_json::json!({})),
    ];

    for (case, criteria) in &refused {
        let body = create_body(&format!("token-{case}"), "refused", criteria.clone());
        let (status, reply) =
            call_in(&router, region, "POST", ALLOW_LISTS_URI, &body.to_string()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {reply:?}"
        );
        assert!(
            !error_message(&reply).is_empty(),
            "case {case:?}: a refusal must say what is wrong, got {reply:?}"
        );
    }

    // A refused create leaves NOTHING behind: the store holds no allow list.
    let held = engine
        .state::<macie2::ComAmazonawsMacie2Macie2State>()
        .lock()
        .allow_lists
        .len();
    assert_eq!(
        held, 0,
        "a refused create must store nothing, got {held} allow list(s)"
    );

    // And criteria naming exactly one alternative are accepted, so the rows
    // above failed on the rule rather than on the operation being unreachable.
    for (case, criteria) in [
        ("a regex alone", regex_criteria("[0-9]{3}")),
        (
            "an S3 object alone",
            words_list_criteria("macie-lists", "ignore.txt"),
        ),
    ] {
        let body = create_body(&format!("token-accepted-{case}"), "accepted", criteria);
        create(&router, region, &body, case).await;
    }
    let held = engine
        .state::<macie2::ComAmazonawsMacie2Macie2State>()
        .lock()
        .allow_lists
        .len();
    assert_eq!(
        held, 2,
        "the two accepted creates must be the whole store, got {held} allow list(s)"
    );
}

/// `status` reports whether Macie can access and use the list's criteria: for a
/// regex, whether the service could compile the expression, and for an S3 object
/// what object storage answers for that object right now. The rows span the
/// answers each kind of criterion can produce — a pattern that compiles and one
/// that does not, an object with content and one with none — and the status is
/// re-resolved when the object changes.
///
/// Source of the codes: `AllowListStatusCode` in this service's Smithy model —
/// "OK - Macie can retrieve and parse the contents of the object",
/// "S3_OBJECT_EMPTY - Macie can retrieve the object but the object doesn't
/// contain any content", "S3_OBJECT_NOT_FOUND - The object doesn't exist in
/// Amazon S3", "UNKNOWN_ERROR - A transient or internal error occurred when
/// Macie attempted to retrieve or parse the object" — read with the `code` doc,
/// which says of a regex that the status is OK because "Amazon Macie can compile
/// the expression".
#[tokio::test]
async fn the_status_reports_what_object_storage_answers_for_the_criteria() {
    let router = emulator_with_object_storage();
    enable_in(&router, OBJECT_STORAGE_REGION, "{}").await;

    // The objects the rows below name, written through S3's OWN front door.
    assert_eq!(
        s3_put(&router, "/macie-allow-lists", &[]).await,
        200,
        "the bucket the rows name must be created through S3"
    );
    assert_eq!(
        s3_put(&router, "/macie-allow-lists/lists/ignore.txt", b"alice\nbob\n").await,
        200,
        "the object with content must be written through S3"
    );
    assert_eq!(
        s3_put(&router, "/macie-allow-lists/lists/empty.txt", &[]).await,
        200,
        "the object with no content must be written through S3"
    );

    let cases: [(&str, serde_json::Value, &str); 7] = [
        ("a regex", regex_criteria("[0-9]{3}"), "OK"),
        ("another regex", regex_criteria("^(alice|bob)$"), "OK"),
        // A pattern the service cannot compile is not a pattern Macie can use,
        // and the status says so rather than claiming the expression compiled.
        (
            "a regex with an unterminated class",
            regex_criteria("[0-9"),
            "UNKNOWN_ERROR",
        ),
        (
            "a regex with an unterminated group",
            regex_criteria("(unclosed"),
            "UNKNOWN_ERROR",
        ),
        (
            "a regex whose repetition has no bounds",
            regex_criteria("a{3,1}"),
            "UNKNOWN_ERROR",
        ),
        (
            "an object with content",
            words_list_criteria("macie-allow-lists", "lists/ignore.txt"),
            "OK",
        ),
        (
            "an object with no content",
            words_list_criteria("macie-allow-lists", "lists/empty.txt"),
            "S3_OBJECT_EMPTY",
        ),
    ];

    for (case, criteria, expected) in &cases {
        let body = create_body(&format!("token-{case}"), case, criteria.clone());
        let reply = create(&router, OBJECT_STORAGE_REGION, &body, case).await;
        let id = reply["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a create must report an id, got {reply:?}"))
            .to_string();

        let record = read(&router, OBJECT_STORAGE_REGION, &id, case).await;
        assert_eq!(
            record["status"]["code"].as_str(),
            Some(*expected),
            "case {case:?}: expected status code {expected:?}, got {:?}",
            record["status"]
        );
    }

    // The status is RESOLVED on every read rather than frozen at create: an
    // object REMOVED after the list was created stops being an object Macie can
    // retrieve, and the status says so.
    let body = create_body(
        "token-removed-later",
        "removed later",
        words_list_criteria("macie-allow-lists", "lists/transient.txt"),
    );
    assert_eq!(
        s3_put(&router, "/macie-allow-lists/lists/transient.txt", b"carol\n").await,
        200,
        "case \"removed later\": the object must be written through S3 first"
    );
    let reply = create(&router, OBJECT_STORAGE_REGION, &body, "removed later").await;
    let id = reply["id"].as_str().unwrap_or_default().to_string();
    let before = read(&router, OBJECT_STORAGE_REGION, &id, "removed later").await;
    assert_eq!(
        before["status"]["code"].as_str(),
        Some("OK"),
        "case \"removed later\": while the object is there the status must be OK, got {:?}",
        before["status"]
    );
    let deleted = s3_call(
        &router,
        "DELETE",
        "/macie-allow-lists/lists/transient.txt",
        &[],
    )
    .await;
    assert!(
        (200..300).contains(&deleted),
        "case \"removed later\": the object must be deleted through S3, got {deleted}"
    );
    let after = read(&router, OBJECT_STORAGE_REGION, &id, "removed later").await;
    assert_eq!(
        after["status"]["code"].as_str(),
        Some("S3_OBJECT_NOT_FOUND"),
        "case \"removed later\": once the object is gone the status must say so, got {:?}",
        after["status"]
    );
}

/// Macie checks the S3 object a create names UP FRONT: criteria naming an object
/// that does not exist are refused 400 `ValidationException`, naming the bucket
/// and the key the request carried, and nothing is stored. The rows span the
/// ways an object can be absent — a key the bucket does not hold, a near miss of
/// a key it does, and a bucket the account does not hold.
#[tokio::test]
async fn criteria_naming_an_object_that_does_not_exist_are_refused_up_front() {
    let router = emulator_with_object_storage();
    enable_in(&router, OBJECT_STORAGE_REGION, "{}").await;

    assert_eq!(
        s3_put(&router, "/macie-allow-lists", &[]).await,
        200,
        "the bucket the rows are near-misses of must be created through S3"
    );
    assert_eq!(
        s3_put(&router, "/macie-allow-lists/lists/ignore.txt", b"alice\n").await,
        200,
        "the one object this account holds must be written through S3"
    );

    let absent: [(&str, &str, &str); 4] = [
        (
            "a key the bucket does not hold",
            "macie-allow-lists",
            "lists/absent.txt",
        ),
        (
            "a near miss of the held key",
            "macie-allow-lists",
            "lists/ignore.txt.bak",
        ),
        (
            "the held key in another case",
            "macie-allow-lists",
            "Lists/Ignore.txt",
        ),
        (
            "a bucket the account does not hold",
            "absent-allow-lists",
            "lists/ignore.txt",
        ),
    ];

    for (case, bucket, key) in &absent {
        let body = create_body(
            &format!("token-{case}"),
            "refused",
            words_list_criteria(bucket, key),
        );
        let (status, reply) =
            call_in(&router, OBJECT_STORAGE_REGION, "POST", ALLOW_LISTS_URI, &body.to_string())
                .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {reply:?}"
        );
        let expected = format!("The S3 object doesn't exist: {bucket}, {key}");
        assert_eq!(
            error_message(&reply),
            expected,
            "case {case:?}: expected the refusal {expected:?}, got {:?}",
            error_message(&reply)
        );
    }

    // The object this account DOES hold is accepted, so the rows above failed on
    // the object rather than on the operation being unreachable.
    let body = create_body(
        "token-held-object",
        "held",
        words_list_criteria("macie-allow-lists", "lists/ignore.txt"),
    );
    let reply = create(&router, OBJECT_STORAGE_REGION, &body, "the held object").await;
    let id = reply["id"].as_str().unwrap_or_default().to_string();
    let record = read(&router, OBJECT_STORAGE_REGION, &id, "the held object").await;
    assert_eq!(
        record["status"]["code"].as_str(),
        Some("OK"),
        "case \"the held object\": an object Macie can retrieve reports OK, got {:?}",
        record["status"]
    );
}

/// A retried `clientToken` replays the allow list that token minted; a
/// different token mints a different list, however alike the two requests are.
#[tokio::test]
async fn a_retried_client_token_replays_the_first_allow_list() {
    let router = emulator();

    for region in REGIONS {
        enable_in(&router, region, "{}").await;
        for (case, criteria) in [
            ("a regex", regex_criteria("[0-9]{4}")),
            (
                "an S3 object",
                words_list_criteria("macie-allow-lists", "lists/ignore.txt"),
            ),
        ] {
            let case = format!("{case} in {region}");
            let body = create_body("retried-token", "retried", criteria.clone());
            let first = create(&router, region, &body, &case).await;
            let replayed = create(&router, region, &body, &case).await;
            assert_eq!(
                replayed, first,
                "case {case:?}: a retried clientToken must replay the first list, got \
                 {replayed:?} after {first:?}"
            );

            let other = create_body("another-token", "retried", criteria);
            let second = create(&router, region, &other, &case).await;
            assert_ne!(
                second["id"], first["id"],
                "case {case:?}: another clientToken must mint another list, got {second:?}"
            );
        }
    }
}

/// An allow list belongs to the region its create addressed: a read naming
/// another region does not find it, and an id nothing minted is refused 404
/// `ResourceNotFoundException`.
#[tokio::test]
async fn an_allow_list_is_only_visible_to_the_region_it_was_created_in() {
    let router = emulator();
    for region in REGIONS {
        enable_in(&router, region, "{}").await;
    }

    let body = create_body("token-region", "regional", regex_criteria("[0-9]+"));
    let reply = create(&router, REGIONS[0], &body, "regional").await;
    let id = reply["id"].as_str().unwrap_or_default().to_string();

    // Ids no region minted, and the live id read in the OTHER region: each one
    // addresses no allow list of the region the request names.
    let absent: [(&str, String); 3] = [
        ("an id nothing minted", "a".repeat(ID_LEN)),
        ("another id nothing minted", "0123456789abcdefghijkl".to_string()),
        ("the live id in another region", id.clone()),
    ];
    for (case, candidate) in &absent {
        let region = if case.contains("another region") {
            REGIONS[1]
        } else {
            REGIONS[0]
        };
        let (status, reply) = call_in(
            &router,
            region,
            "GET",
            &format!("{ALLOW_LISTS_URI}/{candidate}"),
            "",
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {reply:?}"
        );
        assert_eq!(
            error_message(&reply),
            NOT_FOUND_MESSAGE,
            "case {case:?}: expected the refusal {NOT_FOUND_MESSAGE:?}, got {reply:?}"
        );
    }

    // And the list is still there for the region that created it.
    let record = read(&router, REGIONS[0], &id, "regional").await;
    assert_eq!(
        record["id"].as_str(),
        Some(id.as_str()),
        "the created list must still read back in its own region, got {record:?}"
    );
}

/// Both operations are refused 403 `AccessDeniedException` until Macie is
/// enabled in the region the request names. The rows span a create whose
/// criteria are well formed, a create naming an object nothing holds (so the
/// refusal is the session's rather than the object's), and a read.
#[tokio::test]
async fn the_allow_list_operations_are_refused_until_macie_is_enabled() {
    let router = emulator();
    let region = REGIONS[0];

    let body = create_body("token-disabled", "disabled", regex_criteria("[0-9]+"));
    let absent = create_body(
        "token-disabled-absent",
        "disabled",
        words_list_criteria("absent-allow-lists", "lists/ignore.txt"),
    );
    let read_uri = format!("{ALLOW_LISTS_URI}/{}", "a".repeat(ID_LEN));
    let calls: [(&str, &str, &str, String); 3] = [
        ("a create", "POST", ALLOW_LISTS_URI, body.to_string()),
        (
            "a create naming an object nothing holds",
            "POST",
            ALLOW_LISTS_URI,
            absent.to_string(),
        ),
        ("a read", "GET", read_uri.as_str(), String::new()),
    ];
    for (case, method, uri, payload) in &calls {
        let (status, reply) = call_in(&router, region, method, uri, payload).await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected 403 before Macie is enabled, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "AccessDeniedException",
            "case {case:?}: expected AccessDeniedException, got {reply:?}"
        );
    }

    // Enabled, the create goes through: the rows above failed on the session
    // rather than on the operation being unreachable.
    enable_in(&router, region, "{}").await;
    create(&router, region, &body, "a create once enabled").await;
}

// ---------------------------------------------------------------------------
// Editing and deleting a list
// ---------------------------------------------------------------------------

/// `DeleteAllowList` for `id`, carrying `ignore_job_checks` as the request's own
/// `ignoreJobChecks` query parameter when the row names one.
async fn delete(
    router: &MultiServiceRouter,
    region: &str,
    id: &str,
    ignore_job_checks: Option<&str>,
) -> (u16, serde_json::Value) {
    let uri = match ignore_job_checks {
        Some(asked) => format!("{ALLOW_LISTS_URI}/{id}?ignoreJobChecks={asked}"),
        None => format!("{ALLOW_LISTS_URI}/{id}"),
    };
    call_in(router, region, "DELETE", &uri, "").await
}

/// `UpdateAllowList` for `id`.
async fn update(
    router: &MultiServiceRouter,
    region: &str,
    id: &str,
    body: &serde_json::Value,
) -> (u16, serde_json::Value) {
    call_in(
        router,
        region,
        "PUT",
        &format!("{ALLOW_LISTS_URI}/{id}"),
        &body.to_string(),
    )
    .await
}

/// Assert a response body carries no members, which is what a success of an
/// operation whose output models none answers: `DeleteAllowListResponse` has no
/// members at all.
fn assert_no_members(body: &serde_json::Value, case: &str) {
    let empty = body.is_null()
        || body
            .as_object()
            .is_some_and(|members| members.is_empty());
    assert!(
        empty,
        "case {case:?}: a delete answers a body with no members, got {body:?}"
    );
}

/// `ListAllowLists` for `region`.
async fn list(router: &MultiServiceRouter, region: &str) -> serde_json::Value {
    let (status, reply) = call_in(router, region, "GET", ALLOW_LISTS_URI, "").await;
    assert_eq!(
        status, 200,
        "an enumeration must answer 200, got {status} with {reply:?}"
    );
    reply
}

/// An edit body: `name` and `criteria` are required and `description` is not,
/// so this is the whole of a list's settings.
fn update_body(
    name: &str,
    description: Option<&str>,
    criteria: serde_json::Value,
) -> serde_json::Value {
    let mut body = serde_json::json!({ "name": name, "criteria": criteria });
    if let Some(description) = description {
        body["description"] = serde_json::Value::String(description.to_string());
    }
    body
}

/// Create one classification job of `region` configured to use `allow_list_ids`
/// and leave it in `job_status`, requiring a read to report that status.
///
/// A recurring job that was not asked to analyze existing objects has started
/// no run, so it is `IDLE`, and a status a REQUEST may move it to is asked for
/// through `UpdateClassificationJob`. Every status reached here is one nothing
/// moves the job out of afterwards, so a case built on it cannot depend on how
/// fast the host runs.
async fn job_using(
    router: &MultiServiceRouter,
    region: &str,
    token: &str,
    allow_list_ids: &[&str],
    job_status: &str,
    case: &str,
) -> String {
    let body = job_scheduled(
        token,
        serde_json::json!({ "allowListIds": allow_list_ids }),
    );
    let (job_id, _) = create_job(router, region, &body, case).await;
    if job_status != JOB_IDLE {
        let (status, reply) = call_in(
            router,
            region,
            "PATCH",
            &format!("/jobs/{job_id}"),
            &serde_json::json!({ "jobStatus": job_status }).to_string(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the job must move to {job_status:?}, got {status} with {reply:?}"
        );
    }
    let (status, reply) = describe_job(router, region, &job_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the job must be readable, got {status} with {reply:?}"
    );
    assert_eq!(
        reply["jobStatus"].as_str(),
        Some(job_status),
        "case {case:?}: the job must be {job_status:?}, got {:?}",
        reply["jobStatus"]
    );
    job_id
}

/// A delete is refused while classification jobs that are not done are still
/// configured to use the list, and the refusal names how many there are. The
/// rows span the counts and the non-terminal statuses such a job can be in, and
/// run with the request leaving `ignoreJobChecks` out and with it naming the
/// value that keeps the checks in place.
#[tokio::test]
async fn a_delete_is_refused_while_jobs_that_are_not_done_use_the_list() {
    let router = emulator();

    // Each row: the statuses of the jobs configured to use the list. Every one
    // of them is a status other than COMPLETE or CANCELLED, so the refusal
    // counts them all.
    let rows: [(&str, &[&str]); 5] = [
        ("one recurring job awaiting its next run", &[JOB_IDLE]),
        ("one job the caller paused", &[JOB_USER_PAUSED]),
        ("two jobs awaiting their next run", &[JOB_IDLE, JOB_IDLE]),
        ("a paused job and a waiting one", &[JOB_USER_PAUSED, JOB_IDLE]),
        (
            "three jobs, one of them paused",
            &[JOB_IDLE, JOB_USER_PAUSED, JOB_IDLE],
        ),
    ];

    // The forms of the request that leave the documented checks in place: the
    // member absent, and the member naming the value that asks for them.
    let keeps_checks: [Option<&str>; 2] = [None, Some("false")];

    for region in REGIONS {
        enable_in(&router, region, "{}").await;
        for (row, (name, statuses)) in rows.iter().enumerate() {
            for asked in keeps_checks {
                let case = format!("{name} in {region} with ignoreJobChecks {asked:?}");
                let token = format!("token-in-use-{region}-{row}-{asked:?}");
                let created = create(
                    &router,
                    region,
                    &create_body(&token, "in use", regex_criteria("[0-9]{2}")),
                    &case,
                )
                .await;
                let id = created["id"].as_str().unwrap_or_default().to_string();

                for (index, job_status) in statuses.iter().enumerate() {
                    job_using(
                        &router,
                        region,
                        &format!("{token}-job-{index}"),
                        &[id.as_str()],
                        job_status,
                        &case,
                    )
                    .await;
                }

                let (status, reply) = delete(&router, region, &id, asked).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected 400, got {status} with {reply:?}"
                );
                assert_eq!(
                    error_code(&reply),
                    "ValidationException",
                    "case {case:?}: expected ValidationException, got {reply:?}"
                );
                let expected = in_use_message(statuses.len());
                assert_eq!(
                    error_message(&reply),
                    expected,
                    "case {case:?}: expected the refusal {expected:?}, got {:?}",
                    error_message(&reply)
                );

                // The refusal left the list where it was.
                let record = read(&router, region, &id, &case).await;
                assert_eq!(
                    record["id"].as_str(),
                    Some(id.as_str()),
                    "case {case:?}: the refused delete must leave the list readable, got \
                     {record:?}"
                );

                // And the caller that asks for the checks to be skipped gets
                // the delete, whatever those jobs are doing.
                let (status, reply) = delete(&router, region, &id, Some("true")).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: a delete that skips the checks must answer 200, got \
                     {status} with {reply:?}"
                );
                assert_no_members(&reply, &case);
            }
        }
    }
}

/// A delete goes through with the checks in place when no job that is not done
/// uses the list. The rows span every way that can be so: no job at all, a job
/// in each terminal status, a job that uses another list, and a job of another
/// region that names this id.
#[tokio::test]
async fn a_delete_succeeds_when_no_job_that_is_not_done_uses_the_list() {
    let router = emulator();
    for region in REGIONS {
        enable_in(&router, region, "{}").await;
    }
    let region = REGIONS[0];
    let other_region = REGIONS[1];

    // A list the "another list" row points its job at, which must still be
    // there at the end.
    let bystander = create(
        &router,
        region,
        &create_body("token-bystander", "bystander", regex_criteria("[a-z]+")),
        "a list another job uses",
    )
    .await;
    let bystander_id = bystander["id"].as_str().unwrap_or_default().to_string();

    let rows: [&str; 5] = [
        "no job at all",
        "one cancelled job",
        "two cancelled jobs",
        "a job that uses another list",
        "a job of another region naming this id",
    ];

    for (row, case) in rows.iter().enumerate() {
        let token = format!("token-deletable-{row}");
        let created = create(
            &router,
            region,
            &create_body(&token, "deletable", regex_criteria("[0-9]+")),
            case,
        )
        .await;
        let id = created["id"].as_str().unwrap_or_default().to_string();

        match *case {
            "one cancelled job" => {
                job_using(
                    &router,
                    region,
                    &format!("{token}-job"),
                    &[id.as_str()],
                    JOB_CANCELLED,
                    case,
                )
                .await;
            }
            "two cancelled jobs" => {
                for index in 0..2 {
                    job_using(
                        &router,
                        region,
                        &format!("{token}-job-{index}"),
                        &[id.as_str()],
                        JOB_CANCELLED,
                        case,
                    )
                    .await;
                }
            }
            "a job that uses another list" => {
                job_using(
                    &router,
                    region,
                    &format!("{token}-job"),
                    &[bystander_id.as_str()],
                    JOB_IDLE,
                    case,
                )
                .await;
            }
            "a job of another region naming this id" => {
                job_using(
                    &router,
                    other_region,
                    &format!("{token}-job"),
                    &[id.as_str()],
                    JOB_IDLE,
                    case,
                )
                .await;
            }
            _ => {}
        }

        let (status, reply) = delete(&router, region, &id, Some("false")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {reply:?}"
        );
        assert_no_members(&reply, case);
    }

    // The list the rows pointed another job at was never addressed, so it is
    // still there: the deletes above took exactly the list they named.
    let record = read(&router, region, &bystander_id, "a list another job uses").await;
    assert_eq!(
        record["id"].as_str(),
        Some(bystander_id.as_str()),
        "the list no delete addressed must still be readable, got {record:?}"
    );
}

/// Once a list is deleted every later call addressing its id reports it
/// missing, with the refusal the service words for a list that does not exist,
/// and the enumeration no longer carries it. The rows span the calls that
/// address one list by id.
#[tokio::test]
async fn a_deleted_allow_list_is_gone_from_every_later_call() {
    let router = emulator();

    for region in REGIONS {
        enable_in(&router, region, "{}").await;

        // Two lists, so what the delete takes can be told from what it leaves.
        let deleted = create(
            &router,
            region,
            &create_body("token-deleted", "deleted", regex_criteria("[0-9]+")),
            "the deleted list",
        )
        .await;
        let deleted_id = deleted["id"].as_str().unwrap_or_default().to_string();
        let kept = create(
            &router,
            region,
            &create_body("token-kept", "kept", regex_criteria("[a-z]+")),
            "the kept list",
        )
        .await;
        let kept_id = kept["id"].as_str().unwrap_or_default().to_string();

        let (status, _) = delete(&router, region, &deleted_id, None).await;
        assert_eq!(
            status, 200,
            "case {region:?}: a list no job uses must delete, got {status}"
        );

        let edit = update_body("deleted", None, regex_criteria("[0-9]+"));
        let calls: [(&str, &str, String); 3] = [
            ("a read", "GET", String::new()),
            ("an edit", "PUT", edit.to_string()),
            ("a second delete", "DELETE", String::new()),
        ];
        for (case, method, payload) in &calls {
            let case = format!("{case} of a deleted list in {region}");
            let (status, reply) = call_in(
                &router,
                region,
                method,
                &format!("{ALLOW_LISTS_URI}/{deleted_id}"),
                payload,
            )
            .await;
            assert_eq!(
                status, 404,
                "case {case:?}: expected 404, got {status} with {reply:?}"
            );
            assert_eq!(
                error_code(&reply),
                "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException, got {reply:?}"
            );
            assert_eq!(
                error_message(&reply),
                NOT_FOUND_MESSAGE,
                "case {case:?}: expected the refusal {NOT_FOUND_MESSAGE:?}, got {:?}",
                error_message(&reply)
            );
        }

        // The enumeration reports exactly what is left.
        let page = list(&router, region).await;
        let ids: Vec<&str> = page["allowLists"]
            .as_array()
            .map(|lists| {
                lists
                    .iter()
                    .filter_map(|summary| summary["id"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            ids,
            vec![kept_id.as_str()],
            "case {region:?}: the enumeration must report only the kept list, got {page:?}"
        );
    }
}

/// `UpdateAllowList` replaces the settings of the list it addresses, so a read
/// reports the edited settings and nothing else moves: the id and the ARN are
/// the list's own, and `createdAt` is where it was. The rows span the settings
/// an edit can change — the name, the description, and what the criterion of
/// the stored kind says.
#[tokio::test]
async fn an_updated_allow_list_reads_back_with_the_edited_settings() {
    let router = emulator();

    let edits: [(&str, serde_json::Value); 5] = [
        (
            "another name",
            update_body("renamed", None, regex_criteria("[0-9]+")),
        ),
        (
            "a description where there was none",
            update_body("described", Some("ignores test data"), regex_criteria("[0-9]+")),
        ),
        (
            "another expression",
            update_body("re-scoped", None, regex_criteria("^ACCT-[0-9]{4}$")),
        ),
        (
            "every member at once",
            update_body(
                "everything",
                Some("all of the settings"),
                regex_criteria("[a-z]+@example\\.com"),
            ),
        ),
        (
            "the settings it already has",
            update_body("original", None, regex_criteria("[0-9]+")),
        ),
    ];

    for region in REGIONS {
        enable_in(&router, region, "{}").await;
        for (row, (name, edit)) in edits.iter().enumerate() {
            let case = format!("{name} in {region}");
            let created = create(
                &router,
                region,
                &create_body(
                    &format!("token-edit-{region}-{row}"),
                    "original",
                    regex_criteria("[0-9]+"),
                ),
                &case,
            )
            .await;
            let id = created["id"].as_str().unwrap_or_default().to_string();
            let before = read(&router, region, &id, &case).await;

            let (status, reply) = update(&router, region, &id, edit).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} with {reply:?}"
            );
            assert_eq!(
                reply["id"].as_str(),
                Some(id.as_str()),
                "case {case:?}: an edit must report the id it addressed, got {reply:?}"
            );
            assert_eq!(
                reply["arn"], created["arn"],
                "case {case:?}: an edit must report the list's own ARN, got {reply:?}"
            );

            let record = read(&router, region, &id, &case).await;
            for member in ["name", "description", "criteria"] {
                let expected = edit.get(member).cloned();
                assert_eq!(
                    record.get(member).cloned().filter(|value| !value.is_null()),
                    expected,
                    "case {case:?}: expected {member} {expected:?}, got {:?}",
                    record.get(member)
                );
            }
            assert_eq!(
                record["createdAt"], before["createdAt"],
                "case {case:?}: an edit must not move createdAt, got {record:?}"
            );
            let created_at = reported_instant(&case, "createdAt", &record);
            let updated_at = reported_instant(&case, "updatedAt", &record);
            if *name == "the settings it already has" {
                // Nothing changed, so the instant the settings were last
                // changed is where it was.
                assert_eq!(
                    record["updatedAt"], before["updatedAt"],
                    "case {case:?}: an edit that changes nothing must not move updatedAt, got \
                     {record:?}"
                );
            } else {
                assert!(
                    updated_at >= created_at,
                    "case {case:?}: the settings were changed after the list was created, got \
                     {updated_at} against {created_at}"
                );
            }
        }
    }
}

/// An edit is refused when its criteria do not specify exactly one criterion,
/// when they specify the other KIND of criterion from the stored one, and when
/// no list of the addressed region carries the id — and a refused edit leaves
/// the stored settings exactly as they were.
#[tokio::test]
async fn an_edit_outside_the_documented_rules_is_refused_and_changes_nothing() {
    let router = emulator();
    let region = REGIONS[0];
    enable_in(&router, region, "{}").await;

    let created = create(
        &router,
        region,
        &create_body("token-refused-edit", "original", regex_criteria("[0-9]+")),
        "a stored list",
    )
    .await;
    let id = created["id"].as_str().unwrap_or_default().to_string();
    let before = read(&router, region, &id, "a stored list").await;

    let refused: [(&str, serde_json::Value); 4] = [
        (
            "criteria naming both alternatives",
            serde_json::json!({
                "name": "refused",
                "criteria": {
                    "regex": "[0-9]+",
                    "s3WordsList": { "bucketName": "macie-lists", "objectKey": "ignore.txt" },
                },
            }),
        ),
        (
            "criteria naming neither alternative",
            serde_json::json!({ "name": "refused", "criteria": {} }),
        ),
        (
            "criteria of the other kind",
            update_body(
                "refused",
                None,
                words_list_criteria("macie-allow-lists", "lists/ignore.txt"),
            ),
        ),
        (
            "criteria of the other kind with a description",
            update_body(
                "refused",
                Some("switching the kind"),
                words_list_criteria("other-allow-lists", "deep/ignore.txt"),
            ),
        ),
    ];

    for (case, edit) in &refused {
        let (status, reply) = update(&router, region, &id, edit).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {reply:?}"
        );
        assert!(
            !error_message(&reply).is_empty(),
            "case {case:?}: a refusal must say what is wrong, got {reply:?}"
        );
        let record = read(&router, region, &id, case).await;
        assert_eq!(
            record, before,
            "case {case:?}: a refused edit must change nothing, got {record:?}"
        );
    }

    // And an id no list of the region carries is refused 404, whatever the edit
    // asks for.
    for candidate in ["b".repeat(ID_LEN), "0123456789abcdefghijkl".to_string()] {
        let case = format!("an edit of {candidate:?}");
        let (status, reply) = update(
            &router,
            region,
            &candidate,
            &update_body("absent", None, regex_criteria("[0-9]+")),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {reply:?}"
        );
        assert_eq!(
            error_message(&reply),
            NOT_FOUND_MESSAGE,
            "case {case:?}: expected the refusal {NOT_FOUND_MESSAGE:?}, got {:?}",
            error_message(&reply)
        );
    }
}

/// An edit whose `s3WordsList` names an object the object store holds nothing
/// at is refused, the way a create naming that object is, and the stored
/// criteria are left alone. The rows span the addresses that name no object.
#[tokio::test]
async fn an_edit_naming_an_object_nothing_holds_is_refused() {
    let router = emulator_with_object_storage();
    enable_in(&router, OBJECT_STORAGE_REGION, "{}").await;

    assert_eq!(
        s3_put(&router, "/macie-allow-lists", &[]).await,
        200,
        "the bucket the rows are near-misses of must be created through S3"
    );
    assert_eq!(
        s3_put(&router, "/macie-allow-lists/lists/ignore.txt", b"alice\n").await,
        200,
        "the object the stored list names must be written through S3"
    );
    assert_eq!(
        s3_put(&router, "/macie-allow-lists/lists/other.txt", b"bob\n").await,
        200,
        "the object the accepted edit names must be written through S3"
    );

    let created = create(
        &router,
        OBJECT_STORAGE_REGION,
        &create_body(
            "token-words-edit",
            "words",
            words_list_criteria("macie-allow-lists", "lists/ignore.txt"),
        ),
        "a stored words list",
    )
    .await;
    let id = created["id"].as_str().unwrap_or_default().to_string();
    let before = read(&router, OBJECT_STORAGE_REGION, &id, "a stored words list").await;

    let absent: [(&str, &str, &str); 3] = [
        (
            "a key the bucket does not hold",
            "macie-allow-lists",
            "lists/absent.txt",
        ),
        (
            "a near miss of the held key",
            "macie-allow-lists",
            "lists/ignore.txt.bak",
        ),
        (
            "a bucket the account does not hold",
            "absent-allow-lists",
            "lists/ignore.txt",
        ),
    ];
    for (case, bucket, key) in &absent {
        let edit = update_body("words", None, words_list_criteria(bucket, key));
        let (status, reply) = update(&router, OBJECT_STORAGE_REGION, &id, &edit).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {reply:?}"
        );
        let expected = format!("The S3 object doesn't exist: {bucket}, {key}");
        assert_eq!(
            error_message(&reply),
            expected,
            "case {case:?}: expected the refusal {expected:?}, got {:?}",
            error_message(&reply)
        );
        let record = read(&router, OBJECT_STORAGE_REGION, &id, case).await;
        assert_eq!(
            record, before,
            "case {case:?}: a refused edit must change nothing, got {record:?}"
        );
    }

    // An object the store DOES hold is accepted, so the rows above failed on
    // the object rather than on the edit being unreachable.
    let edit = update_body(
        "words",
        None,
        words_list_criteria("macie-allow-lists", "lists/other.txt"),
    );
    let (status, reply) = update(&router, OBJECT_STORAGE_REGION, &id, &edit).await;
    assert_eq!(
        status, 200,
        "case \"the held object\": expected 200, got {status} with {reply:?}"
    );
    let record = read(&router, OBJECT_STORAGE_REGION, &id, "the held object").await;
    assert_eq!(
        record["criteria"],
        words_list_criteria("macie-allow-lists", "lists/other.txt"),
        "case \"the held object\": the edited criteria must read back, got {record:?}"
    );
}

/// An edit and a delete are refused 403 `AccessDeniedException` until Macie is
/// enabled in the region the request names.
#[tokio::test]
async fn an_edit_and_a_delete_are_refused_until_macie_is_enabled() {
    let router = emulator();
    let region = REGIONS[0];
    let uri = format!("{ALLOW_LISTS_URI}/{}", "a".repeat(ID_LEN));
    let edit = update_body("disabled", None, regex_criteria("[0-9]+"));

    for (case, method, payload) in [
        ("an edit", "PUT", edit.to_string()),
        ("a delete", "DELETE", String::new()),
    ] {
        let (status, reply) = call_in(&router, region, method, &uri, &payload).await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected 403 before Macie is enabled, got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "AccessDeniedException",
            "case {case:?}: expected AccessDeniedException, got {reply:?}"
        );
    }

    // Enabled, each one reaches the store and reports the id as missing: the
    // rows above failed on the session rather than on the operation being
    // unreachable.
    enable_in(&router, region, "{}").await;
    for (case, method, payload) in [
        ("an edit", "PUT", edit.to_string()),
        ("a delete", "DELETE", String::new()),
    ] {
        let (status, reply) = call_in(&router, region, method, &uri, &payload).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 once enabled, got {status} with {reply:?}"
        );
    }
}
