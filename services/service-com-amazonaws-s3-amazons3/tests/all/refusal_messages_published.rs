// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the MESSAGE S3 refuses a bad request value with.
//!
//! The rule under test: where S3 publishes its own sentence for a field, a refusal of that
//! field carries that sentence — never a report naming the Smithy member path the value
//! travelled on, which no S3 client or document uses. The error CODE and the HTTP status
//! are the ones the published refusal carries too.
//!
//! The sentences, each from the AWS S3 API reference for the operation that binds the
//! field:
//!
//! * `CopyObject`'s `x-amz-tagging-directive` outside `TaggingDirective` -> `Unknown
//!   tagging directive.`, and its `x-amz-metadata-directive` outside `MetadataDirective`
//!   -> `Unknown metadata directive.` (both enums over `[COPY, REPLACE]`).
//! * `GetObjectAttributes`' `x-amz-object-attributes` outside `ObjectAttributes` ->
//!   `Invalid attribute name specified.`
//! * `CreateBucket`'s `x-amz-object-ownership` outside `ObjectOwnership` -> `Invalid
//!   x-amz-object-ownership header: <the rejected value>`, the one sentence of the set that
//!   interpolates anything, and what it interpolates is the request's own header value.
//! * `ListBuckets`' `max-buckets` outside `MaxBuckets`' range (`min 1`, `max 10000`) ->
//!   `Argument max-buckets must be an integer between 1 and 10000`: the published BOUNDS,
//!   not the value that was sent.
//! * A tag set S3 will not store is refused with the fixed sentence for the restriction it
//!   broke — `Object tags cannot be greater than 10`, `Cannot provide multiple Tags with
//!   the same key`, `The TagKey you have provided is invalid`, `The TagValue you have
//!   provided is invalid` — none of which carries the offending key, value or count.
//!
//! The tests after those two carry the same rule into the refusals whose sentence states a
//! PUBLISHED LIMIT, and into the two whose CLASS is what a client branches on. Each spans
//! its rule over a set of requests, so a sentence quoting the request could not be one
//! string for every row:
//!
//! * `PutBucketPolicy` past the published 20 KB bucket-policy limit — one byte past it and
//!   far past it carry the SAME sentence, which states the limit.
//! * A BUCKET tag set past its maximum — the sentence is that resource's own published text
//!   (it names the tag COUNT, where an object's names the TAGS), so neither scope's sentence
//!   is a template the other fills.
//! * An integer page bound outside the range its argument publishes — the sentence names the
//!   query parameter and the two bounds, for `max-annotation-results` and `max-buckets`
//!   alike.
//! * `PutBucketCors` past the published 100 rules per configuration — and the refused put
//!   leaves the bucket with no cors configuration at all.
//! * `PutBucketLogging` naming a target bucket nothing holds — the sentence the service
//!   words on the wire, not the error-code list's longer description of the condition.
//! * An ACL write carrying an `AccessControlPolicy` BODY beside an ACL named in its headers
//!   — `UnexpectedContent`, the class for content a call does not take, told apart from the
//!   `InvalidRequest` of one ACL named in two HEADER forms.
//! * `UpdateObjectEncryption` naming an object it cannot resolve — the one not-found error
//!   that operation publishes, whether or not the address named a version id.

use service_com_amazonaws_s3_amazons3::state::{
    ALL_USERS_GROUP_URI, MAX_BUCKET_POLICY_BYTES, MAX_CORS_RULES, MAX_TAGS_PER_BUCKET,
    MAX_TAGS_PER_OBJECT, MAX_TAG_KEY_CHARS, MAX_TAG_VALUE_CHARS, PERMISSION_READ,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_acl::{access_control_policy, GrantXml};
use super::bucket_cors::{get_cors, put_cors};
use super::bucket_lifecycle::{
    create_bucket, policy_of_size, put_bucket_policy, router, send, send_bytes, Answer,
};
use super::bucket_ownership_controls::put_ownership_controls;
use super::bucket_public_access_block::allow_public_access;
use super::bucket_tagging::{put_bucket_tagging, tag, tag_set_of, tagging_payload};
use super::kms_key_references::{created_key, router_with_kms};
use super::object_acl::put_object_acl;
use super::object_and_tagging_roundtrip::{put_object, text};
use super::object_encryption_update::{payload as encryption_payload, update_object_encryption};
use super::object_tagging::{get_object_tagging, put_object_tagging};
use super::object_versioning::{delete_object, set_versioning};

/// Every value `ObjectOwnership` publishes
/// (`smithy_query(query="shape",shape="ObjectOwnership")`), so a row that must be REFUSED
/// can be checked against the set rather than assumed to be outside it.
const OBJECT_OWNERSHIP_VALUES: [&str; 3] =
    ["BucketOwnerPreferred", "ObjectWriter", "BucketOwnerEnforced"];

/// The published bounds of `MaxBuckets` (`smithy_query(query="shape",shape="MaxBuckets")`
/// reports `range {min 1, max 10000}`), which the refusal states.
const MAX_BUCKETS_RANGE: (i64, i64) = (1, 10_000);

/// One refused request: the case, the request to send, and the code, status and message it
/// must be refused with.
struct Row {
    case: String,
    method: &'static str,
    uri: String,
    headers: Vec<(String, String)>,
    body: String,
    code: &'static str,
    message: String,
}

/// The per-field rows, each naming a DIFFERENT operation and a different HTTP binding (two
/// headers of one operation, a header whose target is a list of enums, a header on a
/// create, and a query parameter with a numeric range), so the table spans what the rule
/// generalises over rather than repeating one recorded call.
fn rows(bucket: &str) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();

    // The two `CopyObject` directives. Each is tried with two values outside its enum —
    // one that differs in case from a value inside it, one that is a word of its own — so
    // the sentence is not keyed to one rejected spelling.
    for (header, outside, message) in [
        (
            "x-amz-tagging-directive",
            ["MERGE", "copy"],
            "Unknown tagging directive.",
        ),
        (
            "x-amz-metadata-directive",
            ["MERGE", "replace-all"],
            "Unknown metadata directive.",
        ),
    ] {
        for value in outside {
            rows.push(Row {
                case: format!("{header}: {value}"),
                method: "PUT",
                uri: format!("/{bucket}/copied"),
                headers: vec![
                    (
                        "x-amz-copy-source".to_string(),
                        format!("{bucket}/source"),
                    ),
                    (header.to_string(), value.to_string()),
                ],
                body: String::new(),
                code: "InvalidArgument",
                message: message.to_string(),
            });
        }
    }

    // `GetObjectAttributes`' header is a LIST of enums, so the rejected element may be the
    // only one, the first of several or a later one — the sentence names none of that.
    for value in [
        "NotARealAttribute",
        "etag",
        "NotARealAttribute, ETag",
        "ETag, ObjectSize, NotARealAttribute",
    ] {
        rows.push(Row {
            case: format!("x-amz-object-attributes: {value}"),
            method: "GET",
            uri: format!("/{bucket}/source?attributes"),
            headers: vec![("x-amz-object-attributes".to_string(), value.to_string())],
            body: String::new(),
            code: "InvalidArgument",
            message: "Invalid attribute name specified.".to_string(),
        });
    }

    // `CreateBucket`'s header is the one row whose sentence carries a value from the
    // request, so each row asserts its OWN value comes back in it.
    for value in ["BucketOwnerOnly", "objectwriter", "Owner Enforced"] {
        assert!(
            !OBJECT_OWNERSHIP_VALUES.contains(&value),
            "every row of this table must name a value OUTSIDE {OBJECT_OWNERSHIP_VALUES:?}, \
             got {value:?}"
        );
        rows.push(Row {
            case: format!("x-amz-object-ownership: {value}"),
            method: "PUT",
            uri: "/refusal-message-ownership".to_string(),
            headers: vec![("x-amz-object-ownership".to_string(), value.to_string())],
            body: String::new(),
            code: "InvalidArgument",
            message: format!("Invalid x-amz-object-ownership header: {value}"),
        });
    }

    // `ListBuckets`' range, one past each published bound and far past the maximum: the
    // sentence states the bounds, so every row expects the same text.
    let (min, max) = MAX_BUCKETS_RANGE;
    for value in [min - 1, max + 1, max * 10] {
        rows.push(Row {
            case: format!("max-buckets={value}"),
            method: "GET",
            uri: format!("/?x-id=ListBuckets&max-buckets={value}"),
            headers: Vec::new(),
            body: String::new(),
            code: "InvalidArgument",
            message: format!(
                "Argument max-buckets must be an integer between {min} and {max}"
            ),
        });
    }

    rows
}

#[tokio::test]
async fn a_refused_field_carries_the_message_s3_publishes_for_it() {
    let router = router();
    let bucket = "refusal-messages";
    let created = create_bucket(&router, bucket, None, None).await;
    assert_eq!(
        created.status, 200,
        "the bucket must be created first, got {} with body {:?}",
        created.status,
        text(&created)
    );
    let stored = put_object(&router, bucket, "source", b"bytes", &[]).await;
    assert_eq!(
        stored.status, 200,
        "the source object must be stored first, got {} with body {:?}",
        stored.status,
        text(&stored)
    );

    for row in rows(bucket) {
        let headers: Vec<(&str, &str)> = row
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let answer = send(&router, row.method, &row.uri, &headers, &row.body).await;
        assert_eq!(
            answer.error_code(),
            row.code,
            "case {:?}: expected the error code {:?}, got {:?} with status {} and body {:?}",
            row.case,
            row.code,
            answer.error_code(),
            answer.status,
            text(&answer)
        );
        assert_eq!(
            answer.status, 400,
            "case {:?}: an S3 refusal of a bad argument is HTTP 400, got {}",
            row.case, answer.status
        );
        assert_eq!(
            answer.error_message(),
            row.message,
            "case {:?}: expected the published message {:?}, got {:?}",
            row.case,
            row.message,
            answer.error_message()
        );
    }

    // The bounds themselves are SERVED, so the rows above are about being outside the
    // range and not about the parameter being present at all.
    let (min, max) = MAX_BUCKETS_RANGE;
    for value in [min, max] {
        let answer = send(
            &router,
            "GET",
            &format!("/?x-id=ListBuckets&max-buckets={value}"),
            &[],
            "",
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case \"max-buckets={value}\": a value at a published bound must be served, got \
             {} with body {:?}",
            answer.status,
            text(&answer)
        );
    }
    // And so is each value the ownership enum publishes.
    for (index, setting) in OBJECT_OWNERSHIP_VALUES.into_iter().enumerate() {
        let answer = send(
            &router,
            "PUT",
            &format!("/refusal-message-ownership-{index}"),
            &[("x-amz-object-ownership", setting)],
            "",
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {setting:?}: a value the enum publishes must be served, got {} with body \
             {:?}",
            answer.status,
            text(&answer)
        );
    }
}

/// One refused tag set: the case, the tags, and the code and message the refusal carries.
struct TagRow {
    case: String,
    tags: Vec<(String, String)>,
    code: &'static str,
    message: &'static str,
}

/// A character outside the class a tag may hold, one per row of the character rows: an
/// astral-plane code point, a BMP symbol and an ASCII punctuation mark the class leaves out.
const DISALLOWED: [char; 3] = ['\u{1f600}', '§', '?'];

#[tokio::test]
async fn a_refused_tag_set_carries_the_fixed_message_s3_publishes() {
    let router = router();
    let bucket = "refusal-message-tags";
    let created = create_bucket(&router, bucket, None, None).await;
    assert_eq!(
        created.status, 200,
        "the bucket must be created first, got {}",
        created.status
    );
    let key = "tagged.bin";
    let stored = put_object(&router, bucket, key, b"bytes", &[]).await;
    assert_eq!(
        stored.status, 200,
        "the object must be stored first, got {} with body {:?}",
        stored.status,
        text(&stored)
    );
    // A tag set that IS acceptable, stored first: every refusal below must leave it.
    let held = vec![tag("held", "value")];
    let accepted = put_object_tagging(&router, bucket, key, &held).await;
    assert_eq!(
        accepted.status, 200,
        "the setup tag set must be stored, got {} with body {:?}",
        accepted.status,
        text(&accepted)
    );

    let mut rows: Vec<TagRow> = Vec::new();

    // Past the published count, one over and far over: the sentence carries the resource's
    // own maximum, never the count that was sent.
    for count in [MAX_TAGS_PER_OBJECT + 1, MAX_TAGS_PER_OBJECT * 3] {
        rows.push(TagRow {
            case: format!("{count} tags on an object"),
            tags: (0..count)
                .map(|index| tag(&format!("key{index}"), "value"))
                .collect(),
            code: "BadRequest",
            message: "Object tags cannot be greater than 10",
        });
    }

    // One key named twice, as the first pair and as a later one.
    rows.push(TagRow {
        case: "the same key twice".to_string(),
        tags: vec![tag("dup", "a"), tag("dup", "b")],
        code: "InvalidTag",
        message: "Cannot provide multiple Tags with the same key",
    });
    rows.push(TagRow {
        case: "the same key twice, after another tag".to_string(),
        tags: vec![tag("other", "x"), tag("dup", "a"), tag("dup", "b")],
        code: "InvalidTag",
        message: "Cannot provide multiple Tags with the same key",
    });

    // A character outside the class, in the VALUE and in the KEY, with its own sentence
    // for each half of the tag.
    for character in DISALLOWED {
        rows.push(TagRow {
            case: format!("a value carrying {character:?}"),
            tags: vec![tag("key", &format!("value{character}"))],
            code: "InvalidTag",
            message: "The TagValue you have provided is invalid",
        });
        rows.push(TagRow {
            case: format!("a key carrying {character:?}"),
            tags: vec![tag(&format!("key{character}"), "value")],
            code: "InvalidTag",
            message: "The TagKey you have provided is invalid",
        });
    }

    // Past the published character bounds: the same two sentences, because what S3 states
    // is which half of the tag was not valid.
    rows.push(TagRow {
        case: "a key one character past its bound".to_string(),
        tags: vec![tag(&"k".repeat(MAX_TAG_KEY_CHARS + 1), "value")],
        code: "InvalidTag",
        message: "The TagKey you have provided is invalid",
    });
    rows.push(TagRow {
        case: "a value one character past its bound".to_string(),
        tags: vec![tag("key", &"v".repeat(MAX_TAG_VALUE_CHARS + 1))],
        code: "InvalidTag",
        message: "The TagValue you have provided is invalid",
    });

    for row in &rows {
        let answer = put_object_tagging(&router, bucket, key, &row.tags).await;
        assert_eq!(
            answer.error_code(),
            row.code,
            "case {:?}: expected the error code {:?}, got {:?} with status {} and body {:?}",
            row.case,
            row.code,
            answer.error_code(),
            answer.status,
            text(&answer)
        );
        assert_eq!(
            answer.status, 400,
            "case {:?}: a refused tag set is HTTP 400, got {}",
            row.case, answer.status
        );
        assert_eq!(
            answer.error_message(),
            row.message,
            "case {:?}: expected the published message {:?}, got {:?}",
            row.case,
            row.message,
            answer.error_message()
        );
        let read = get_object_tagging(&router, bucket, key, "").await;
        assert_eq!(
            tag_set_of(&read),
            held,
            "case {:?}: a refused put must leave the stored tag set untouched",
            row.case
        );
    }

    // The SAME restrictions on a BUCKET's tag set carry the same sentences, with the
    // bucket's own published count in the one that states a maximum.
    let bucket_rows: Vec<TagRow> = vec![
        TagRow {
            case: format!("{} tags on a bucket", MAX_TAGS_PER_BUCKET + 1),
            tags: (0..=MAX_TAGS_PER_BUCKET)
                .map(|index| tag(&format!("key{index}"), "value"))
                .collect(),
            code: "BadRequest",
            message: "Bucket tag count cannot be greater than 50",
        },
        TagRow {
            case: "the same key twice on a bucket".to_string(),
            tags: vec![tag("dup", "a"), tag("dup", "b")],
            code: "InvalidTag",
            message: "Cannot provide multiple Tags with the same key",
        },
        TagRow {
            case: "a bucket tag value outside the class".to_string(),
            tags: vec![tag("key", "value\u{1f600}")],
            code: "InvalidTag",
            message: "The TagValue you have provided is invalid",
        },
    ];
    for row in &bucket_rows {
        let answer = put_bucket_tagging(&router, bucket, &row.tags).await;
        assert_eq!(
            answer.error_code(),
            row.code,
            "case {:?}: expected the error code {:?}, got {:?} with status {} and body {:?}",
            row.case,
            row.code,
            answer.error_code(),
            answer.status,
            text(&answer)
        );
        assert_eq!(
            answer.error_message(),
            row.message,
            "case {:?}: expected the published message {:?}, got {:?}",
            row.case,
            row.message,
            answer.error_message()
        );
    }

    // A tag set the published restrictions ALLOW is still stored, so the rows above are
    // about what they carried: the count at exactly the object's maximum, with every
    // character class the published class does hold.
    let at_limit: Vec<(String, String)> = (0..MAX_TAGS_PER_OBJECT)
        .map(|index| tag(&format!("key.{index}"), "value-1 2:3/4@5_6=7+8"))
        .collect();
    let answer = put_object_tagging(&router, bucket, key, &at_limit).await;
    assert_eq!(
        answer.status, 200,
        "a tag set at the published maximum must be stored, got {} with body {:?}",
        answer.status,
        text(&answer)
    );
    let read = get_object_tagging(&router, bucket, key, "").await;
    assert_eq!(
        tag_set_of(&read),
        at_limit,
        "the accepted tag set must read back unchanged"
    );
    // The payload builder is the shared one, so this suite cannot drift from the others on
    // how a `Tagging` document is spelled.
    assert!(
        tagging_payload(&at_limit).contains("<TagSet>"),
        "the shared payload builder must carry the TagSet container"
    );
}

/// The XML namespace botocore puts on an S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// A bucket for one of the tests below, asserted created so a later refusal cannot be
/// mistaken for a missing bucket.
async fn made_bucket(router: &MultiServiceRouter, name: &str) {
    let created = create_bucket(router, name, None, None).await;
    assert_eq!(
        created.status,
        200,
        "the bucket {name:?} must be created first, got {} with body {:?}",
        created.status,
        text(&created)
    );
}

#[tokio::test]
async fn an_oversized_bucket_policy_states_the_published_limit() {
    let router = router();
    let name = "refusal-messages-policy";
    made_bucket(&router, name).await;
    // The padded document this test measures sizes with grants to everyone, and a bucket is
    // created with Block Public Access on, which would deny the write before its size was
    // judged. Lifted here the way the bucket-policy status suite lifts it.
    allow_public_access(&router, name).await;

    // Fixed text naming the published bucket-policy limit, so every row below expects the
    // same string however large the document it sent was.
    let expected = format!(
        "Normalized policy document exceeds the maximum allowed size of \
         {MAX_BUCKET_POLICY_BYTES} bytes"
    );

    // One byte past the ceiling, a kilobyte past it, and twice the ceiling: a sentence
    // interpolating the request's own byte count could not be one string for all three.
    for bytes in [
        MAX_BUCKET_POLICY_BYTES + 1,
        MAX_BUCKET_POLICY_BYTES + 1_024,
        MAX_BUCKET_POLICY_BYTES * 2,
    ] {
        let document = policy_of_size(bytes);
        assert_eq!(
            document.len(),
            bytes,
            "case {bytes}: the test's own document must be exactly that many bytes, got {}",
            document.len()
        );
        let answer = put_bucket_policy(&router, name, &document).await;
        assert_eq!(
            answer.status,
            400,
            "case {bytes}: an oversized policy is HTTP 400, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        assert_eq!(
            answer.error_code(),
            "MalformedPolicy",
            "case {bytes}: expected MalformedPolicy, got {:?}",
            answer.error_code()
        );
        assert_eq!(
            answer.error_message(),
            expected,
            "case {bytes}: expected the published sentence {expected:?}, got {:?}",
            answer.error_message()
        );
    }

    // The ceiling itself is SERVED, so the rows above are about being past the limit rather
    // than about the document's shape.
    let served = put_bucket_policy(&router, name, &policy_of_size(MAX_BUCKET_POLICY_BYTES)).await;
    assert_eq!(
        served.status,
        204,
        "a document exactly at the ceiling must be stored, got {} with body {:?}",
        served.status,
        text(&served)
    );
}

#[tokio::test]
async fn a_tag_set_past_its_maximum_states_its_resource_s_own_sentence() {
    let router = router();
    let name = "refusal-messages-tag-count";
    made_bucket(&router, name).await;
    let key = "tagged.bin";
    let stored = put_object(&router, name, key, b"tag-subject", &[]).await;
    assert_eq!(
        stored.status,
        200,
        "the object must be stored first, got {} with body {:?}",
        stored.status,
        text(&stored)
    );

    let tags = |count: usize| -> Vec<(String, String)> {
        (0..count)
            .map(|position| tag(&format!("key-{position}"), "value"))
            .collect()
    };

    // The two scopes word that one refusal differently, and each sentence states the bound
    // its own resource publishes rather than the count the request sent.
    for count in [
        MAX_TAGS_PER_BUCKET + 1,
        MAX_TAGS_PER_BUCKET + 10,
        MAX_TAGS_PER_BUCKET * 3,
    ] {
        let answer = put_bucket_tagging(&router, name, &tags(count)).await;
        assert_eq!(
            answer.error_code(),
            "BadRequest",
            "case {count} bucket tags: expected BadRequest, got {:?} with body {:?}",
            answer.error_code(),
            text(&answer)
        );
        assert_eq!(
            answer.error_message(),
            "Bucket tag count cannot be greater than 50",
            "case {count} bucket tags: expected the published sentence, got {:?}",
            answer.error_message()
        );
    }
    for count in [MAX_TAGS_PER_OBJECT + 1, MAX_TAGS_PER_OBJECT * 4] {
        let answer = put_object_tagging(&router, name, key, &tags(count)).await;
        assert_eq!(
            answer.error_message(),
            "Object tags cannot be greater than 10",
            "case {count} object tags: expected the object scope's own published sentence, \
             got {:?}",
            answer.error_message()
        );
    }

    // Each maximum itself is SERVED, so the rows above are about being past it.
    let at_maximum = put_bucket_tagging(&router, name, &tags(MAX_TAGS_PER_BUCKET)).await;
    assert_eq!(
        at_maximum.status,
        204,
        "a bucket tag set exactly at its maximum must be stored, got {} with body {:?}",
        at_maximum.status,
        text(&at_maximum)
    );
}

#[tokio::test]
async fn an_integer_page_bound_outside_its_range_names_the_parameter_and_the_bounds() {
    let router = router();
    let name = "refusal-messages-page-bound";
    made_bucket(&router, name).await;
    let key = "annotated.bin";
    let stored = put_object(&router, name, key, b"annotation-subject", &[]).await;
    assert_eq!(
        stored.status,
        200,
        "the object must be stored first, got {} with body {:?}",
        stored.status,
        text(&stored)
    );

    // `MaxAnnotationResults` publishes `range {min 1, max 1000}` and binds the query
    // parameter `max-annotation-results`
    // (`smithy_query(query="operation",operation="ListObjectAnnotations")`), which is what
    // the sentence names — never the modelled member.
    let expected = "Argument max-annotation-results must be an integer between 1 and 1000";
    for value in [1_001, 2_000, 100_000, 0, -1] {
        let answer = list_annotations(&router, name, key, value).await;
        assert_eq!(
            answer.status,
            400,
            "case max-annotation-results={value}: expected HTTP 400, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        assert_eq!(
            answer.error_code(),
            "InvalidArgument",
            "case max-annotation-results={value}: expected InvalidArgument, got {:?}",
            answer.error_code()
        );
        assert_eq!(
            answer.error_message(),
            expected,
            "case max-annotation-results={value}: expected {expected:?}, got {:?}",
            answer.error_message()
        );
    }

    // Both published bounds are SERVED, so the rows above are about being outside the range.
    for value in [1, 1_000] {
        let answer = list_annotations(&router, name, key, value).await;
        assert_eq!(
            answer.status,
            200,
            "case max-annotation-results={value}: a published bound must be served, got {} \
             with body {:?}",
            answer.status,
            text(&answer)
        );
    }

    // The SAME sentence for the other page bound this service publishes a range for, so the
    // wording belongs to the class rather than to one listing.
    let (min, max) = MAX_BUCKETS_RANGE;
    let max_buckets = format!("Argument max-buckets must be an integer between {min} and {max}");
    for value in [max + 1, min - 1] {
        let answer = send(
            &router,
            "GET",
            &format!("/?x-id=ListBuckets&max-buckets={value}"),
            &[],
            "",
        )
        .await;
        assert_eq!(
            answer.error_message(),
            max_buckets,
            "case max-buckets={value}: expected {max_buckets:?}, got {:?}",
            answer.error_message()
        );
    }
}

/// `GET /<bucket>/<key>?annotation&x-id=ListObjectAnnotations&max-annotation-results=<bound>`
/// — `ListObjectAnnotations` asked for a page of `bound` annotations.
async fn list_annotations(
    router: &MultiServiceRouter,
    bucket_name: &str,
    key: &str,
    bound: i64,
) -> Answer {
    let uri = format!(
        "/{bucket_name}/{key}?annotation&x-id=ListObjectAnnotations&\
         max-annotation-results={bound}"
    );
    send_bytes(router, "GET", &uri, &[], b"").await
}

#[tokio::test]
async fn a_cors_configuration_past_its_rule_ceiling_states_the_published_limit() {
    let router = router();
    let name = "refusal-messages-cors";
    made_bucket(&router, name).await;

    // Fixed text naming the published ceiling, so one string serves every row however many
    // rules the document carried.
    let expected = format!(
        "The number of CORS rules should not exceed allowed limit of {MAX_CORS_RULES} rules."
    );

    for count in [MAX_CORS_RULES + 1, MAX_CORS_RULES + 10, MAX_CORS_RULES * 2] {
        let answer = put_cors(&router, name, &cors_rules(count)).await;
        assert_eq!(
            answer.status,
            400,
            "case {count} rules: expected HTTP 400, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        assert_eq!(
            answer.error_code(),
            "InvalidRequest",
            "case {count} rules: expected InvalidRequest, got {:?}",
            answer.error_code()
        );
        assert_eq!(
            answer.error_message(),
            expected,
            "case {count} rules: expected {expected:?}, got {:?}",
            answer.error_message()
        );

        // A refused put records nothing, so the bucket still carries no cors configuration
        // at all rather than a partly applied one.
        let read = get_cors(&router, name).await;
        assert_eq!(
            read.error_code(),
            "NoSuchCORSConfiguration",
            "case {count} rules: a refused put leaves the bucket with no cors configuration, \
             got {:?} with status {}",
            read.error_code(),
            read.status
        );
    }

    // The ceiling itself is SERVED, so the rows above are about being past it.
    let served = put_cors(&router, name, &cors_rules(MAX_CORS_RULES)).await;
    assert_eq!(
        served.status,
        200,
        "a configuration of exactly {MAX_CORS_RULES} rules must be stored, got {} with body \
         {:?}",
        served.status,
        text(&served)
    );
}

/// `count` `CORSRule`s, each allowing `GET` from an origin of its own — the two members
/// `CORSRule` requires (`smithy_query(query="shape",shape="CORSRule")`).
fn cors_rules(count: usize) -> String {
    (0..count)
        .map(|position| {
            format!(
                "<CORSRule><AllowedMethod>GET</AllowedMethod>\
                 <AllowedOrigin>https://host-{position}.example.com</AllowedOrigin></CORSRule>"
            )
        })
        .collect::<Vec<String>>()
        .concat()
}

#[tokio::test]
async fn a_logging_target_bucket_nothing_holds_states_the_wire_sentence() {
    let router = router();
    let name = "refusal-messages-logging";
    made_bucket(&router, name).await;

    // Fixed text with no variable part: the target the request named is not in it, which is
    // why every row expects the same string.
    let expected = "The target bucket for logging does not exist";

    // Each row names a target this account does not hold, differing in HOW: a name nothing
    // was created under, a prefix of the source bucket's name, that name in another case,
    // and no name at all.
    for target in [
        "refusal-messages-logging-target",
        "refusal-messages",
        "Refusal-Messages-Logging",
        "",
    ] {
        let answer = put_logging(&router, name, target).await;
        assert_eq!(
            answer.status,
            400,
            "case {target:?}: expected HTTP 400, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        assert_eq!(
            answer.error_code(),
            "InvalidTargetBucketForLogging",
            "case {target:?}: expected InvalidTargetBucketForLogging, got {:?}",
            answer.error_code()
        );
        assert_eq!(
            answer.error_message(),
            expected,
            "case {target:?}: expected {expected:?}, got {:?}",
            answer.error_message()
        );
    }

    // A target this account DOES hold is accepted, so the rows above are about the target
    // not existing: "you can have your logs delivered to any bucket that you own, including
    // the same bucket that is being logged" (`LoggingEnabled$TargetBucket`).
    let served = put_logging(&router, name, name).await;
    assert_eq!(
        served.status,
        200,
        "a target bucket this account holds must be accepted, got {} with body {:?}",
        served.status,
        text(&served)
    );
}

/// `PUT /<bucket>?logging` — `PutBucketLogging` delivering this bucket's logs to `target`.
async fn put_logging(router: &MultiServiceRouter, bucket_name: &str, target: &str) -> Answer {
    let payload = format!(
        "<BucketLoggingStatus xmlns=\"{XMLNS}\"><LoggingEnabled>\
         <TargetBucket>{target}</TargetBucket><TargetPrefix>logs/</TargetPrefix>\
         </LoggingEnabled></BucketLoggingStatus>"
    );
    send(
        router,
        "PUT",
        &format!("/{bucket_name}?logging"),
        &[],
        &payload,
    )
    .await
}

#[tokio::test]
async fn an_acl_body_beside_an_acl_header_is_content_the_call_does_not_take() {
    let router = router();
    let name = "refusal-messages-acl";
    made_bucket(&router, name).await;
    // A bucket is created with the Object Ownership setting that DISABLES ACLs, and with
    // Block Public Access on, so an ACL write would be refused for either before anything
    // about the body-beside-a-header rule could be observed.
    let enabled = put_ownership_controls(&router, name, &["ObjectWriter"]).await;
    assert_eq!(
        enabled.status,
        200,
        "the bucket must take the ownership setting that enables ACLs, got {} with body {:?}",
        enabled.status,
        text(&enabled)
    );
    allow_public_access(&router, name).await;
    let key = "acl-subject.bin";
    let stored = put_object(&router, name, key, b"acl-subject", &[]).await;
    assert_eq!(
        stored.status,
        200,
        "the object must be stored first, got {} with body {:?}",
        stored.status,
        text(&stored)
    );

    let body = access_control_policy(
        None,
        &[GrantXml::group(ALL_USERS_GROUP_URI, PERMISSION_READ)],
    );
    let grant = format!("uri={ALL_USERS_GROUP_URI}");

    // Each row names the ACL in the HEADERS and sends a payload as well, which is content
    // the call does not take; the rows differ in WHICH header form named it.
    let with_body: &[(&str, Vec<(&str, &str)>)] = &[
        ("a canned ACL", vec![("x-amz-acl", "private")]),
        ("an explicit grant header", vec![("x-amz-grant-read", grant.as_str())]),
        (
            "another explicit grant header",
            vec![("x-amz-grant-full-control", grant.as_str())],
        ),
    ];
    for (case, headers) in with_body {
        for (operation, answer) in [
            (
                "PutObjectAcl",
                put_object_acl(&router, name, key, "", headers, &body).await,
            ),
            (
                "PutBucketAcl",
                put_bucket_acl(&router, name, headers, &body).await,
            ),
        ] {
            assert_eq!(
                answer.status,
                400,
                "case {operation} with {case}: expected HTTP 400, got {} with body {:?}",
                answer.status,
                text(&answer)
            );
            assert_eq!(
                answer.error_code(),
                "UnexpectedContent",
                "case {operation} with {case}: expected UnexpectedContent, got {:?}",
                answer.error_code()
            );
            assert_eq!(
                answer.error_message(),
                "This request does not support content",
                "case {operation} with {case}: expected the published sentence, got {:?}",
                answer.error_message()
            );
        }
    }

    // One ACL named in two HEADER forms and NO body is the other class: a bad argument pair,
    // which a client branching on the code has to be able to tell from content it may not
    // send at all.
    let two_header_forms: Vec<(&str, &str)> =
        vec![("x-amz-acl", "private"), ("x-amz-grant-read", grant.as_str())];
    for (operation, answer) in [
        (
            "PutObjectAcl",
            put_object_acl(&router, name, key, "", &two_header_forms, "").await,
        ),
        (
            "PutBucketAcl",
            put_bucket_acl(&router, name, &two_header_forms, "").await,
        ),
    ] {
        assert_eq!(
            answer.error_code(),
            "InvalidRequest",
            "case {operation} naming its ACL in two header forms: expected InvalidRequest, \
             got {:?} with body {:?}",
            answer.error_code(),
            text(&answer)
        );
        assert_eq!(
            answer.error_message(),
            "Specifying both Canned ACLs and Header Grants is not allowed",
            "case {operation} naming its ACL in two header forms: expected the published \
             sentence, got {:?}",
            answer.error_message()
        );
    }

    // The body ALONE is served, so the rows above are about the body arriving BESIDE a
    // header rather than about the document.
    let served = put_object_acl(&router, name, key, "", &[], &body).await;
    assert_eq!(
        served.status,
        200,
        "an AccessControlPolicy body on its own must be applied, got {} with body {:?}",
        served.status,
        text(&served)
    );
}

/// `PUT /<bucket>?acl` — `PutBucketAcl`, with whatever headers and body the case names.
async fn put_bucket_acl(
    router: &MultiServiceRouter,
    bucket_name: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Answer {
    send(router, "PUT", &format!("/{bucket_name}?acl"), headers, body).await
}

#[tokio::test]
async fn an_object_encryption_update_reports_an_unresolvable_address_as_its_own_not_found() {
    let router = router_with_kms();
    let name = "refusal-messages-encryption";
    made_bucket(&router, name).await;
    set_versioning(&router, name, "Enabled").await;
    // A key KMS really holds for this account, so every row below fails on the ADDRESS and
    // not on the key the payload names.
    let (_, key_arn) = created_key(&router, "refusal-messages-encryption").await;

    let key = "subject.bin";
    let stored = put_object(&router, name, key, b"encryption-subject", &[]).await;
    assert_eq!(
        stored.status,
        200,
        "the object must be stored first, got {} with body {:?}",
        stored.status,
        text(&stored)
    );
    let version_id = stored
        .header("x-amz-version-id")
        .expect("a versioning-enabled bucket reports the version id it minted");

    // A second version, then a PERMANENT delete of the first: an id this bucket really
    // minted that names no version of it any more.
    let replaced = put_object(&router, name, key, b"encryption-subject-2", &[]).await;
    assert_eq!(
        replaced.status,
        200,
        "the object must be overwritten, got {} with body {:?}",
        replaced.status,
        text(&replaced)
    );
    let removed = delete_object(&router, name, key, &format!("?versionId={version_id}")).await;
    assert_eq!(
        removed.status,
        204,
        "the version must be permanently deleted, got {} with body {:?}",
        removed.status,
        text(&removed)
    );

    // Each row is an address this bucket does not hold, differing in HOW: a version it
    // minted and then permanently deleted, a well-formed id it never minted, a key never
    // written, and a key never written under a version id as well.
    let never_minted = "0123456789abcdef0123456789abcdef";
    let rows: &[(&str, &str, String)] = &[
        (
            "a version this bucket minted and then permanently deleted",
            key,
            format!("&versionId={version_id}"),
        ),
        (
            "a well-formed version id this bucket never minted",
            key,
            format!("&versionId={never_minted}"),
        ),
        ("a key never written", "absent.bin", String::new()),
        (
            "a key never written, under a version id",
            "absent.bin",
            format!("&versionId={never_minted}"),
        ),
    ];
    for (case, addressed, query) in rows {
        let answer = update_object_encryption(
            &router,
            name,
            addressed,
            query,
            &encryption_payload(&key_arn, None),
        )
        .await;
        assert_eq!(
            answer.status,
            404,
            "case {case:?}: an address this bucket does not hold is HTTP 404, got {} with \
             body {:?}",
            answer.status,
            text(&answer)
        );
        assert_eq!(
            answer.error_code(),
            "NoSuchKey",
            "case {case:?}: expected NoSuchKey, the one not-found error this operation \
             publishes, got {:?}",
            answer.error_code()
        );
    }

    // The CURRENT version is served, so every row above failed on the address rather than on
    // the request being unservable.
    let served =
        update_object_encryption(&router, name, key, "", &encryption_payload(&key_arn, None))
            .await;
    assert_eq!(
        served.status,
        200,
        "the held object must be updated, got {} with body {:?}",
        served.status,
        text(&served)
    );
}
