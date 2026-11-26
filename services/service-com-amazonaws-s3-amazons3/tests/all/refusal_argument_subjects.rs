// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHAT an `InvalidArgument` refusal of this service is about: the token
//! form a `versionId` has to carry, and the field the envelope's `<ArgumentName>` names.
//!
//! Three rules, each asserted over a table of inputs rather than on one request:
//!
//! 1. A `versionId` carries an opaque token of the form S3 ISSUES version ids in. A value
//!    written in another form this service publishes — an object's ENTITY TAG, which is a
//!    hexadecimal digest of the object's data, alone or with the `-<parts>` of a multipart tag
//!    — names no version of any object, so it is refused as a malformed ARGUMENT (HTTP 400
//!    `InvalidArgument`, `Invalid version id specified`, naming `versionId` and echoing the
//!    value) instead of being reported as an absent version. HTTP 404 `NoSuchVersion` is for a
//!    token of the ISSUED form that names no version the bucket holds, which is why the id the
//!    store minted keeps resolving and a token like it does not.
//! 2. An `InvalidArgument` refusal about the SERVER-SIDE ENCRYPTION parameters of a request
//!    names the `x-amz-server-side-encryption` header those parameters belong to in its
//!    `<ArgumentName>` and echoes no value — the members carry a secret key and a digest of
//!    one. The refusal answered under another code, the digest of the key not matching the key
//!    (`InvalidDigest`), names no argument: the pair belongs to the `InvalidArgument` document.
//! 3. An `InvalidArgument` refusal about one MEMBER inside a container names that member by its
//!    dotted wire path — a key-name filter rule's `Name` is `FilterRule.Name`, not the
//!    `FilterRule` element carrying it — while a refusal whose fault is the container itself
//!    (two configurations the filter rules leave ambiguous) names the container.

use service_com_amazonaws_s3_amazons3::state::{is_entity_tag_shaped, VERSION_ID_CHARS};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send, Answer};
use super::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};
use super::object_attributes::get_object_attributes;
use super::object_versioning::{head_object, set_versioning};

/// The XML namespace botocore puts on an S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The code and status every refusal of a value the request carried answers with
/// (Amazon S3 API Reference, *Error responses*).
const INVALID_ARGUMENT: &str = "InvalidArgument";
const INVALID_ARGUMENT_STATUS: u16 = 400;

/// The `<Message>` S3 answers a `versionId` that is not a version id with, fixed text.
const INVALID_VERSION_ID: &str = "Invalid version id specified";

/// The parameter such a refusal names: the query parameter every operation that takes one
/// binds its `VersionId` from
/// (`smithy_query(query="operation",operation="GetObjectAttributes")` reports
/// `VersionId: {httpQuery: "versionId"}`).
const VERSION_ID_ARGUMENT: &str = "versionId";

/// What an absent version is answered with instead.
const NO_SUCH_VERSION: &str = "NoSuchVersion";

/// The header an `InvalidArgument` refusal about a request's encryption parameters names.
const SERVER_SIDE_ENCRYPTION_HEADER: &str = "x-amz-server-side-encryption";

/// The three members one customer-provided key travels in
/// (`smithy_query(query="operation",operation="PutObject")` reports `SSECustomerAlgorithm`,
/// `SSECustomerKey` and `SSECustomerKeyMD5` as `@httpHeader` members of its input).
const SSE_CUSTOMER_ALGORITHM_HEADER: &str = "x-amz-server-side-encryption-customer-algorithm";
const SSE_CUSTOMER_KEY_HEADER: &str = "x-amz-server-side-encryption-customer-key";
const SSE_CUSTOMER_KEY_MD5_HEADER: &str = "x-amz-server-side-encryption-customer-key-MD5";

/// The one algorithm S3 serves a customer-provided key under
/// (`PutObjectRequest$SSECustomerAlgorithm`: "`AES256` is the only supported value").
const SSE_CUSTOMER_ALGORITHM: &str = "AES256";

/// The fixed sentences S3 answers each unusable set of those members with.
const NEEDS_ALGORITHM: &str = "Requests specifying Server Side Encryption with Customer \
                               provided keys must provide a valid encryption algorithm.";
const NEEDS_KEY: &str = "Requests specifying Server Side Encryption with Customer provided \
                         keys must provide an appropriate secret key.";
const NEEDS_DIGEST: &str = "Requests specifying Server Side Encryption with Customer provided \
                            keys must provide the client calculated MD5 of the secret key.";
const DIGEST_MISMATCH: &str = "The calculated MD5 hash of the key did not match the hash that \
                               was provided.";

/// The code the last of those carries: the digest a request computed for its own payload is
/// not the digest the service computes ("InvalidDigest", Amazon S3 *Error responses*).
const INVALID_DIGEST: &str = "InvalidDigest";

/// The `<ArgumentName>` a refusal about one filter rule's `Name` names, and the one a document
/// its rules leave ambiguous names.
const FILTER_RULE_NAME_ARGUMENT: &str = "FilterRule.Name";
const FILTER_RULE_ARGUMENT: &str = "FilterRule";

/// The sentence a rule name outside `FilterRuleName`'s values is refused with.
const INVALID_FILTER_RULE_NAME: &str = "filter rule name must be either prefix or suffix";

/// A destination in this emulator's account for a notification configuration to publish to.
const QUEUE_ARN: &str = "arn:aws:sqs:us-east-1:123456789012:object-events";

/// A customer-provided key of the length `AES256` takes, base64 encoded, with the base64 MD5
/// of its bytes beside it — the pair a client's SDK computes for the two headers.
fn customer_key(seed: u8) -> (String, String) {
    use base64::Engine as _;
    use md5::Digest as _;
    let key: Vec<u8> = (0..32u8)
        .map(|index| index.wrapping_mul(31).wrapping_add(seed))
        .collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&key);
    let digest =
        base64::engine::general_purpose::STANDARD.encode(md5::Md5::digest(&key).as_slice());
    (encoded, digest)
}

/// Assert `answer` is the `InvalidArgument` refusal `message` names, whose `<ArgumentName>` is
/// `argument` and whose `<ArgumentValue>` is `value` (`None` for a refusal that echoes none).
fn assert_invalid_argument(
    case: &str,
    answer: &Answer,
    message: &str,
    argument: &str,
    value: Option<&str>,
) {
    assert_eq!(
        answer.status,
        INVALID_ARGUMENT_STATUS,
        "case {case:?}: expected HTTP {INVALID_ARGUMENT_STATUS}, got {} with body {:?}",
        answer.status,
        text(answer)
    );
    assert_eq!(
        answer.error_code(),
        INVALID_ARGUMENT,
        "case {case:?}: expected the code {INVALID_ARGUMENT:?}, got {:?}",
        answer.error_code()
    );
    assert_eq!(
        answer.error_message(),
        message,
        "case {case:?}: expected the message {message:?}, got {:?}",
        answer.error_message()
    );
    assert_eq!(
        answer.element("ArgumentName").as_deref(),
        Some(argument),
        "case {case:?}: expected the refusal to name {argument:?}, got body {:?}",
        text(answer)
    );
    assert_eq!(
        answer.element("ArgumentValue").as_deref(),
        value,
        "case {case:?}: expected the refusal to carry the value {value:?}, got body {:?}",
        text(answer)
    );
}

/// How a `versionId` row must be classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// Not a version id at all: the argument refusal, echoing the token.
    Malformed,
    /// A token of the issued form that names no version the bucket holds.
    Absent,
    /// The id the store minted for the version the read addresses.
    Served,
}

#[tokio::test]
async fn an_entity_tag_is_not_a_version_id_wherever_one_is_taken() {
    let router = router();
    let name = "argument-subject-versions";
    bucket(&router, name).await;
    set_versioning(&router, name, "Enabled").await;

    // Two objects, so the tags under test are the tags of DIFFERENT bodies rather than one
    // recorded digest: each is read back from the write that stored it, which is where a
    // client that mistakes a tag for a version id gets it.
    let key = "ledger/current.bin";
    let written = put_object(&router, name, key, b"argument-subject-body", &[]).await;
    assert_eq!(
        written.status,
        200,
        "the object must be stored first, got {} with body {:?}",
        written.status,
        text(&written)
    );
    let minted = written
        .header("x-amz-version-id")
        .expect("a versioning-enabled bucket reports the version id it minted");
    let tag = written
        .header("ETag")
        .expect("a write reports the entity tag of the object it stored");
    let tag = tag.trim_matches('"').to_string();
    let sibling = put_object(&router, name, "ledger/sibling.bin", b"another body", &[]).await;
    let sibling_tag = sibling
        .header("ETag")
        .expect("a write reports the entity tag of the object it stored")
        .trim_matches('"')
        .to_string();

    // The id this store HANDS OUT is never written in the tag's form, which is what keeps the
    // two apart wherever one of them is judged.
    for (index, subject) in ["mint/one.bin", "mint/two.bin", "mint/three.bin"]
        .into_iter()
        .enumerate()
    {
        let stored = put_object(&router, name, subject, b"mint-shape", &[]).await;
        let id = stored
            .header("x-amz-version-id")
            .expect("a versioning-enabled bucket reports the version id it minted");
        assert!(
            !is_entity_tag_shaped(&id),
            "case {index}: a minted version id must not be written in an entity tag's form, \
             got {id:?}"
        );
        assert_eq!(
            id.chars().count(),
            VERSION_ID_CHARS,
            "case {index}: a minted version id carries the published number of characters, \
             got {id:?}"
        );
    }

    // The rows differ along the FORM of the token: the tag of the object addressed, the tag of
    // another object, a tag whose digest is written in one character, the multipart form of a
    // tag, a token of the issued form the store never minted, Amazon S3's own published
    // example of an issued id, and the id the store really minted.
    let rows: Vec<(String, String, Expected)> = vec![
        (
            "the entity tag of the object addressed".to_string(),
            tag.clone(),
            Expected::Malformed,
        ),
        (
            "the entity tag of another object".to_string(),
            sibling_tag.clone(),
            Expected::Malformed,
        ),
        (
            "an entity tag's digest written in one character".to_string(),
            "e".repeat(tag.chars().count()),
            Expected::Malformed,
        ),
        (
            "the multipart form of that tag".to_string(),
            format!("{tag}-2"),
            Expected::Malformed,
        ),
        (
            "a token of the issued form the store never minted".to_string(),
            "z".repeat(VERSION_ID_CHARS),
            Expected::Absent,
        ),
        (
            // Amazon S3 User Guide, *Working with object versioning*.
            "the published example of an issued id".to_string(),
            "3HL4kqtJlcpXroDTDmJ_RGrmHJ1PyFCY".to_string(),
            Expected::Absent,
        ),
        (
            "the id the store minted".to_string(),
            minted.clone(),
            Expected::Served,
        ),
    ];

    for (case, version_id, expected) in rows {
        assert_ne!(
            (version_id.as_str(), expected),
            (minted.as_str(), Expected::Malformed),
            "case {case:?}: no row may call the minted id malformed"
        );
        let attributes = get_object_attributes(
            &router,
            name,
            key,
            &format!("&versionId={version_id}"),
            &["ETag", "ObjectSize"],
        )
        .await;
        let read = get_object(&router, name, key, &format!("?versionId={version_id}"), &[]).await;
        let headed = head_object(&router, name, key, &format!("?versionId={version_id}")).await;

        match expected {
            Expected::Malformed => {
                for (operation, answer) in [
                    ("GetObjectAttributes", &attributes),
                    ("GetObject", &read),
                ] {
                    assert_invalid_argument(
                        &format!("{case}/{operation}"),
                        answer,
                        INVALID_VERSION_ID,
                        VERSION_ID_ARGUMENT,
                        Some(version_id.as_str()),
                    );
                }
                // HEAD carries no body, so the status is the whole of its answer.
                assert_eq!(
                    headed.status,
                    INVALID_ARGUMENT_STATUS,
                    "case {case:?}/HeadObject: expected HTTP {INVALID_ARGUMENT_STATUS} for \
                     {version_id:?}, got {}",
                    headed.status
                );
            }
            Expected::Absent => {
                for (operation, answer) in [
                    ("GetObjectAttributes", &attributes),
                    ("GetObject", &read),
                ] {
                    assert_eq!(
                        (answer.status, answer.error_code()),
                        (404, NO_SUCH_VERSION.to_string()),
                        "case {case:?}/{operation}: expected 404 {NO_SUCH_VERSION} for \
                         {version_id:?}, got {} with body {:?}",
                        answer.status,
                        text(answer)
                    );
                }
                assert_eq!(
                    headed.status, 404,
                    "case {case:?}/HeadObject: expected HTTP 404 for {version_id:?}, got {}",
                    headed.status
                );
            }
            Expected::Served => {
                for (operation, answer) in [
                    ("GetObjectAttributes", &attributes),
                    ("GetObject", &read),
                    ("HeadObject", &headed),
                ] {
                    assert_eq!(
                        answer.status, 200,
                        "case {case:?}/{operation}: the minted version must be served, got {} \
                         with body {:?}",
                        answer.status,
                        text(answer)
                    );
                }
            }
        }
    }

    // A refused read changed nothing: the object is still there under the id its write minted,
    // and its tag is still the one that write reported.
    let served = get_object(&router, name, key, &format!("?versionId={minted}"), &[]).await;
    assert_eq!(
        (
            served.status,
            served.header("ETag").unwrap_or_default().trim_matches('"').to_string()
        ),
        (200, tag),
        "the addressed version must survive every refused read, got {} with body {:?}",
        served.status,
        text(&served)
    );
}

/// One row of the encryption-parameter table: the case, the three members as the request
/// carried them, and the code, the sentence and the `<ArgumentName>` the refusal must carry
/// (`None` for the one answered under a code whose document names no argument).
type EncryptionRow<'a> = (
    &'a str,
    Vec<(&'a str, String)>,
    &'a str,
    &'a str,
    Option<&'a str>,
);

#[tokio::test]
async fn an_encryption_parameter_refusal_names_the_server_side_encryption_header() {
    let router = router();
    let name = "argument-subject-encryption";
    bucket(&router, name).await;
    let key = "sealed/object.bin";
    let written = put_object(&router, name, key, b"encryption-subject-body", &[]).await;
    assert_eq!(
        written.status,
        200,
        "the object must be stored first, got {} with body {:?}",
        written.status,
        text(&written)
    );

    let (first_key, first_digest) = customer_key(1);
    let (other_key, _) = customer_key(2);
    let short_key = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode([7u8; 16])
    };

    // The rows differ along WHICH of the three members cannot be used and how: the algorithm
    // absent, the algorithm not the one S3 serves, the key absent, the key not base64, the key
    // of another length, the digest absent, and the digest not the digest of the key beside it
    // — the last being the one answered under another code.
    let rows: Vec<EncryptionRow<'_>> = vec![
        (
            "a key with no algorithm",
            vec![
                (SSE_CUSTOMER_KEY_HEADER, first_key.clone()),
                (SSE_CUSTOMER_KEY_MD5_HEADER, first_digest.clone()),
            ],
            INVALID_ARGUMENT,
            NEEDS_ALGORITHM,
            Some(SERVER_SIDE_ENCRYPTION_HEADER),
        ),
        (
            "an algorithm S3 does not serve",
            vec![
                (SSE_CUSTOMER_ALGORITHM_HEADER, "AES128".to_string()),
                (SSE_CUSTOMER_KEY_HEADER, first_key.clone()),
                (SSE_CUSTOMER_KEY_MD5_HEADER, first_digest.clone()),
            ],
            INVALID_ARGUMENT,
            NEEDS_ALGORITHM,
            Some(SERVER_SIDE_ENCRYPTION_HEADER),
        ),
        (
            "an algorithm and no key",
            vec![(
                SSE_CUSTOMER_ALGORITHM_HEADER,
                SSE_CUSTOMER_ALGORITHM.to_string(),
            )],
            INVALID_ARGUMENT,
            NEEDS_KEY,
            Some(SERVER_SIDE_ENCRYPTION_HEADER),
        ),
        (
            "a key that is not base64 at all",
            vec![
                (
                    SSE_CUSTOMER_ALGORITHM_HEADER,
                    SSE_CUSTOMER_ALGORITHM.to_string(),
                ),
                (SSE_CUSTOMER_KEY_HEADER, "not base64".to_string()),
                (SSE_CUSTOMER_KEY_MD5_HEADER, first_digest.clone()),
            ],
            INVALID_ARGUMENT,
            NEEDS_KEY,
            Some(SERVER_SIDE_ENCRYPTION_HEADER),
        ),
        (
            "a key of another length",
            vec![
                (
                    SSE_CUSTOMER_ALGORITHM_HEADER,
                    SSE_CUSTOMER_ALGORITHM.to_string(),
                ),
                (SSE_CUSTOMER_KEY_HEADER, short_key.clone()),
                (SSE_CUSTOMER_KEY_MD5_HEADER, first_digest.clone()),
            ],
            INVALID_ARGUMENT,
            NEEDS_KEY,
            Some(SERVER_SIDE_ENCRYPTION_HEADER),
        ),
        (
            "a key with no digest of it",
            vec![
                (
                    SSE_CUSTOMER_ALGORITHM_HEADER,
                    SSE_CUSTOMER_ALGORITHM.to_string(),
                ),
                (SSE_CUSTOMER_KEY_HEADER, first_key.clone()),
            ],
            INVALID_ARGUMENT,
            NEEDS_DIGEST,
            Some(SERVER_SIDE_ENCRYPTION_HEADER),
        ),
        (
            "another key's digest",
            vec![
                (
                    SSE_CUSTOMER_ALGORITHM_HEADER,
                    SSE_CUSTOMER_ALGORITHM.to_string(),
                ),
                (SSE_CUSTOMER_KEY_HEADER, other_key.clone()),
                (SSE_CUSTOMER_KEY_MD5_HEADER, first_digest.clone()),
            ],
            INVALID_DIGEST,
            DIGEST_MISMATCH,
            None,
        ),
    ];

    for (case, headers, code, message, argument) in rows {
        let borrowed: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();

        // The same members, on a READ of a stored object and on a WRITE of a new one: one rule
        // reads them wherever they arrive.
        let read = get_object(&router, name, key, "", &borrowed).await;
        let stored = put_object(&router, name, "sealed/written.bin", b"body", &borrowed).await;

        for (operation, answer) in [("GetObject", &read), ("PutObject", &stored)] {
            let case = format!("{case}/{operation}");
            assert_eq!(
                answer.status,
                INVALID_ARGUMENT_STATUS,
                "case {case:?}: expected HTTP {INVALID_ARGUMENT_STATUS}, got {} with body {:?}",
                answer.status,
                text(answer)
            );
            assert_eq!(
                answer.error_code(),
                code,
                "case {case:?}: expected the code {code:?}, got {:?}",
                answer.error_code()
            );
            assert_eq!(
                answer.error_message(),
                message,
                "case {case:?}: expected the message {message:?}, got {:?}",
                answer.error_message()
            );
            assert_eq!(
                answer.element("ArgumentName").as_deref(),
                argument,
                "case {case:?}: expected the refusal to name {argument:?}, got body {:?}",
                text(answer)
            );
            assert_eq!(
                answer.element("ArgumentValue"),
                None,
                "case {case:?}: an encryption-parameter refusal echoes no value, got body {:?}",
                text(answer)
            );
        }
    }

    // Nothing of the refused writes was stored, and the object the reads addressed still
    // answers without any of those members.
    let served = get_object(&router, name, key, "", &[]).await;
    assert_eq!(
        served.status,
        200,
        "the stored object must survive every refused request, got {} with body {:?}",
        served.status,
        text(&served)
    );
    let never_written = get_object(&router, name, "sealed/written.bin", "", &[]).await;
    assert_eq!(
        never_written.error_code(),
        "NoSuchKey",
        "a refused write stores nothing, got {} with body {:?}",
        never_written.status,
        text(&never_written)
    );
}

/// A notification document carrying one queue configuration whose key-name filter holds
/// `rules`, each a `(Name, Value)` pair as the request spells them.
fn notification_payload(events: &[&str], rules: &[(&str, &str)]) -> String {
    let mut body = format!(
        "<NotificationConfiguration xmlns=\"{XMLNS}\"><QueueConfiguration>\
         <Queue>{QUEUE_ARN}</Queue>"
    );
    for event in events {
        body.push_str(&format!("<Event>{event}</Event>"));
    }
    body.push_str("<Filter><S3Key>");
    for (name, value) in rules {
        body.push_str(&format!(
            "<FilterRule><Name>{name}</Name><Value>{value}</Value></FilterRule>"
        ));
    }
    body.push_str("</S3Key></Filter></QueueConfiguration></NotificationConfiguration>");
    body
}

/// `PUT /<bucket>?notification` carrying `body`, with destination validation skipped so the
/// answer is about the DOCUMENT rather than about whether the queue exists.
async fn put_notification(router: &MultiServiceRouter, bucket: &str, body: &str) -> Answer {
    send(
        router,
        "PUT",
        &format!("/{bucket}?notification"),
        &[("x-amz-skip-destination-validation", "true")],
        body,
    )
    .await
}

#[tokio::test]
async fn a_filter_rule_refusal_names_the_member_at_fault() {
    let router = router();

    // (case, the document the put carries, the sentence it is refused with, the argument it
    // names, the value it echoes). The rows differ along WHAT is wrong with the rules: a name
    // outside `FilterRuleName`'s published values (`prefix`, `suffix`), in several ways; a
    // second rule of a kind the filter already carries, for each kind; and a document whose
    // rules leave two configurations ambiguous, where the fault is no single member.
    let rows: Vec<(&str, String, &str, &str, Option<String>)> = vec![
        (
            "a name outside the published values",
            notification_payload(&["s3:ObjectCreated:Put"], &[("Prefixes", "logs/")]),
            INVALID_FILTER_RULE_NAME,
            FILTER_RULE_NAME_ARGUMENT,
            Some("Prefixes".to_string()),
        ),
        (
            "a name of another attribute",
            notification_payload(&["s3:ObjectCreated:Put"], &[("extension", ".csv")]),
            INVALID_FILTER_RULE_NAME,
            FILTER_RULE_NAME_ARGUMENT,
            Some("extension".to_string()),
        ),
        (
            "a published name with inner whitespace",
            notification_payload(&["s3:ObjectCreated:Put"], &[("pre fix", "logs/")]),
            INVALID_FILTER_RULE_NAME,
            FILTER_RULE_NAME_ARGUMENT,
            Some("pre fix".to_string()),
        ),
        (
            "a second prefix rule in one filter",
            notification_payload(
                &["s3:ObjectCreated:Put"],
                &[("prefix", "one/"), ("prefix", "two/")],
            ),
            "Cannot specify more than one prefix rule in a filter.",
            FILTER_RULE_NAME_ARGUMENT,
            None,
        ),
        (
            "a second suffix rule in one filter",
            notification_payload(
                &["s3:ObjectCreated:Put"],
                &[("suffix", ".csv"), ("suffix", ".tsv")],
            ),
            "Cannot specify more than one suffix rule in a filter.",
            FILTER_RULE_NAME_ARGUMENT,
            None,
        ),
        (
            "a second prefix rule beside a suffix rule",
            notification_payload(
                &["s3:ObjectCreated:Put"],
                &[("prefix", "one/"), ("suffix", ".csv"), ("prefix", "two/")],
            ),
            "Cannot specify more than one prefix rule in a filter.",
            FILTER_RULE_NAME_ARGUMENT,
            None,
        ),
    ];

    for (index, (case, payload, message, argument, value)) in rows.into_iter().enumerate() {
        let bucket_name = format!("argument-subject-notification-{index}");
        bucket(&router, &bucket_name).await;
        let answer = put_notification(&router, &bucket_name, &payload).await;
        assert_invalid_argument(case, &answer, message, argument, value.as_deref());

        // A refused document is not recorded: the bucket still reports no rule at all.
        let reported = send(
            &router,
            "GET",
            &format!("/{bucket_name}?notification"),
            &[],
            "",
        )
        .await;
        assert_eq!(
            reported.element("FilterRule"),
            None,
            "case {case:?}: a refused document must store no rule, got body {:?}",
            text(&reported)
        );
    }

    // The refusal whose fault IS the container keeps naming the container: two configurations
    // of one event whose prefixes overlap leave the document unable to say which rule an
    // object matches, and no single rule is at fault.
    let ambiguous_bucket = "argument-subject-notification-ambiguous";
    bucket(&router, ambiguous_bucket).await;
    let ambiguous = format!(
        "<NotificationConfiguration xmlns=\"{XMLNS}\">\
         <QueueConfiguration><Id>outer</Id><Queue>{QUEUE_ARN}</Queue>\
         <Event>s3:ObjectCreated:*</Event><Filter><S3Key>\
         <FilterRule><Name>prefix</Name><Value>CategoryA/</Value></FilterRule>\
         </S3Key></Filter></QueueConfiguration>\
         <QueueConfiguration><Id>inner</Id><Queue>{QUEUE_ARN}</Queue>\
         <Event>s3:ObjectCreated:*</Event><Filter><S3Key>\
         <FilterRule><Name>prefix</Name><Value>CategoryA/sub/</Value></FilterRule>\
         </S3Key></Filter></QueueConfiguration></NotificationConfiguration>"
    );
    let refused = put_notification(&router, ambiguous_bucket, &ambiguous).await;
    assert_eq!(
        (
            refused.status,
            refused.error_code(),
            refused.element("ArgumentName")
        ),
        (
            INVALID_ARGUMENT_STATUS,
            INVALID_ARGUMENT.to_string(),
            Some(FILTER_RULE_ARGUMENT.to_string())
        ),
        "case \"two configurations the rules leave ambiguous\": expected the container to be \
         named, got {} with body {:?}",
        refused.status,
        text(&refused)
    );
}
