// SPDX-License-Identifier: CC0-1.0

//! The allow-list plane: what `CreateAllowList` proves about its criteria
//! before it stores anything, and what `GetAllowList` reports about a stored
//! list's STATUS afterwards.
//!
//! Every case drives the production `MultiServiceRouter` with this service
//! mounted beside this emulator's own Amazon S3 on ONE engine, so the objects an
//! `s3WordsList` criterion is resolved against are real objects of that store,
//! staged and removed through its own API.
//!
//! The rules under test, each asserted over a SET of inputs rather than one
//! request:
//!
//! * a `criteria` specifies exactly one criterion — "The criteria must specify a
//!   regular expression (regex) or an S3 object (s3WordsList). It can't specify
//!   both" — so one specifying both or neither is refused 400
//!   `ValidationException`;
//! * an `s3WordsList` criterion is RESOLVED against the object store before
//!   anything is stored: an address the store answers that it holds no object at
//!   is refused 400 `ValidationException` with the message naming that bucket
//!   and that key, and nothing is persisted — the refused name reaches no
//!   `ListAllowLists` page;
//! * an address the store serves is accepted, and the list is stored: the
//!   `GetAllowList` that follows reports every member of the request and the
//!   `ListAllowLists` enumeration carries it exactly once;
//! * a stored list's `status.code` is DERIVED from what the object store
//!   answers about its object at the time of the read — `OK` for an object that
//!   is there and carries content, `S3_OBJECT_EMPTY` for one that carries none,
//!   `S3_OBJECT_NOT_FOUND` once the object is removed again — and a regex
//!   criterion derives `OK`;
//! * a retried `clientToken` replays the list it minted rather than minting a
//!   second one.

use std::sync::Arc;

use service_com_amazonaws_macie2_macie2 as macie2;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use super::macie_session_and_custom_data_identifiers::{
    call_in, enable_in, error_code, error_message, reported_instant,
};

/// The region every case signs for; it must reach every minted ARN.
const REGION: &str = "us-west-2";

/// The allow-list collection's URI, from `CreateAllowList`'s `@http` binding.
const ALLOW_LISTS_URI: &str = "/allow-lists";

/// The bucket every staged word list lives in.
const BUCKET: &str = "macie-word-lists";

/// A second bucket, so no rule below is keyed to one bucket name.
const OTHER_BUCKET: &str = "macie-word-lists-two";

/// A bucket name the object store holds no bucket under.
const ABSENT_BUCKET: &str = "macie-word-lists-absent";

/// The `AllowListStatusCode` values these cases assert, entries of the modelled
/// enum (`smithy_query(query="shape",shape="AllowListStatusCode")`).
const STATUS_OK: &str = "OK";
const STATUS_NOT_FOUND: &str = "S3_OBJECT_NOT_FOUND";
const STATUS_EMPTY: &str = "S3_OBJECT_EMPTY";

/// How the service refuses a `criteria` that specifies BOTH a regular
/// expression and an S3 object: fixed text for that error class, interpolating
/// nothing from the request.
const CRITERIA_BOTH_MESSAGE: &str =
    "Allow list criteria can specify a regex or an S3 object, but not both.";

/// How it refuses a `criteria` that specifies NEITHER of them.
const CRITERIA_NEITHER_MESSAGE: &str = "Allow list criteria must specify a regex or an S3 object.";

/// This service beside this emulator's own Amazon S3, on one shared engine.
struct Aws {
    router: MultiServiceRouter,
}

impl Aws {
    /// One emulator, with Macie enabled in [`REGION`] and the two buckets the
    /// cases stage objects in created.
    async fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signatures are the emulator core's business, not this service's.
        router.set_request_authentication(false);
        router.register_service(macie2::EmulatedService::new(Arc::clone(&engine)));
        // The object an allow list's criteria name is an object of THIS store.
        router.register_service(
            service_com_amazonaws_s3_amazons3::EmulatedService::new(Arc::clone(&engine)),
        );
        let aws = Self { router };
        enable_in(&aws.router, REGION, "{}").await;
        for bucket in [BUCKET, OTHER_BUCKET] {
            aws.create_bucket(bucket).await;
        }
        aws
    }

    fn callback(&self) -> EmbeddedEmulator {
        EmbeddedEmulator::from_router(self.router.clone())
    }

    fn s3(&self) -> ::aws_sdk_s3::Client {
        ::service_common::s3_client::s3_client(&self.callback(), REGION)
    }

    async fn create_bucket(&self, bucket: &str) {
        let created = self
            .s3()
            .create_bucket()
            .bucket(bucket)
            .create_bucket_configuration(
                ::aws_sdk_s3::types::CreateBucketConfiguration::builder()
                    .location_constraint(::aws_sdk_s3::types::BucketLocationConstraint::from(
                        REGION,
                    ))
                    .build(),
            )
            .send()
            .await;
        assert!(
            created.is_ok(),
            "case {bucket:?}: Amazon S3 must create the bucket, got {created:?}"
        );
    }

    /// Stage an object holding `body` at `bucket`/`key`.
    async fn put_object(&self, bucket: &str, key: &str, body: &'static [u8]) {
        let put = self
            .s3()
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(::aws_sdk_s3::primitives::ByteStream::from_static(body))
            .send()
            .await;
        assert!(
            put.is_ok(),
            "case {bucket:?}/{key:?}: Amazon S3 must hold the object, got {put:?}"
        );
    }

    /// Remove the object at `bucket`/`key` again.
    async fn delete_object(&self, bucket: &str, key: &str) {
        let deleted = self
            .s3()
            .delete_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await;
        assert!(
            deleted.is_ok(),
            "case {bucket:?}/{key:?}: Amazon S3 must remove the object, got {deleted:?}"
        );
    }

    /// One `CreateAllowList` call, with the body a client sends.
    async fn create(&self, body: &serde_json::Value) -> (u16, serde_json::Value) {
        call_in(
            &self.router,
            REGION,
            "POST",
            ALLOW_LISTS_URI,
            &body.to_string(),
        )
        .await
    }

    /// `GetAllowList` of `id`.
    async fn get(&self, id: &str) -> (u16, serde_json::Value) {
        call_in(
            &self.router,
            REGION,
            "GET",
            &format!("{ALLOW_LISTS_URI}/{id}"),
            "",
        )
        .await
    }

    /// `UpdateAllowList` of `id`, with the body a client sends.
    async fn update(&self, id: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
        call_in(
            &self.router,
            REGION,
            "PUT",
            &format!("{ALLOW_LISTS_URI}/{id}"),
            &body.to_string(),
        )
        .await
    }

    /// `ListAllowLists`, with whatever query string `query` adds.
    async fn list(&self, query: &str) -> (u16, serde_json::Value) {
        call_in(
            &self.router,
            REGION,
            "GET",
            &format!("{ALLOW_LISTS_URI}{query}"),
            "",
        )
        .await
    }

    /// The names `ListAllowLists` reports, paging until it hands back no token.
    async fn listed_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        let mut query = String::new();
        loop {
            let (status, body) = self.list(&query).await;
            assert_eq!(
                status, 200,
                "ListAllowLists must answer 200, got {status} with {body:?}"
            );
            let page = body["allowLists"]
                .as_array()
                .unwrap_or_else(|| panic!("ListAllowLists must carry allowLists, got {body:?}"));
            for summary in page {
                names.push(
                    summary["name"]
                        .as_str()
                        .unwrap_or_else(|| panic!("a summary must carry a name, got {summary:?}"))
                        .to_string(),
                );
            }
            match body["nextToken"].as_str() {
                Some(token) => query = format!("?nextToken={token}"),
                None => return names,
            }
        }
    }
}

/// The body of a create naming an S3 object criterion.
fn words_list_body(name: &str, bucket: &str, key: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "clientToken": format!("token-{name}"),
        "criteria": { "s3WordsList": { "bucketName": bucket, "objectKey": key } },
    })
}

/// Create one allow list that must be accepted, and hand back its id.
async fn created(aws: &Aws, body: &serde_json::Value, case: &str) -> String {
    let (status, reply) = aws.create(body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAllowList must answer 200, got {status} with {reply:?}"
    );
    reply["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create must report an id, got {reply:?}"))
        .to_string()
}

/// The `status.code` `GetAllowList` reports for `id`.
async fn status_code(aws: &Aws, id: &str, case: &str) -> String {
    let (status, body) = aws.get(id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetAllowList must answer 200, got {status} with {body:?}"
    );
    body["status"]["code"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: GetAllowList must report status.code, got {body:?}")
        })
        .to_string()
}

/// An `s3WordsList` naming an object the store holds nothing at is refused 400
/// `ValidationException`, with the message naming the bucket and the key the
/// REQUEST carried, and nothing is stored for it.
///
/// The rows vary along everything the rule generalises over: the bucket (one the
/// store holds, a second one, one it holds no bucket under, and a name no bucket
/// could be held under at all) and the shape of the key (plain, prefixed, with a
/// space, and one that differs from a staged key in a single character).
#[tokio::test]
async fn a_create_naming_an_absent_object_is_refused_and_stores_nothing() {
    let aws = Aws::new().await;
    aws.put_object(BUCKET, "staged.txt", b"allowed\n").await;

    let rows: &[(&str, &str, &str)] = &[
        ("a plain key in a held bucket", BUCKET, "does-not-exist.txt"),
        (
            "a prefixed key in a held bucket",
            BUCKET,
            "lists/nested/absent.csv",
        ),
        ("a key with a space", OTHER_BUCKET, "my words.txt"),
        (
            "one character off a staged key",
            BUCKET,
            "staged.tx",
        ),
        ("a bucket the store does not hold", ABSENT_BUCKET, "staged.txt"),
        (
            "a name no bucket can be held under",
            "Not_A_Bucket_Name",
            "staged.txt",
        ),
    ];

    for (index, (case, bucket, key)) in rows.iter().enumerate() {
        let name = format!("refused-{index}");
        let (status, reply) = aws.create(&words_list_body(&name, bucket, key)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: an absent object must be refused 400, got {status} with {reply:?}"
        );
        let code = error_code(&reply);
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected ValidationException, got {code:?} with {reply:?}"
        );
        let expected = format!("The S3 object doesn't exist: {bucket}, {key}");
        let message = error_message(&reply);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert!(
            reply["id"].is_null(),
            "case {case:?}: a refused create reports no id, got {reply:?}"
        );
    }

    // Nothing the refused creates named was persisted: the enumeration is empty
    // even though six creates were made.
    let listed = aws.listed_names().await;
    assert!(
        listed.is_empty(),
        "a refused create must store nothing, got {listed:?}"
    );
}

/// A `criteria` that specifies both criteria, or neither, is refused 400
/// `ValidationException` with the sentence for that violation, and stores
/// nothing. The rows differ in which criteria are named and in the values they
/// carry, so no sentence is keyed to one regex, bucket or key.
#[tokio::test]
async fn a_create_must_specify_exactly_one_criterion() {
    let aws = Aws::new().await;
    aws.put_object(BUCKET, "staged.txt", b"allowed\n").await;
    aws.put_object(OTHER_BUCKET, "other.txt", b"also-allowed\n")
        .await;

    let rows: &[(&str, serde_json::Value, &str)] = &[
        (
            "both a regex and an S3 object",
            serde_json::json!({
                "regex": "[0-9]{3}",
                "s3WordsList": { "bucketName": BUCKET, "objectKey": "staged.txt" },
            }),
            CRITERIA_BOTH_MESSAGE,
        ),
        (
            "both, naming another bucket, key and expression",
            serde_json::json!({
                "regex": ".+",
                "s3WordsList": { "bucketName": OTHER_BUCKET, "objectKey": "other.txt" },
            }),
            CRITERIA_BOTH_MESSAGE,
        ),
        (
            "both, naming an object the store holds nothing at",
            serde_json::json!({
                "regex": "a",
                "s3WordsList": { "bucketName": ABSENT_BUCKET, "objectKey": "absent.txt" },
            }),
            CRITERIA_BOTH_MESSAGE,
        ),
        ("neither", serde_json::json!({}), CRITERIA_NEITHER_MESSAGE),
    ];

    for (index, (case, criteria, expected)) in rows.iter().enumerate() {
        let name = format!("criteria-{index}");
        let body = serde_json::json!({
            "name": name,
            "clientToken": format!("token-{name}"),
            "criteria": criteria,
        });
        let (status, reply) = aws.create(&body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {reply:?}"
        );
        let code = error_code(&reply);
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected ValidationException, got {code:?} with {reply:?}"
        );
        let message = error_message(&reply);
        assert_eq!(
            message, *expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    let listed = aws.listed_names().await;
    assert!(
        listed.is_empty(),
        "a refused create must store nothing, got {listed:?}"
    );
}

/// The edit is held to the same rule as the create and refuses it in the same
/// words, so one error class reads the same whichever operation reports it. The
/// settings the stored list carries are untouched by a refused edit.
#[tokio::test]
async fn an_edit_must_specify_exactly_one_criterion() {
    let aws = Aws::new().await;
    aws.put_object(BUCKET, "staged.txt", b"allowed\n").await;
    aws.put_object(OTHER_BUCKET, "other.txt", b"also-allowed\n")
        .await;

    let id = created(
        &aws,
        &serde_json::json!({
            "name": "edited-list",
            "clientToken": "token-edited-list",
            "criteria": { "regex": "[0-9]{3}" },
        }),
        "a regex-only create",
    )
    .await;

    let rows: &[(&str, serde_json::Value, &str)] = &[
        (
            "both a regex and an S3 object",
            serde_json::json!({
                "regex": "[0-9]{4}",
                "s3WordsList": { "bucketName": BUCKET, "objectKey": "staged.txt" },
            }),
            CRITERIA_BOTH_MESSAGE,
        ),
        (
            "both, naming another expression and another bucket",
            serde_json::json!({
                "regex": "x+",
                "s3WordsList": { "bucketName": OTHER_BUCKET, "objectKey": "other.txt" },
            }),
            CRITERIA_BOTH_MESSAGE,
        ),
        ("neither", serde_json::json!({}), CRITERIA_NEITHER_MESSAGE),
    ];

    for (case, criteria, expected) in rows {
        let body = serde_json::json!({
            "name": "edited-list",
            "criteria": criteria,
        });
        let (status, reply) = aws.update(&id, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {reply:?}"
        );
        let code = error_code(&reply);
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected ValidationException, got {code:?} with {reply:?}"
        );
        let message = error_message(&reply);
        assert_eq!(
            message, *expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // The refused edits left the stored criteria alone.
    let (status, read) = aws.get(&id).await;
    assert_eq!(
        status, 200,
        "the edited list must still be readable, got {status} with {read:?}"
    );
    assert_eq!(
        read["criteria"]["regex"].as_str(),
        Some("[0-9]{3}"),
        "a refused edit must leave the stored criteria alone, got {read:?}"
    );
}

/// An accepted create stores every member of the request, so the read that
/// follows reports it and the enumeration carries the list exactly once.
#[tokio::test]
async fn an_accepted_create_is_stored_and_read_back() {
    let aws = Aws::new().await;
    aws.put_object(BUCKET, "lists/allow.txt", b"allowed\nalso-allowed\n")
        .await;

    let body = serde_json::json!({
        "name": "staged-words",
        "clientToken": "token-staged-words",
        "description": "the words Macie ignores",
        "criteria": { "s3WordsList": { "bucketName": BUCKET, "objectKey": "lists/allow.txt" } },
        "tags": { "env": "test", "owner": "suite" },
    });
    let (status, reply) = aws.create(&body).await;
    assert_eq!(
        status, 200,
        "an object the store serves must be accepted, got {status} with {reply:?}"
    );
    let id = reply["id"]
        .as_str()
        .unwrap_or_else(|| panic!("the create must report an id, got {reply:?}"))
        .to_string();
    let expected_arn = format!(
        "arn:aws:macie2:{REGION}:{}:allow-list/{id}",
        ::engine::emulator_account_id()
    );
    assert_eq!(
        reply["arn"].as_str(),
        Some(expected_arn.as_str()),
        "the create must report the minted ARN, got {reply:?}"
    );

    let (status, read) = aws.get(&id).await;
    assert_eq!(
        status, 200,
        "GetAllowList must answer 200, got {status} with {read:?}"
    );
    for (member, expected) in [
        ("id", serde_json::json!(id)),
        ("arn", serde_json::json!(expected_arn)),
        ("name", serde_json::json!("staged-words")),
        ("description", serde_json::json!("the words Macie ignores")),
        (
            "criteria",
            serde_json::json!({
                "s3WordsList": { "bucketName": BUCKET, "objectKey": "lists/allow.txt" }
            }),
        ),
        (
            "tags",
            serde_json::json!({ "env": "test", "owner": "suite" }),
        ),
        ("status", serde_json::json!({ "code": STATUS_OK })),
    ] {
        assert_eq!(
            read[member], expected,
            "case {member:?}: expected {expected}, got {}",
            read[member]
        );
    }
    for instant in ["createdAt", "updatedAt"] {
        reported_instant(instant, instant, &read);
    }

    let listed = aws.listed_names().await;
    assert_eq!(
        listed,
        vec!["staged-words".to_string()],
        "the stored list must be enumerated exactly once, got {listed:?}"
    );

    // The summary reports the same stored record as the read, in the same wire
    // forms — the instants included.
    let (_, page) = aws.list("").await;
    let summary = page["allowLists"]
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or_else(|| panic!("the page must carry the summary, got {page:?}"));
    for member in ["arn", "createdAt", "description", "id", "name", "updatedAt"] {
        assert_eq!(
            summary[member], read[member],
            "case {member:?}: a summary and a read must report one stored value, got {} and {}",
            summary[member], read[member]
        );
    }
    for instant in ["createdAt", "updatedAt"] {
        reported_instant("AllowListSummary", instant, &summary);
    }

    // A read of an id the region holds no list under is refused, whatever it
    // looks like.
    for case in ["aaaaaaaaaaaaaaaaaaaaaa", "not-an-id", ""] {
        let (status, body) = aws.get(case).await;
        if case.is_empty() {
            // An empty id addresses the collection, not a list.
            assert_eq!(
                status, 200,
                "case {case:?}: the collection answers the enumeration, got {status} with {body:?}"
            );
            continue;
        }
        assert_eq!(
            status, 404,
            "case {case:?}: an unheld id must be refused 404, got {status} with {body:?}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {code:?}"
        );
    }
}

/// `status.code` is derived from what the object store answers about the list's
/// object AT THE TIME OF THE READ, over the span of answers a word list can
/// have, and a regex criterion derives `OK`.
#[tokio::test]
async fn the_status_follows_what_the_object_store_answers_now() {
    let aws = Aws::new().await;
    // Each row stages its own object, so the rows differ only in what the store
    // holds at that address: content, nothing at all, and a single byte.
    aws.put_object(BUCKET, "with-content.txt", b"allowed\n")
        .await;
    aws.put_object(BUCKET, "empty.txt", b"").await;
    aws.put_object(OTHER_BUCKET, "one-byte.txt", b"a").await;

    let rows: &[(&str, serde_json::Value, &str)] = &[
        (
            "an object carrying entries",
            words_list_body("content", BUCKET, "with-content.txt"),
            STATUS_OK,
        ),
        (
            "an object carrying nothing",
            words_list_body("empty", BUCKET, "empty.txt"),
            STATUS_EMPTY,
        ),
        (
            "an object of a single byte in another bucket",
            words_list_body("one-byte", OTHER_BUCKET, "one-byte.txt"),
            STATUS_OK,
        ),
        (
            "a regex criterion",
            serde_json::json!({
                "name": "regex",
                "clientToken": "token-regex",
                "criteria": { "regex": "[0-9]{3}-[0-9]{4}" },
            }),
            STATUS_OK,
        ),
    ];

    for (case, body, expected) in rows {
        let id = created(&aws, body, case).await;
        let actual = status_code(&aws, &id, case).await;
        assert_eq!(
            actual, *expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }

    // The status is not frozen at create: removing the object the first list
    // names moves that list's status, and leaves the other lists' alone.
    aws.put_object(BUCKET, "transient.txt", b"allowed\n").await;
    let id = created(
        &aws,
        &words_list_body("removed-later", BUCKET, "transient.txt"),
        "before the object is removed",
    )
    .await;
    // The object has to be there for the create to be accepted at all.
    assert_eq!(
        status_code(&aws, &id, "before the object is removed").await,
        STATUS_OK,
        "a list naming an object the store serves reports OK"
    );
    aws.delete_object(BUCKET, "transient.txt").await;
    let actual = status_code(&aws, &id, "after the object is removed").await;
    assert_eq!(
        actual, STATUS_NOT_FOUND,
        "case {:?}: expected {STATUS_NOT_FOUND:?}, got {actual:?}",
        "after the object is removed"
    );
}

/// A retried `clientToken` replays the list it minted rather than minting a
/// second one, so the enumeration still carries one list.
#[tokio::test]
async fn a_retried_client_token_replays_the_list_it_minted() {
    let aws = Aws::new().await;
    aws.put_object(BUCKET, "retry.txt", b"allowed\n").await;

    let body = words_list_body("retried", BUCKET, "retry.txt");
    let first = created(&aws, &body, "the first call").await;
    let second = created(&aws, &body, "the retry").await;
    assert_eq!(
        first, second,
        "a retry must replay the list the token minted, got {first:?} then {second:?}"
    );

    // A different token mints a second list, so the replay above is the token's
    // doing and not a refusal to create twice.
    let other = created(
        &aws,
        &serde_json::json!({
            "name": "retried-again",
            "clientToken": "token-another",
            "criteria": { "s3WordsList": { "bucketName": BUCKET, "objectKey": "retry.txt" } },
        }),
        "another token",
    )
    .await;
    assert_ne!(
        other, first,
        "another token must mint another list, got {other:?}"
    );

    let mut listed = aws.listed_names().await;
    listed.sort();
    assert_eq!(
        listed,
        vec!["retried".to_string(), "retried-again".to_string()],
        "exactly the two minted lists must be enumerated, got {listed:?}"
    );
}

/// The enumeration pages by `maxResults` and resumes from its own `nextToken`,
/// reporting every stored list exactly once across the pages.
#[tokio::test]
async fn the_enumeration_pages_and_resumes_from_its_own_token() {
    let aws = Aws::new().await;
    aws.put_object(BUCKET, "paged.txt", b"allowed\n").await;

    let mut expected: Vec<String> = Vec::new();
    for index in 0..5 {
        let name = format!("paged-{index}");
        created(
            &aws,
            &words_list_body(&name, BUCKET, "paged.txt"),
            "a staged list",
        )
        .await;
        expected.push(name);
    }
    expected.sort();

    for page_size in [1_usize, 2, 5, 25] {
        let mut names: Vec<String> = Vec::new();
        let mut query = format!("?maxResults={page_size}");
        loop {
            let (status, body) = aws.list(&query).await;
            assert_eq!(
                status, 200,
                "case {page_size}: ListAllowLists must answer 200, got {status} with {body:?}"
            );
            let page = body["allowLists"].as_array().unwrap_or_else(|| {
                panic!("case {page_size}: a page must carry allowLists, got {body:?}")
            });
            assert!(
                page.len() <= page_size,
                "case {page_size}: a page must hold at most {page_size} summaries, got {}",
                page.len()
            );
            for summary in page {
                names.push(
                    summary["name"]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {page_size}: a summary must carry a name, got {summary:?}")
                        })
                        .to_string(),
                );
            }
            match body["nextToken"].as_str() {
                Some(token) => query = format!("?maxResults={page_size}&nextToken={token}"),
                None => break,
            }
        }
        names.sort();
        assert_eq!(
            names, expected,
            "case {page_size}: every stored list must be enumerated exactly once, got {names:?}"
        );
    }

    // A token this service did not issue names no page.
    let (status, body) = aws.list("?nextToken=not-a-token").await;
    assert_eq!(
        status, 400,
        "a foreign nextToken must be refused 400, got {status} with {body:?}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, "ValidationException",
        "a foreign nextToken must be refused ValidationException, got {code:?}"
    );
}
