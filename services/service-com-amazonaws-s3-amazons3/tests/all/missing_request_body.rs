// SPDX-License-Identifier: CC0-1.0

//! The refusal a write earns when the DOCUMENT its operation requires in the body never
//! arrived.
//!
//! The rule under test: where this service's model binds a request's whole body as a
//! structured document and marks that member `@required`
//! (`smithy_query(query="operation",operation="PutBucketOwnershipControls")` reports
//! `OwnershipControls: {httpPayload: true, required: true}`, and
//! [`REQUIRED_XML_DOCUMENT_PAYLOAD_MEMBERS`] is the catalog of every such member), a request
//! that carries NO body sent no document at all — which Amazon S3 refuses with
//! `MissingRequestBodyError` and HTTP 400 ("This happens when the user sends an empty XML
//! document as a request", Amazon S3 API Reference, *Error responses*), not with the schema
//! refusal a document that DID arrive and did not validate earns.
//!
//! Why it matters: the two refusals ask the caller for different things. `MalformedXML` says
//! the document it sent is wrong, and a client, a runbook or a retry that branches on the
//! code has nothing to fix; `MissingRequestBodyError` says to send one. And for a container
//! whose members are all optional (a logging status, a notification configuration) a body
//! read as "an empty container" is worse than a refusal: the call is SERVED, and it turns
//! the bucket's configuration off.
//!
//! The table spans what the rule generalises over:
//!
//! * the OPERATION and the document it requires — every member of the catalog
//!   ([`every_required_document_is_covered`] holds the table to it), which covers a
//!   container of required members, one whose members are all optional, and a list;
//! * the METHOD (`PUT` and `POST`) and the SCOPE the request addresses — a bucket, a
//!   bucket subresource addressed by a configuration `id`, and an object;
//! * how the operation is SERVED — plain, behind this service's front door (the metrics and
//!   S3 Inventory writes) and behind the readable-document wrapper (the S3 Inventory write).
//!
//! [`the_required_document_catalog_matches_the_model`] re-derives the catalog from the model
//! itself, so a model update that requires another document cannot quietly leave it out.
//!
//! Two contrast halves pin the rule's edges: a document the request DID carry but that does
//! not validate still earns `MalformedXML`, and an operation whose document the model leaves
//! OPTIONAL is not refused by this rule at all.

use service_com_amazonaws_s3_amazons3::service::REQUIRED_XML_DOCUMENT_PAYLOAD_MEMBERS;

use super::bucket_lifecycle::{create_bucket, router, send, Answer};
use super::object_and_tagging_roundtrip::{put_object, text};
use super::refusal_codes::{lower_camel, model};

/// The code Amazon S3 answers a request that carried no body where the operation requires a
/// document with (Amazon S3 API Reference, *Error responses*).
const MISSING_REQUEST_BODY_CODE: &str = "MissingRequestBodyError";

/// The `<Message>` that refusal carries.
const MISSING_REQUEST_BODY_MESSAGE: &str = "Request Body is empty";

/// The code a document that DID arrive and did not validate against the published schema
/// earns instead — the refusal the rule has to be told apart from.
const MALFORMED_XML_CODE: &str = "MalformedXML";

/// The status both refusals are rendered with.
const REFUSAL_STATUS: u16 = 400;

/// The XML namespace botocore puts on an S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The object every object-scoped row addresses.
const OBJECT_KEY: &str = "reports/summary.csv";

/// The id every id-addressed row names its configuration by.
const CONFIGURATION_ID: &str = "summary";

/// One row: the request to send with no body, and the input member whose document it left
/// out.
struct Row {
    case: &'static str,
    method: &'static str,
    /// The path and query, with `{bucket}` standing for the bucket the suite created.
    target: String,
    /// The `@required` structured `@httpPayload` member of that operation's input, as a
    /// validation path spells it — which is what ties the table to the catalog.
    member: &'static str,
}

#[test]
fn the_required_document_catalog_matches_the_model() {
    // Every `@httpPayload` member of an operation INPUT whose target is a structured shape
    // and which the model marks `@required`, read from the model rather than from a second
    // hand-written list.
    let model = model();
    let shapes = model["shapes"]
        .as_object()
        .expect("the model must carry a shapes object");

    let input_shapes: std::collections::BTreeSet<&str> = shapes
        .iter()
        .filter(|(_, shape)| shape["type"] == "operation")
        .filter_map(|(_, shape)| shape["input"]["target"].as_str())
        .collect();

    let mut derived: Vec<String> = Vec::new();
    for input in input_shapes {
        let Some(members) = shapes.get(input).and_then(|shape| shape["members"].as_object()) else {
            continue;
        };
        for (name, member) in members {
            if member["traits"].get("smithy.api#httpPayload").is_none()
                || member["traits"].get("smithy.api#required").is_none()
            {
                continue;
            }
            let Some(target) = member["target"].as_str() else {
                continue;
            };
            let kind = shapes
                .get(target)
                .and_then(|shape| shape["type"].as_str())
                .unwrap_or_default();
            if matches!(kind, "structure" | "union" | "list" | "map" | "document") {
                derived.push(lower_camel(name));
            }
        }
    }
    derived.sort();
    derived.dedup();

    let mut declared: Vec<String> = REQUIRED_XML_DOCUMENT_PAYLOAD_MEMBERS
        .iter()
        .map(|member| (*member).to_string())
        .collect();
    declared.sort();
    assert_eq!(
        declared, derived,
        "REQUIRED_XML_DOCUMENT_PAYLOAD_MEMBERS must name exactly the required structured \
         @httpPayload input members the model binds; declared {declared:?}, the model binds \
         {derived:?}"
    );
}

/// The rows, one per operation whose document the model requires.
fn rows(bucket: &str) -> Vec<Row> {
    let subresource = |subresource: &str| format!("/{bucket}?{subresource}");
    let addressed = |subresource: &str| format!("/{bucket}?{subresource}&id={CONFIGURATION_ID}");
    vec![
        Row {
            case: "PutBucketOwnershipControls",
            method: "PUT",
            target: subresource("ownershipControls"),
            member: "ownershipControls",
        },
        Row {
            case: "PutBucketVersioning",
            method: "PUT",
            target: subresource("versioning"),
            member: "versioningConfiguration",
        },
        Row {
            case: "PutBucketTagging",
            method: "PUT",
            target: subresource("tagging"),
            member: "tagging",
        },
        Row {
            case: "PutBucketCors",
            method: "PUT",
            target: subresource("cors"),
            member: "cORSConfiguration",
        },
        Row {
            case: "PutBucketEncryption",
            method: "PUT",
            target: subresource("encryption"),
            member: "serverSideEncryptionConfiguration",
        },
        Row {
            case: "PutBucketReplication",
            method: "PUT",
            target: subresource("replication"),
            member: "replicationConfiguration",
        },
        Row {
            case: "PutBucketWebsite",
            method: "PUT",
            target: subresource("website"),
            member: "websiteConfiguration",
        },
        // A container every one of whose members is OPTIONAL: read as "an empty container"
        // this request would be served, and it would turn the bucket's logging off.
        Row {
            case: "PutBucketLogging",
            method: "PUT",
            target: subresource("logging"),
            member: "bucketLoggingStatus",
        },
        Row {
            case: "PutBucketRequestPayment",
            method: "PUT",
            target: subresource("requestPayment"),
            member: "requestPaymentConfiguration",
        },
        Row {
            case: "PutBucketAccelerateConfiguration",
            method: "PUT",
            target: subresource("accelerate"),
            member: "accelerateConfiguration",
        },
        // The second such container, whose empty reading withdraws every notification the
        // bucket publishes.
        Row {
            case: "PutBucketNotificationConfiguration",
            method: "PUT",
            target: subresource("notification"),
            member: "notificationConfiguration",
        },
        Row {
            case: "PutPublicAccessBlock",
            method: "PUT",
            target: subresource("publicAccessBlock"),
            member: "publicAccessBlockConfiguration",
        },
        Row {
            case: "PutBucketAbac",
            method: "PUT",
            target: subresource("abac"),
            member: "abacStatus",
        },
        // A POST, and a document that is a LIST of keys rather than a configuration.
        Row {
            case: "DeleteObjects",
            method: "POST",
            target: subresource("delete"),
            member: "delete",
        },
        // The four subresource families addressed by a configuration `id`, two of them
        // served behind this service's front door.
        Row {
            case: "PutBucketAnalyticsConfiguration",
            method: "PUT",
            target: addressed("analytics"),
            member: "analyticsConfiguration",
        },
        Row {
            case: "PutBucketIntelligentTieringConfiguration",
            method: "PUT",
            target: addressed("intelligent-tiering"),
            member: "intelligentTieringConfiguration",
        },
        Row {
            case: "PutBucketMetricsConfiguration",
            method: "PUT",
            target: addressed("metrics"),
            member: "metricsConfiguration",
        },
        Row {
            case: "PutBucketInventoryConfiguration",
            method: "PUT",
            target: addressed("inventory"),
            member: "inventoryConfiguration",
        },
        // The metadata-configuration family: two POSTs that CREATE a configuration and three
        // PUTs that update one table of it.
        Row {
            case: "CreateBucketMetadataConfiguration",
            method: "POST",
            target: subresource("metadataConfiguration"),
            member: "metadataConfiguration",
        },
        Row {
            case: "CreateBucketMetadataTableConfiguration",
            method: "POST",
            target: subresource("metadataTable"),
            member: "metadataTableConfiguration",
        },
        Row {
            case: "UpdateBucketMetadataAnnotationTableConfiguration",
            method: "PUT",
            target: subresource("metadataAnnotationTable"),
            member: "annotationTableConfiguration",
        },
        Row {
            case: "UpdateBucketMetadataInventoryTableConfiguration",
            method: "PUT",
            target: subresource("metadataInventoryTable"),
            member: "inventoryTableConfiguration",
        },
        Row {
            case: "UpdateBucketMetadataJournalTableConfiguration",
            method: "PUT",
            target: subresource("metadataJournalTable"),
            member: "journalTableConfiguration",
        },
        // And the two object-scoped writes, whose address carries a key.
        Row {
            case: "PutObjectTagging",
            method: "PUT",
            target: format!("/{bucket}/{OBJECT_KEY}?tagging"),
            member: "tagging",
        },
        Row {
            case: "UpdateObjectEncryption",
            method: "PUT",
            target: format!("/{bucket}/{OBJECT_KEY}?encryption"),
            member: "objectEncryption",
        },
    ]
}

/// Assert that `answer` is the refusal S3 answers a request that sent no document with,
/// named by this service's own request-correlation ids.
fn assert_refused_for_the_absent_body(answer: &Answer, case: &str) {
    assert_eq!(
        answer.status, REFUSAL_STATUS,
        "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {} with body {:?}",
        answer.status,
        text(answer)
    );
    assert_eq!(
        answer.error_code(),
        MISSING_REQUEST_BODY_CODE,
        "case {case:?}: expected the code {MISSING_REQUEST_BODY_CODE:?}, got {:?} in {:?}",
        answer.error_code(),
        text(answer)
    );
    assert_eq!(
        answer.error_message(),
        MISSING_REQUEST_BODY_MESSAGE,
        "case {case:?}: expected the message {MISSING_REQUEST_BODY_MESSAGE:?}, got {:?}",
        answer.error_message()
    );
    // The refusal goes out in this service's own envelope: its `<RequestId>` is the id the
    // response headers name the request by.
    let header = answer.header("x-amz-request-id");
    assert_eq!(
        answer.element("RequestId"),
        header,
        "case {case:?}: the envelope's RequestId must be the header's {header:?}, got {:?} \
         in {:?}",
        answer.element("RequestId"),
        text(answer)
    );
}

#[tokio::test]
async fn an_operation_requiring_a_document_refuses_a_request_that_carries_no_body() {
    let router = router();
    let bucket = "missing-request-body";
    let created = create_bucket(&router, bucket, None, None).await;
    assert_eq!(
        created.status, 200,
        "the bucket must be created first, got {} with body {:?}",
        created.status,
        text(&created)
    );
    let written = put_object(&router, bucket, OBJECT_KEY, b"one,two\n", &[]).await;
    assert_eq!(
        written.status, 200,
        "the object every object-scoped row addresses must be written first, got {} with \
         body {:?}",
        written.status,
        text(&written)
    );

    for row in rows(bucket) {
        let answer = send(&router, row.method, &row.target, &[], "").await;
        assert_refused_for_the_absent_body(&answer, row.case);
    }

    // Nothing the refused calls addressed changed: the object they would have retagged,
    // re-encrypted or deleted is still there, and the bucket still reports the tag set and
    // the versioning state it had.
    let object = send(&router, "HEAD", &format!("/{bucket}/{OBJECT_KEY}"), &[], "").await;
    assert_eq!(
        object.status, 200,
        "case \"the refused batch delete\": the object must still be there, got {}",
        object.status
    );
    let versioning = send(&router, "GET", &format!("/{bucket}?versioning"), &[], "").await;
    assert_eq!(
        versioning.element("Status"),
        None,
        "case \"the refused versioning write\": the bucket must still report no versioning \
         state, got {:?}",
        text(&versioning)
    );
}

#[test]
fn every_required_document_is_covered_by_a_request() {
    let covered: std::collections::BTreeSet<&str> =
        rows("coverage").iter().map(|row| row.member).collect();
    let missing: Vec<&&str> = REQUIRED_XML_DOCUMENT_PAYLOAD_MEMBERS
        .iter()
        .filter(|member| !covered.contains(**member))
        .collect();
    assert!(
        missing.is_empty(),
        "every member of REQUIRED_XML_DOCUMENT_PAYLOAD_MEMBERS must be exercised by a row; \
         {missing:?} are not"
    );
}

#[tokio::test]
async fn a_document_that_did_arrive_is_still_refused_against_the_published_schema() {
    let router = router();
    let bucket = "missing-request-body-present";
    create_bucket(&router, bucket, None, None).await;

    // (case, the subresource, the document the request DID carry). Each is a body the
    // published schema cannot take, so each keeps the schema refusal: the rule above is
    // about a body that never arrived, not about one that is wrong.
    let rows: [(&str, &str, String); 4] = [
        (
            "an ownership-controls container naming no rule",
            "ownershipControls",
            format!("<OwnershipControls xmlns=\"{XMLNS}\"></OwnershipControls>"),
        ),
        (
            "a document whose root element is not the one the operation binds",
            "ownershipControls",
            format!("<NotOwnershipControls xmlns=\"{XMLNS}\"/>"),
        ),
        (
            "a body that is not XML at all",
            "cors",
            "not a document".to_string(),
        ),
        (
            "a body carrying nothing but whitespace",
            "cors",
            "   \n".to_string(),
        ),
    ];

    for (case, subresource, body) in rows {
        let answer = send(
            &router,
            "PUT",
            &format!("/{bucket}?{subresource}"),
            &[],
            &body,
        )
        .await;
        assert_eq!(
            answer.status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        assert_eq!(
            answer.error_code(),
            MALFORMED_XML_CODE,
            "case {case:?}: a document that did arrive must keep the schema refusal \
             {MALFORMED_XML_CODE:?}, got {:?} in {:?}",
            answer.error_code(),
            text(&answer)
        );
    }
}

#[tokio::test]
async fn an_operation_whose_document_is_optional_is_not_refused_for_the_body() {
    let router = router();
    // `CreateBucket`'s `CreateBucketConfiguration` is an `@httpPayload` the model does NOT
    // mark required (`smithy_query(query="operation",operation="CreateBucket")`), so a
    // create that carries no body is the ordinary create of a bucket in the Region its
    // endpoint serves — the rule above must not reach it.
    let created = create_bucket(&router, "missing-request-body-optional", None, None).await;
    assert_eq!(
        created.status, 200,
        "case \"CreateBucket with no body\": expected the create to be served, got {} with \
         body {:?}",
        created.status,
        text(&created)
    );
    let listed = send(&router, "GET", "/", &[], "").await;
    assert!(
        listed
            .elements("Name")
            .iter()
            .any(|name| name == "missing-request-body-optional"),
        "case \"CreateBucket with no body\": the created bucket must be listed, got {:?}",
        text(&listed)
    );
}
