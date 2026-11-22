// SPDX-License-Identifier: CC0-1.0

//! An Amazon S3 BUCKET-level call read back out of CloudTrail event history, through the
//! operation a client reads it with — and the object calls of the same window, which must be
//! in none of its pages.
//!
//! RULE: every management (control-plane) API call an account's services serve is recorded in
//! that Region's event history and reported by CloudTrail's `LookupEvents`, whether or not a
//! trail exists — the call's own `eventName`, the serving service's `eventSource`, the
//! `eventCategory` `Management`, `readOnly` from the operation's own semantics, the members
//! the request carried as `requestParameters`, and for a call the service REFUSED the fault
//! shape's name as `errorCode` beside the sentence the caller was answered with. For Amazon
//! S3 the management calls are the BUCKET-level ones (its lifecycle, its configuration
//! surfaces, its listings and the account's bucket listings); a call on the CONTENTS of one
//! object is a data event, which is delivered to a trail's bucket and never to event history.
//!
//! Only a cross-service suite can pin this: the calls are served by Amazon S3 (`restXml`),
//! the records are read back through `cloudtrail:LookupEvents` (`awsJson1_1`), and the two
//! meet in the one shared management-event log of the engine `core/router/src/lib.rs` mounts
//! them both on. The S3-side half of the rule — which operations are recorded as which class
//! — is `services/service-com-amazonaws-s3-amazons3/src/service/management_events.rs` and its
//! own tests.
//!
//! The rows span what the rule generalises over rather than the one create that motivated it:
//! a create and a delete of the bucket itself, a configuration write and the read of what it
//! wrote, a read of the bucket's own placement, a listing of the account's buckets, a listing
//! of what one bucket holds, and two REFUSED calls of different fault shapes — with the
//! object write and read of the same window as the control. Every page of the lookup is read,
//! paging `NextToken` to exhaustion, because a record a client has to find is a record it
//! finds however the service pages.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, which is the Region the records are filed in
/// and the one the lookup is made in.
///
/// A Region other than the partition's leader on purpose: a duplicate `CreateBucket` by the
/// bucket's own owner is answered 200 in `us-east-1` and refused everywhere else, and one of
/// the rows below is that refusal.
const REGION: &str = "us-west-2";

/// The account every emulated resource and every record belongs to.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The access key id every call below is presented as signed by.
const ACCESS_KEY_ID: &str = "AKIDTEST";

/// The `eventSource` of every record an Amazon S3 call makes: the model's own
/// `awsService.cloudTrailEventSource`.
const S3_EVENT_SOURCE: &str = "s3.amazonaws.com";

/// The `eventCategory` a lookup serves.
const MANAGEMENT_CATEGORY: &str = "Management";

/// The bucket the calls below are made on, and the one a refused listing addresses instead.
const BUCKET: &str = "ct-s3-management-subject";
const ABSENT_BUCKET: &str = "ct-s3-management-absent";

/// The object the control rows write and read: a call on its contents is a DATA event.
const OBJECT_KEY: &str = "contents/object.txt";

/// The member an S3 record names the addressed bucket by, which is Amazon S3's own spelling
/// rather than the modelled member's.
const RECORDED_BUCKET_MEMBER: &str = "/requestParameters/bucketName";

/// How many pages of one lookup are read at most: a bound on this test, not on the API — the
/// log holds a handful of records here, and a service that paged without end would fail here
/// rather than loop.
const MAX_PAGES: usize = 50;

/// What a record carries in place of a value the serving API declares `@sensitive`:
/// CloudTrail's own fixed literal, which the tree states once as
/// `service_common::management_events::HIDDEN_SENSITIVE_VALUE` and is repeated here because
/// this test crate takes no service crate but the two it drives.
const HIDDEN_SENSITIVE_VALUE: &str = "HIDDEN_DUE_TO_SECURITY_REASONS";

/// The KMS key a bucket's default encryption is configured with below: a key ARN of the
/// documented form, whose value the record must NOT carry
/// (`smithy_query(query="shape",shape="ServerSideEncryptionByDefault")` marks
/// `KMSMasterKeyID` sensitive).
const KMS_KEY_ARN: &str =
    "arn:aws:kms:us-west-2:000000000000:key/1234abcd-12ab-34cd-56ef-1234567890ab";

/// One answer as its caller reads it.
struct Answered {
    status: u16,
    text: String,
    /// The id Amazon S3 named the request with, which is what the record of the call carries
    /// as its `requestID`.
    request_id: Option<String>,
}

/// Amazon S3 and CloudTrail over one shared engine, exactly as `core/router/src/lib.rs`
/// mounts them. No trail is created: event history holds an account's management calls
/// whether or not a trail exists, which is part of the rule this suite pins.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so the signatures are not verified; the credential
    // scope is still carried, because it is what names the Region and the caller.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// Dispatch one request and read back its status, body and request id.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    signing_name: &str,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
    target: Option<&str>,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type);
    if let Some(target) = target {
        builder = builder.header("x-amz-target", target);
    }
    let request = builder
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let request_id = response
        .headers()
        .get("x-amz-request-id")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let text = String::from_utf8_lossy(&response.into_body()).to_string();
    Answered {
        status,
        text,
        request_id,
    }
}

/// One Amazon S3 call on the path-style address (`restXml`), answered as it stands.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answered {
    dispatch(
        router,
        method,
        "s3",
        path,
        "application/xml",
        body.as_bytes().to_vec(),
        None,
    )
    .await
}

/// One CloudTrail call (`awsJson1_1`, `CloudTrail_20131101.<Operation>`) that must be served.
async fn cloudtrail(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let target = format!("CloudTrail_20131101.{operation}");
    let answered = dispatch(
        router,
        "POST",
        "cloudtrail",
        "/",
        "application/x-amz-json-1.1",
        body.to_string().into_bytes(),
        Some(&target),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from CloudTrail {operation}, got {} with {}",
        answered.status, answered.text
    );
    serde_json::from_str(&answered.text).unwrap_or(Value::Null)
}

/// Every event of `attributes`, paging `NextToken` to exhaustion, as the parsed
/// `CloudTrailEvent` document of each one beside the `EventName` the event itself reports.
async fn looked_up(
    router: &MultiServiceRouter,
    case: &str,
    attributes: &Value,
) -> Vec<(String, Value)> {
    let mut found: Vec<(String, Value)> = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut request = json!({ "LookupAttributes": [attributes], "MaxResults": 2 });
        if let Some(token) = &next {
            request["NextToken"] = json!(token);
        }
        let answered = cloudtrail(router, case, "LookupEvents", &request).await;
        for event in answered["Events"].as_array().unwrap_or(&Vec::new()) {
            let document = event["CloudTrailEvent"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: every event carries its CloudTrailEvent, got {event}")
            });
            let parsed: Value = serde_json::from_str(document).unwrap_or_else(|error| {
                panic!("case {case:?}: a CloudTrailEvent is JSON ({error}): {document}")
            });
            found.push((
                event["EventName"].as_str().unwrap_or_default().to_string(),
                parsed,
            ));
        }
        next = answered["NextToken"].as_str().map(ToString::to_string);
        if next.is_none() {
            return found;
        }
    }
    panic!(
        "case {case:?}: the lookup must page to exhaustion, got {} events",
        found.len()
    );
}

/// The `eventName`s of what a lookup answered.
fn names(found: &[(String, Value)]) -> Vec<&str> {
    found.iter().map(|(name, _)| name.as_str()).collect()
}

/// The document a create carries outside the partition's leader Region: a bucket created
/// anywhere else states the Region in the request body.
fn create_bucket_body() -> String {
    format!(
        "<CreateBucketConfiguration><LocationConstraint>{REGION}</LocationConstraint>\
         </CreateBucketConfiguration>"
    )
}

/// One call this suite makes and reads back, and what the record of it must say.
struct Row {
    case: &'static str,
    method: &'static str,
    path: String,
    body: String,
    /// Whether Amazon S3 must SERVE the call (a 2xx) or refuse it (a 4xx).
    served: bool,
    /// The `eventName` CloudTrail publishes the call under, which for this service is the
    /// operation's own modelled name.
    event_name: &'static str,
    /// The operation's own semantics, as the record's `readOnly`.
    read_only: bool,
    /// The fault SHAPE's name a refused call records, and `None` for a served one.
    error_code: Option<&'static str>,
    /// The bucket the call addressed, which its record must name — `None` for the one call
    /// that addresses no bucket at all.
    bucket: Option<&'static str>,
}

/// Every call this suite makes, in the order it makes them.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the bucket's own create",
            method: "PUT",
            path: format!("/{BUCKET}"),
            body: create_bucket_body(),
            served: true,
            event_name: "CreateBucket",
            read_only: false,
            error_code: None,
            bucket: Some(BUCKET),
        },
        Row {
            case: "a duplicate create of the same bucket, refused",
            method: "PUT",
            path: format!("/{BUCKET}"),
            body: create_bucket_body(),
            served: false,
            event_name: "CreateBucket",
            read_only: false,
            // The modelled fault of a create the caller already owns
            // (`smithy_query(query="operation",operation="CreateBucket")` lists
            // `BucketAlreadyOwnedByYou`, httpError 409).
            error_code: Some("BucketAlreadyOwnedByYou"),
            bucket: Some(BUCKET),
        },
        Row {
            case: "a configuration write",
            method: "PUT",
            path: format!("/{BUCKET}?tagging"),
            body: "<Tagging><TagSet><Tag><Key>owner</Key><Value>audit</Value></Tag></TagSet>\
                   </Tagging>"
                .to_string(),
            served: true,
            event_name: "PutBucketTagging",
            read_only: false,
            error_code: None,
            bucket: Some(BUCKET),
        },
        Row {
            case: "the read of what that write stored",
            method: "GET",
            path: format!("/{BUCKET}?tagging"),
            body: String::new(),
            served: true,
            event_name: "GetBucketTagging",
            read_only: true,
            error_code: None,
            bucket: Some(BUCKET),
        },
        Row {
            case: "a read of the bucket's own placement",
            method: "GET",
            path: format!("/{BUCKET}?location"),
            body: String::new(),
            served: true,
            event_name: "GetBucketLocation",
            read_only: true,
            error_code: None,
            bucket: Some(BUCKET),
        },
        Row {
            case: "a listing of the account's buckets, which addresses none",
            method: "GET",
            path: "/".to_string(),
            body: String::new(),
            served: true,
            event_name: "ListBuckets",
            read_only: true,
            error_code: None,
            bucket: None,
        },
        Row {
            case: "a listing of what one bucket holds",
            method: "GET",
            path: format!("/{BUCKET}?list-type=2"),
            body: String::new(),
            served: true,
            event_name: "ListObjectsV2",
            read_only: true,
            error_code: None,
            bucket: Some(BUCKET),
        },
        Row {
            case: "the same listing of a bucket that is not there, refused",
            method: "GET",
            path: format!("/{ABSENT_BUCKET}?list-type=2"),
            body: String::new(),
            served: false,
            event_name: "ListObjectsV2",
            read_only: true,
            // `smithy_query(query="operation",operation="ListObjectsV2")` lists `NoSuchBucket`,
            // httpError 404.
            error_code: Some("NoSuchBucket"),
            bucket: Some(ABSENT_BUCKET),
        },
    ]
}

/// RULE, the management half: every bucket-level call Amazon S3 serves — and every one it
/// refuses — is in the Region's event history, and `LookupEvents` reports it with the fields
/// above.
#[tokio::test]
async fn an_s3_bucket_call_is_served_back_by_lookup_events() {
    let router = router();
    let rows = rows();

    // Every call, with the id its caller was answered under kept beside it: a record a client
    // finds by its own request id belongs to that call and to no other.
    let mut answered_ids: Vec<(usize, String)> = Vec::new();
    for (at, row) in rows.iter().enumerate() {
        let answered = s3(&router, row.method, &row.path, &row.body).await;
        if row.served {
            assert!(
                (200..300).contains(&answered.status),
                "case {:?}: the call must be served, got {} with {}",
                row.case,
                answered.status,
                answered.text
            );
        } else {
            assert!(
                (400..500).contains(&answered.status),
                "case {:?}: the call must be refused, got {} with {}",
                row.case,
                answered.status,
                answered.text
            );
        }
        if let Some(id) = answered.request_id {
            answered_ids.push((at, id));
        }
    }

    // The object calls of the same window, which are DATA events: delivered to a trail's
    // bucket and never to event history. Made LAST, so a record of one that did reach the log
    // would already be there when the pages below are read.
    let written = s3(&router, "PUT", &format!("/{BUCKET}/{OBJECT_KEY}"), "contents").await;
    assert!(
        (200..300).contains(&written.status),
        "case the object write: it must be served, got {} with {}",
        written.status,
        written.text
    );
    let read = s3(&router, "GET", &format!("/{BUCKET}/{OBJECT_KEY}"), "").await;
    assert!(
        (200..300).contains(&read.status),
        "case the object read: it must be served, got {} with {}",
        read.status,
        read.text
    );

    // ONE lookup over the whole event source, as an audit client makes it.
    let by_source = looked_up(
        &router,
        "a lookup by event source",
        &json!({ "AttributeKey": "EventSource", "AttributeValue": S3_EVENT_SOURCE }),
    )
    .await;
    let found_names = names(&by_source);

    for row in &rows {
        let matching: Vec<&Value> = by_source
            .iter()
            .filter(|(name, document)| {
                name == row.event_name
                    && document.get("errorCode").and_then(Value::as_str) == row.error_code
            })
            .map(|(_, document)| document)
            .collect();
        assert!(
            !matching.is_empty(),
            "case {:?}: expected a record of {} with errorCode {:?} in event history, got \
             {found_names:?}",
            row.case,
            row.event_name,
            row.error_code
        );
        let document = matching[0];
        assert_eq!(
            document.get("eventSource"),
            Some(&json!(S3_EVENT_SOURCE)),
            "case {:?}: the record names the service's event source, got {document}",
            row.case
        );
        assert_eq!(
            document.get("eventCategory"),
            Some(&json!(MANAGEMENT_CATEGORY)),
            "case {:?}: a lookup serves management events, got {document}",
            row.case
        );
        assert_eq!(
            document.get("readOnly"),
            Some(&json!(row.read_only)),
            "case {:?}: expected readOnly {}, got {document}",
            row.case,
            row.read_only
        );
        assert_eq!(
            document.get("awsRegion"),
            Some(&json!(REGION)),
            "case {:?}: the record is filed in the Region the call was made in, got {document}",
            row.case
        );
        assert_eq!(
            document.get("recipientAccountId"),
            Some(&json!(ACCOUNT)),
            "case {:?}: the record names the caller's account, got {document}",
            row.case
        );
        assert!(
            document
                .get("eventID")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty()),
            "case {:?}: every record carries an eventID, got {document}",
            row.case
        );
        assert_eq!(
            document.pointer("/userIdentity/accessKeyId"),
            Some(&json!(ACCESS_KEY_ID)),
            "case {:?}: the record names the credential the call was signed with, got \
             {document}",
            row.case
        );
        if let Some(bucket) = row.bucket {
            assert_eq!(
                document.pointer(RECORDED_BUCKET_MEMBER),
                Some(&json!(bucket)),
                "case {:?}: expected {bucket:?} at {RECORDED_BUCKET_MEMBER}, got {document}",
                row.case
            );
        } else {
            assert_eq!(
                document.pointer(RECORDED_BUCKET_MEMBER),
                None,
                "case {:?}: a call that addresses no bucket names none, got {document}",
                row.case
            );
        }
        match row.error_code {
            // A refused call records the fault shape's name and the sentence the caller was
            // answered with, which is what makes a refusal auditable.
            Some(expected) => {
                assert_eq!(
                    document.get("errorCode"),
                    Some(&json!(expected)),
                    "case {:?}: expected errorCode {expected:?}, got {document}",
                    row.case
                );
                assert!(
                    document
                        .get("errorMessage")
                        .and_then(Value::as_str)
                        .is_some_and(|message| !message.is_empty()),
                    "case {:?}: a refused call records the message its caller was answered \
                     with, got {document}",
                    row.case
                );
            }
            None => {
                assert_eq!(
                    document.get("errorCode"),
                    None,
                    "case {:?}: a served call carries no errorCode, got {document}",
                    row.case
                );
            }
        }
    }

    // Every record is its own: no two of them share an `eventID`.
    let ids: BTreeSet<&str> = by_source
        .iter()
        .filter_map(|(_, document)| document.get("eventID").and_then(Value::as_str))
        .collect();
    assert_eq!(
        ids.len(),
        by_source.len(),
        "case every record: expected one eventID per record, got {ids:?} over {} records",
        by_source.len()
    );

    // The record of a call names the request the CALLER was answered for, so a client holding
    // a response finds the record of that very call.
    let request_ids: BTreeSet<&str> = by_source
        .iter()
        .filter_map(|(_, document)| document.get("requestID").and_then(Value::as_str))
        .collect();
    for (at, answered) in &answered_ids {
        assert!(
            request_ids.contains(answered.as_str()),
            "case {:?}: expected a record naming the request the caller was answered under \
             ({answered}), got {request_ids:?}",
            rows[*at].case
        );
    }

    // The other half of the contract: no object call of the window is in event history, under
    // the lookup over the whole source or under a lookup naming the object operation.
    for object_call in ["PutObject", "GetObject"] {
        assert!(
            !found_names.contains(&object_call),
            "case {object_call:?}: a call on an object's contents is a data event and no event \
             history holds it, got {found_names:?}"
        );
        let by_name = looked_up(
            &router,
            "a lookup naming the object call",
            &json!({ "AttributeKey": "EventName", "AttributeValue": object_call }),
        )
        .await;
        assert!(
            by_name.is_empty(),
            "case {object_call:?}: expected no event, got {:?}",
            names(&by_name)
        );
    }
}

/// RULE, the lookup half: the secondary attributes a client filters event history by answer
/// the same records — a lookup by `EventName` answers every record of that operation whatever
/// it was answered with, and `ReadOnly` partitions the account's calls by what each one did.
#[tokio::test]
async fn the_lookup_attributes_partition_the_recorded_s3_calls() {
    let router = router();
    let rows = rows();
    for row in &rows {
        let answered = s3(&router, row.method, &row.path, &row.body).await;
        assert_eq!(
            (200..300).contains(&answered.status),
            row.served,
            "case {:?}: expected served={}, got {} with {}",
            row.case,
            row.served,
            answered.status,
            answered.text
        );
    }

    // A lookup by the operation's published name answers both its records: the create that was
    // served and the duplicate that was refused.
    let by_name = looked_up(
        &router,
        "a lookup by event name",
        &json!({ "AttributeKey": "EventName", "AttributeValue": "CreateBucket" }),
    )
    .await;
    assert_eq!(
        by_name.len(),
        2,
        "case a lookup by event name: expected the served create and the refused duplicate, \
         got {:?}",
        names(&by_name)
    );
    let refused: Vec<&str> = by_name
        .iter()
        .filter_map(|(_, document)| document.get("errorCode").and_then(Value::as_str))
        .collect();
    assert_eq!(
        refused,
        vec!["BucketAlreadyOwnedByYou"],
        "case a lookup by event name: exactly one of the two records is the refusal, got \
         {by_name:?}"
    );

    // And `ReadOnly` partitions them: every reporting call on one side, every mutating call on
    // the other, with no record on both.
    for (case, value) in [("the reads", "true"), ("the writes", "false")] {
        let found = looked_up(
            &router,
            case,
            &json!({ "AttributeKey": "ReadOnly", "AttributeValue": value }),
        )
        .await;
        let read_only = value == "true";
        let expected: BTreeSet<&str> = rows
            .iter()
            .filter(|row| row.read_only == read_only)
            .map(|row| row.event_name)
            .collect();
        let answered: BTreeSet<&str> = found
            .iter()
            .filter(|(_, document)| {
                document.get("eventSource") == Some(&json!(S3_EVENT_SOURCE))
            })
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(
            answered, expected,
            "case {case:?}: expected exactly the {value} calls of this source, got {answered:?}"
        );
        for (name, document) in &found {
            assert_eq!(
                document.get("readOnly"),
                Some(&json!(read_only)),
                "case {case:?}: every answered record carries readOnly {read_only} ({name}), \
                 got {document}"
            );
        }
    }
}

/// RULE, the withholding half: a record carries CloudTrail's redaction literal in place of
/// the value of a member the serving API declares `@sensitive`, so the record still shows
/// that the call carried the member while the value itself reaches event history nowhere.
///
/// The rows differ along WHERE the sensitive value sits in the request — inside the bucket's
/// own encryption document, and inside an inventory configuration's destination document —
/// because the walk that withholds it is schema-driven rather than per operation.
#[tokio::test]
async fn a_recorded_bucket_call_withholds_the_kms_key_it_carried() {
    let router = router();
    let created = s3(&router, "PUT", &format!("/{BUCKET}"), &create_bucket_body()).await;
    assert!(
        (200..300).contains(&created.status),
        "setup: the bucket must be created, got {} with {}",
        created.status,
        created.text
    );

    // (case, the operation, the path, the document carrying the key)
    let rows: Vec<(&str, &str, String, String)> = vec![
        (
            "the bucket's default encryption",
            "PutBucketEncryption",
            format!("/{BUCKET}?encryption"),
            format!(
                "<ServerSideEncryptionConfiguration><Rule>\
                 <ApplyServerSideEncryptionByDefault><SSEAlgorithm>aws:kms</SSEAlgorithm>\
                 <KMSMasterKeyID>{KMS_KEY_ARN}</KMSMasterKeyID>\
                 </ApplyServerSideEncryptionByDefault></Rule>\
                 </ServerSideEncryptionConfiguration>"
            ),
        ),
        (
            "an inventory report's own encryption",
            "PutBucketInventoryConfiguration",
            format!("/{BUCKET}?inventory&id=audit"),
            format!(
                "<InventoryConfiguration><Id>audit</Id><IsEnabled>true</IsEnabled>\
                 <IncludedObjectVersions>All</IncludedObjectVersions>\
                 <Schedule><Frequency>Daily</Frequency></Schedule>\
                 <Destination><S3BucketDestination><Bucket>arn:aws:s3:::{BUCKET}</Bucket>\
                 <Format>CSV</Format><Encryption><SSE-KMS><KeyId>{KMS_KEY_ARN}</KeyId>\
                 </SSE-KMS></Encryption></S3BucketDestination></Destination>\
                 </InventoryConfiguration>"
            ),
        ),
    ];

    for (case, operation, path, document) in &rows {
        // Whether the configuration is ACCEPTED is another suite's subject: a refused call is
        // recorded too, and either way the record must not carry the key.
        let answered = s3(&router, "PUT", path, document).await;
        assert!(
            answered.status < 500,
            "case {case:?}: the call must be answered by this service, got {} with {}",
            answered.status,
            answered.text
        );
        let found = looked_up(
            &router,
            case,
            &json!({ "AttributeKey": "EventName", "AttributeValue": operation }),
        )
        .await;
        assert_eq!(
            found.len(),
            1,
            "case {case:?}: expected the one record of {operation}, got {:?}",
            names(&found)
        );
        let document = found[0].1.to_string();
        assert!(
            !document.contains(KMS_KEY_ARN),
            "case {case:?}: the key the call carried must reach no record, got {document}"
        );
        assert!(
            document.contains(HIDDEN_SENSITIVE_VALUE),
            "case {case:?}: the record must show the member it withheld, got {document}"
        );
    }
}
