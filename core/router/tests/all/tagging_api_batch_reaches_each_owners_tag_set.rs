// SPDX-License-Identifier: CC0-1.0

//! One central-tagging batch spanning TWO owning services reaches each resource's OWN tag
//! set, and an ARN its owner holds nothing under is reported per resource.
//!
//! The rule under test: `TagResources` "appl[ies] one or more tags to the specified
//! resources" and `UntagResources` removes the keys it names from them, and the resources
//! are not this API's — each belongs to the service whose namespace its ARN names — so the
//! set this API writes IS the set that service holds, and after a batch every resource's
//! OWN tag reader reports exactly the keys the writes left. A resource that was tagged
//! successfully is absent from `FailedResourcesMap`; an ARN whose owning service holds no
//! such resource is an entry in `FailedResourcesMap` under that ARN alone, carrying "any
//! valid error code returned by the … service that hosts the resource that the ARN key
//! represents" (`FailureInfo`), with HTTP 200 overall, while every other ARN of the same
//! batch is written.
//!
//! Two owners are driven here at once, each with its own tagging surface and its own
//! not-found identity:
//!
//! * Amazon S3, where a general purpose bucket's tag set is what `GetBucketTagging`
//!   reports — and, when nothing is left on it, the `NoSuchTagSet` that operation
//!   documents as its special error — and whose refusal for a name it holds no bucket
//!   under is `NoSuchBucket`
//!   (`smithy_query(query="shape",shape="NoSuchBucket")` on the S3 model: HTTP 404);
//! * Amazon DynamoDB, where a table's tag set is what `ListTagsOfResource` reports — one
//!   propagation span later, because "the application or removal of tags using TagResource
//!   and UntagResource APIs is eventually consistent" — and whose refusal for an ARN it
//!   holds no table for is `ResourceNotFoundException`
//!   (`smithy_query(query="operation",operation="TagResource")` on the DynamoDB model:
//!   HTTP 400).
//!
//! Three crates have to agree for the rule to hold — the two owners store the tag sets,
//! the tagging API writes them — so the suite lives here, on a router carrying all three
//! over ONE engine, which is how the emulator runs.
//!
//! The first test's rows span what the rule generalises over: how many resources a batch
//! addresses (one, two, three), which owners it mixes and in which order, how many pairs a
//! write carries, a key the seed already carried being overwritten, an empty tag value, a
//! resource nothing seeded, and a removal that empties the set. The second test's rows
//! span where an unresolvable ARN sits in the batch, which owner it belongs to, how many
//! of them there are, and whether the batch tags or untags. The case the issue cites — a
//! bucket and a table in one batch, then a key removed — is the first row of each.
//!
//! Nothing waits on the wall clock: the one state a clock owes (a DynamoDB tag change
//! becoming visible) is polled for.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_dynamodb_dynamodb_20120810 as owner_dynamodb;
use service_com_amazonaws_resourcegroupstaggingapi_resourcegroupstaggingapi_20170126 as tagging_api;
use service_com_amazonaws_s3_amazons3 as owner_s3;

use crate::cloudformation_provisions_stack_resources::xml_texts;

/// The Region every request below is signed for, and the Region the estate lives in.
const REGION: &str = "us-west-2";

/// The account segment every ARN this emulator mints carries.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// The XML namespace S3's own model declares, which a client's payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The SigV4 signing names the router routes a request to a service by.
const TAGGING: &str = "tagging";
const DYNAMODB: &str = "dynamodb";
const S3: &str = "s3";

/// How long a poll keeps looking for a state the clock owes. Generous: it bounds nothing a
/// row asserts, it only stops a broken emulator from hanging.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again — short next to the span it waits out, so
/// the loop observes the change rather than the deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The key each row of the mixed-batch test seeds through the owner's OWN API, and its
/// value.
const PROBE_KEY: &str = "Probe";
const PROBE_VALUE: &str = "seeded";

/// The key the mixed batches add, and its value.
const EXTRA_KEY: &str = "Extra";
const EXTRA_VALUE: &str = "added";

/// A router carrying the tagging API and both owning services over ONE engine — the
/// arrangement `core/router` builds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises handler behaviour, not SigV4: the `Authorization` header below is
    // present because its credential scope is what routes a request to a signing name and
    // names the Region, not to be verified.
    router.set_request_authentication(false);
    router.register_service(tagging_api::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(owner_dynamodb::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(owner_s3::EmulatedService::new(engine));
    router
}

/// The `Authorization` header whose credential scope names `signing_name` and [`REGION`].
fn credential_scope(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One operation of an RPC-wire service: its HTTP status and parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    content_type: &str,
    target: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .header("authorization", credential_scope(signing_name))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{target} answered a body that is not JSON ({error}): {}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, parsed)
}

/// One tagging-API operation: its HTTP status and parsed body.
async fn tagging(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    json_call(
        router,
        TAGGING,
        "application/x-amz-json-1.1",
        &format!("ResourceGroupsTaggingAPI_20170126.{operation}"),
        body,
    )
    .await
}

/// One tagging-API operation, which must answer the modelled 200.
async fn tagging_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = tagging(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must answer 200, got {status} with body {parsed}"
    );
    parsed
}

/// One DynamoDB operation: its HTTP status and parsed body.
async fn ddb(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    json_call(
        router,
        DYNAMODB,
        "application/x-amz-json-1.0",
        &format!("DynamoDB_20120810.{operation}"),
        body,
    )
    .await
}

/// One DynamoDB operation, which must answer 200.
async fn ddb_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = ddb(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must answer 200, got {status} with body {parsed}"
    );
    parsed
}

/// One response whose body is XML: the status and the body as text.
struct XmlAnswer {
    status: u16,
    body: String,
}

/// One S3 request over its REST wire, addressed path-style at the Region's endpoint.
async fn s3_call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &str,
) -> XmlAnswer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/xml")
        .header("authorization", credential_scope(S3))
        .body(bytes::Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    XmlAnswer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// Escape a value for an XML text node, the way a client's XML writer does.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `PUT /<bucket>` — S3's `CreateBucket`, placing the bucket in [`REGION`].
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let answer = s3_call(
        router,
        "PUT",
        &format!("/{bucket}"),
        &format!(
            "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
             <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
        ),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "setup: CreateBucket({bucket:?}) must succeed, got {} / {}",
        answer.status, answer.body
    );
}

/// `PUT /<bucket>?tagging` — S3's `PutBucketTagging`, which REPLACES the whole set.
async fn put_bucket_tagging(
    router: &MultiServiceRouter,
    bucket: &str,
    tags: &[(&str, &str)],
) {
    let elements: String = tags
        .iter()
        .map(|(key, value)| {
            format!(
                "<Tag><Key>{}</Key><Value>{}</Value></Tag>",
                xml_escape(key),
                xml_escape(value)
            )
        })
        .collect();
    let answer = s3_call(
        router,
        "PUT",
        &format!("/{bucket}?tagging"),
        &format!("<Tagging xmlns=\"{S3_XMLNS}\"><TagSet>{elements}</TagSet></Tagging>"),
    )
    .await;
    assert_eq!(
        answer.status, 204,
        "setup: PutBucketTagging({bucket:?}) must succeed, got {} / {}",
        answer.status, answer.body
    );
}

/// What S3's own `GetBucketTagging` answers for one bucket: the status, the `<Code>` and
/// `<Message>` of a refusal, and the `(Key, Value)` pairs of a reported tag set.
struct BucketTagging {
    status: u16,
    code: String,
    message: String,
    tags: BTreeMap<String, String>,
}

/// `GET /<bucket>?tagging` — S3's `GetBucketTagging`, the owning service's OWN tag reader.
async fn bucket_tagging(router: &MultiServiceRouter, bucket: &str) -> BucketTagging {
    let answer = s3_call(router, "GET", &format!("/{bucket}?tagging"), "").await;
    let first = |tag: &str| {
        xml_texts(&answer.body, tag)
            .into_iter()
            .next()
            .unwrap_or_default()
    };
    let (code, message) = (first("Code"), first("Message"));
    let tags = if code.is_empty() {
        xml_texts(&answer.body, "Key")
            .into_iter()
            .zip(xml_texts(&answer.body, "Value"))
            .collect()
    } else {
        BTreeMap::new()
    };
    BucketTagging {
        status: answer.status,
        code,
        message,
        tags,
    }
}

/// An S3 bucket's ARN: `arn:aws:s3:::<bucket>`, whose Region and account fields are empty
/// because a bucket name is globally unique.
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// The ARN form DynamoDB mints for a table of the serving Region and account.
fn table_arn(table: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT_ID}:table/{table}")
}

/// Create a table — `PAY_PER_REQUEST`, one `HASH` key — carrying `tags`, and check it is
/// named by the ARN this API addresses it with.
async fn create_table(router: &MultiServiceRouter, table: &str, tags: &[(&str, &str)]) {
    let mut request = json!({
        "TableName": table,
        "KeySchema": [{ "AttributeName": "Id", "KeyType": "HASH" }],
        "AttributeDefinitions": [{ "AttributeName": "Id", "AttributeType": "S" }],
        "BillingMode": "PAY_PER_REQUEST",
    });
    if !tags.is_empty() {
        request["Tags"] = Value::Array(
            tags.iter()
                .map(|(key, value)| json!({ "Key": key, "Value": value }))
                .collect(),
        );
    }
    let created = ddb_ok(router, "CreateTable", request).await;
    let minted = created["TableDescription"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("a CreateTable body with no TableArn: {created}"));
    assert_eq!(
        minted,
        table_arn(table),
        "setup: expected CreateTable to mint {}, got {minted}",
        table_arn(table)
    );
}

/// The tag map DynamoDB's own `ListTagsOfResource` reports for one ARN, with the raw
/// answer for a message.
async fn listed_tags(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    ddb(router, "ListTagsOfResource", json!({ "ResourceArn": arn })).await
}

/// The tag pairs a `ListTagsOfResource` body reports, as the one ordered map this suite
/// handles tags in.
fn reported_tags(body: &Value) -> BTreeMap<String, String> {
    body["Tags"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|tag| {
            (
                tag["Key"].as_str().unwrap_or_default().to_string(),
                tag["Value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// A declared tag set as the map the assertions compare against.
fn pairs(of: &[(&str, &str)]) -> BTreeMap<String, String> {
    of.iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// A declared tag set as the `TagMap` `TagResources` takes.
fn tag_map(of: &[(&str, &str)]) -> Value {
    Value::Object(
        of.iter()
            .map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string())))
            .collect(),
    )
}

/// One `FailedResourcesMap` entry: the three members `FailureInfo` models.
#[derive(Debug, PartialEq, Eq)]
struct Failure {
    status_code: i64,
    error_code: String,
    error_message: String,
}

/// The `FailedResourcesMap` of a batch answer, keyed by ARN.
fn failures(body: &Value) -> BTreeMap<String, Failure> {
    body["FailedResourcesMap"]
        .as_object()
        .unwrap_or_else(|| panic!("a batch answer with no FailedResourcesMap: {body}"))
        .iter()
        .map(|(arn, failure)| {
            (
                arn.clone(),
                Failure {
                    status_code: failure["StatusCode"].as_i64().unwrap_or_default(),
                    error_code: failure["ErrorCode"].as_str().unwrap_or_default().to_string(),
                    error_message: failure["ErrorMessage"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                },
            )
        })
        .collect()
}

/// Look again until `settled` answers `Ok`, or [`POLL_DEADLINE`] passes.
///
/// A DynamoDB tag change is eventually consistent, so the condition is OWED rather than
/// immediate: the loop waits for the CONDITION and reports the last answer it saw, which
/// is what names the failure.
async fn poll_until<T, F, Fut>(case: &str, what: &str, mut settled: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let started = Instant::now();
    loop {
        let last = match settled().await {
            Ok(value) => return value,
            Err(seen) => seen,
        };
        if started.elapsed() >= POLL_DEADLINE {
            panic!("case {case:?}: {what} never settled; the last answer was {last}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Which service owns a member of a batch, and therefore whose tagging surface the write
/// has to land on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    S3,
    DynamoDb,
}

/// One resource of a batch: its owner, its name, the set its OWN API seeded, and the set
/// it must hold once the batch's writes have landed — computed from the merge rule both
/// planes state, not read back out of an answer.
struct Member {
    owner: Owner,
    name: &'static str,
    seeded: &'static [(&'static str, &'static str)],
    settled: &'static [(&'static str, &'static str)],
}

/// One batch: the members in the order `ResourceARNList` carries them, the pairs
/// `TagResources` adds, and the keys `UntagResources` then removes.
struct Row {
    case: &'static str,
    members: &'static [Member],
    added: &'static [(&'static str, &'static str)],
    removed: &'static [&'static str],
}

/// The batches. They span the owners a batch mixes and the order it names them in, how
/// many resources it addresses, how many pairs a write carries, a seeded key being
/// overwritten, an empty tag value, a resource nothing seeded, and a removal that leaves
/// nothing. The first row is the case the issue cites.
const ROWS: &[Row] = &[
    Row {
        case: "a bucket and a table in one batch, a key added and a key removed",
        members: &[
            Member {
                owner: Owner::S3,
                name: "ownertags-pair-bucket",
                seeded: &[("Env", "prod"), ("Owner", "platform")],
                settled: &[("Env", "prod"), ("Team", "core")],
            },
            Member {
                owner: Owner::DynamoDb,
                name: "ownertags-pair-table",
                seeded: &[("Env", "prod"), ("Owner", "platform")],
                settled: &[("Env", "prod"), ("Team", "core")],
            },
        ],
        added: &[("Team", "core")],
        removed: &["Owner"],
    },
    Row {
        case: "the table named first and the bucket second, two pairs added",
        members: &[
            Member {
                owner: Owner::DynamoDb,
                name: "ownertags-order-table",
                seeded: &[("Env", "dev")],
                settled: &[("Env", "dev"), ("Team", "core"), ("Tier", "gold")],
            },
            Member {
                owner: Owner::S3,
                name: "ownertags-order-bucket",
                seeded: &[("Env", "dev")],
                settled: &[("Env", "dev"), ("Team", "core"), ("Tier", "gold")],
            },
        ],
        added: &[("Team", "core"), ("Tier", "gold")],
        removed: &[],
    },
    Row {
        case: "two buckets and a table, the write overwriting a seeded key",
        members: &[
            Member {
                owner: Owner::S3,
                name: "ownertags-three-bucket-a",
                seeded: &[("Env", "dev"), ("Owner", "platform")],
                settled: &[("Env", "prod"), ("Owner", "platform")],
            },
            Member {
                owner: Owner::S3,
                name: "ownertags-three-bucket-b",
                seeded: &[("Env", "dev")],
                settled: &[("Env", "prod")],
            },
            Member {
                owner: Owner::DynamoDb,
                name: "ownertags-three-table",
                seeded: &[("Env", "dev"), ("Team", "core")],
                settled: &[("Env", "prod"), ("Team", "core")],
            },
        ],
        added: &[("Env", "prod")],
        removed: &[],
    },
    Row {
        case: "resources nothing seeded",
        members: &[
            Member {
                owner: Owner::S3,
                name: "ownertags-fresh-bucket",
                seeded: &[],
                settled: &[("Team", "core")],
            },
            Member {
                owner: Owner::DynamoDb,
                name: "ownertags-fresh-table",
                seeded: &[],
                settled: &[("Team", "core")],
            },
        ],
        added: &[("Team", "core")],
        removed: &[],
    },
    Row {
        // `TagValueString` is `len 0..256`, so the empty string is a tag VALUE and not an
        // absence of one, and a value carrying a space is an ordinary value.
        case: "an empty tag value and a value carrying a space",
        members: &[
            Member {
                owner: Owner::S3,
                name: "ownertags-values-bucket",
                seeded: &[("Env", "prod")],
                settled: &[("Env", ""), ("Owner", "platform team")],
            },
            Member {
                owner: Owner::DynamoDb,
                name: "ownertags-values-table",
                seeded: &[("Env", "prod")],
                settled: &[("Env", ""), ("Owner", "platform team")],
            },
        ],
        added: &[("Env", ""), ("Owner", "platform team")],
        removed: &[],
    },
    Row {
        // Every key removed: a bucket then has no tag set at all, which is the
        // `NoSuchTagSet` `GetBucketTagging` documents, and a table reports an empty set.
        case: "every key removed, leaving no tag at all",
        members: &[
            Member {
                owner: Owner::S3,
                name: "ownertags-emptied-bucket",
                seeded: &[("Env", "prod")],
                settled: &[],
            },
            Member {
                owner: Owner::DynamoDb,
                name: "ownertags-emptied-table",
                seeded: &[("Env", "prod")],
                settled: &[],
            },
        ],
        added: &[("Team", "core")],
        removed: &["Env", "Team"],
    },
];

/// The ARN the owning service names `member` by.
fn member_arn(member: &Member) -> String {
    match member.owner {
        Owner::S3 => bucket_arn(member.name),
        Owner::DynamoDb => table_arn(member.name),
    }
}

/// Create `member` through its OWNER's own API, carrying the set that row seeded.
async fn create_member(router: &MultiServiceRouter, member: &Member) {
    match member.owner {
        Owner::S3 => {
            create_bucket(router, member.name).await;
            if !member.seeded.is_empty() {
                put_bucket_tagging(router, member.name, member.seeded).await;
            }
        }
        Owner::DynamoDb => create_table(router, member.name, member.seeded).await,
    }
}

/// Assert the OWNING service's own tag reader reports `want` for `member` — the reader
/// that did not write the tags.
async fn assert_owner_reports(router: &MultiServiceRouter, case: &str, member: &Member) {
    let want = pairs(member.settled);
    let arn = member_arn(member);
    match member.owner {
        Owner::S3 if want.is_empty() => {
            // "GetBucketTagging has the following special error: Error code: NoSuchTagSet"
            // — a bucket carrying no tag has no tag set to report.
            let answer = bucket_tagging(router, member.name).await;
            assert_eq!(
                (answer.status, answer.code.as_str()),
                (404, "NoSuchTagSet"),
                "case {case:?}: expected S3 to report no tag set for {arn}, got {} / {} \
                 with {:?}",
                answer.status,
                answer.code,
                answer.tags
            );
        }
        Owner::S3 => {
            let answer = bucket_tagging(router, member.name).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: GetBucketTagging({arn}) must be served, got {} / {}",
                answer.status, answer.code
            );
            assert_eq!(
                answer.tags, want,
                "case {case:?}: expected S3 to report {want:?} for {arn}, got {:?}",
                answer.tags
            );
        }
        Owner::DynamoDb => {
            poll_until(case, "the table's own tag set", || async {
                let (status, body) = listed_tags(router, &arn).await;
                if status != 200 {
                    return Err(format!("HTTP {status} with {body}"));
                }
                let got = reported_tags(&body);
                if got == want {
                    Ok(())
                } else {
                    Err(format!("{got:?}, expected {want:?}"))
                }
            })
            .await;
        }
    }
}

/// A batch spanning two owning services writes each resource's OWN tag set: after
/// `TagResources` adds keys and `UntagResources` removes keys, with both reporting no
/// failed resource, every member's own tag reader reports exactly the surviving keys.
#[tokio::test]
async fn a_batch_write_lands_on_every_members_own_tag_set() {
    let router = emulator();

    // Every row's estate is created and written first, so the propagation DynamoDB owes
    // each row's tag change runs alongside the others rather than one after another.
    for row in ROWS {
        let case = row.case;
        for member in row.members {
            create_member(&router, member).await;
        }
        let arns: Vec<String> = row.members.iter().map(member_arn).collect();

        let answered = tagging_ok(
            &router,
            "TagResources",
            json!({ "ResourceARNList": &arns, "Tags": tag_map(row.added) }),
        )
        .await;
        let failed = failures(&answered);
        assert!(
            failed.is_empty(),
            "case {case:?}: TagResources over {arns:?} must fail no resource, got {failed:?}"
        );

        if !row.removed.is_empty() {
            let answered = tagging_ok(
                &router,
                "UntagResources",
                json!({ "ResourceARNList": &arns, "TagKeys": row.removed }),
            )
            .await;
            let failed = failures(&answered);
            assert!(
                failed.is_empty(),
                "case {case:?}: UntagResources over {arns:?} must fail no resource, got \
                 {failed:?}"
            );
        }
    }

    for row in ROWS {
        for member in row.members {
            assert_owner_reports(&router, row.case, member).await;
        }
    }
}

/// One member of a mixed batch: the live resource of an owner, or a name that owner holds
/// nothing under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Addressed {
    LiveBucket,
    LiveTable,
    AbsentBucket,
    AbsentTable,
}

/// What a mixed batch does to the live resources it reaches.
enum Change {
    /// `TagResources` with these pairs.
    Add(&'static [(&'static str, &'static str)]),
    /// `UntagResources` with these keys.
    Remove(&'static [&'static str]),
}

/// One mixed batch: the members in the order `ResourceARNList` carries them, the change it
/// makes, and the set each LIVE member holds afterwards.
struct MixedRow {
    case: &'static str,
    addressed: &'static [Addressed],
    change: Change,
    settled: &'static [(&'static str, &'static str)],
}

/// The mixed batches. They span where an unresolvable ARN sits (first, last, several at
/// once, alone), which owner it belongs to, and whether the batch tags or untags. The
/// first row is the case the issue cites.
const MIXED_ROWS: &[MixedRow] = &[
    MixedRow {
        case: "a live bucket and a table that was never created",
        addressed: &[Addressed::LiveBucket, Addressed::AbsentTable],
        change: Change::Add(&[(EXTRA_KEY, EXTRA_VALUE)]),
        settled: &[(EXTRA_KEY, EXTRA_VALUE), (PROBE_KEY, PROBE_VALUE)],
    },
    MixedRow {
        case: "a bucket that was never created, named before a live table",
        addressed: &[Addressed::AbsentBucket, Addressed::LiveTable],
        change: Change::Add(&[(EXTRA_KEY, EXTRA_VALUE)]),
        settled: &[(EXTRA_KEY, EXTRA_VALUE), (PROBE_KEY, PROBE_VALUE)],
    },
    MixedRow {
        case: "both owners' absent resources ahead of both owners' live ones",
        addressed: &[
            Addressed::AbsentBucket,
            Addressed::AbsentTable,
            Addressed::LiveBucket,
            Addressed::LiveTable,
        ],
        change: Change::Add(&[(EXTRA_KEY, EXTRA_VALUE)]),
        settled: &[(EXTRA_KEY, EXTRA_VALUE), (PROBE_KEY, PROBE_VALUE)],
    },
    MixedRow {
        case: "an untag over a live bucket and an absent table",
        addressed: &[Addressed::LiveBucket, Addressed::AbsentTable],
        change: Change::Remove(&[PROBE_KEY]),
        settled: &[],
    },
    MixedRow {
        case: "an untag over an absent bucket and a live table",
        addressed: &[Addressed::AbsentBucket, Addressed::LiveTable],
        change: Change::Remove(&[PROBE_KEY]),
        settled: &[],
    },
    MixedRow {
        case: "a batch of one absent resource alone",
        addressed: &[Addressed::AbsentTable],
        change: Change::Add(&[(EXTRA_KEY, EXTRA_VALUE)]),
        settled: &[],
    },
];

/// The refusal an owning service answers ITSELF for an ARN it holds no resource under:
/// the code, the status and the sentence it words — which is what a `FailureInfo` for that
/// ARN has to carry, rather than anything this API words for it.
struct OwnRefusal {
    code: String,
    status: i64,
    message: String,
}

/// What S3 itself answers when its own tag reader is handed a bucket it holds nothing
/// under.
async fn s3_own_refusal(router: &MultiServiceRouter, bucket: &str) -> OwnRefusal {
    let answer = bucket_tagging(router, bucket).await;
    assert_eq!(
        (answer.status, answer.code.as_str()),
        (404, "NoSuchBucket"),
        "setup: expected S3's own NoSuchBucket for {bucket:?}, got {} / {}",
        answer.status,
        answer.code
    );
    OwnRefusal {
        code: answer.code,
        status: i64::from(answer.status),
        message: answer.message,
    }
}

/// What DynamoDB itself answers when its own tag reader is handed an ARN it holds no table
/// for.
async fn dynamodb_own_refusal(router: &MultiServiceRouter, table: &str) -> OwnRefusal {
    let arn = table_arn(table);
    let (status, body) = listed_tags(router, &arn).await;
    let kind = body["__type"].as_str().unwrap_or_default().to_string();
    assert!(
        status == 400 && kind.ends_with("ResourceNotFoundException"),
        "setup: expected DynamoDB's own ResourceNotFoundException for {arn}, got {status} \
         with {body}"
    );
    OwnRefusal {
        code: "ResourceNotFoundException".to_string(),
        status: i64::from(status),
        message: body["message"].as_str().unwrap_or_default().to_string(),
    }
}

/// An ARN in a batch whose owning service holds no such resource is reported in
/// `FailedResourcesMap` under that ARN, carrying THAT service's own error code, status and
/// message, with HTTP 200 overall — while every live ARN of the same batch is written, and
/// nothing is brought into being for the absent one.
#[tokio::test]
async fn an_arn_its_owner_holds_no_resource_for_is_reported_per_resource() {
    let router = emulator();

    for (index, row) in MIXED_ROWS.iter().enumerate() {
        let case = row.case;
        // A row's own estate, so one row's writes cannot be read as another's.
        let live_bucket = format!("ownertags-mixed-live-{index}");
        let live_table = format!("ownertags-mixed-live-{index}");
        let absent_bucket = format!("ownertags-mixed-ghost-{index}");
        let absent_table = format!("ownertags-mixed-ghost-{index}");

        let mut live: Vec<Addressed> = Vec::new();
        for member in row.addressed {
            match member {
                Addressed::LiveBucket => {
                    create_bucket(&router, &live_bucket).await;
                    put_bucket_tagging(&router, &live_bucket, &[(PROBE_KEY, PROBE_VALUE)]).await;
                    live.push(*member);
                }
                Addressed::LiveTable => {
                    create_table(&router, &live_table, &[(PROBE_KEY, PROBE_VALUE)]).await;
                    live.push(*member);
                }
                Addressed::AbsentBucket | Addressed::AbsentTable => {}
            }
        }

        // The refusals are taken from the OWNERS themselves, before the batch runs, so the
        // expectation is that service's own answer rather than a sentence worded here.
        let mut expected: BTreeMap<String, Failure> = BTreeMap::new();
        let mut arns: Vec<String> = Vec::new();
        for member in row.addressed {
            let arn = match member {
                Addressed::LiveBucket => bucket_arn(&live_bucket),
                Addressed::LiveTable => table_arn(&live_table),
                Addressed::AbsentBucket => bucket_arn(&absent_bucket),
                Addressed::AbsentTable => table_arn(&absent_table),
            };
            let own = match member {
                Addressed::AbsentBucket => Some(s3_own_refusal(&router, &absent_bucket).await),
                Addressed::AbsentTable => Some(dynamodb_own_refusal(&router, &absent_table).await),
                Addressed::LiveBucket | Addressed::LiveTable => None,
            };
            if let Some(own) = own {
                expected.insert(
                    arn.clone(),
                    Failure {
                        status_code: own.status,
                        error_code: own.code,
                        error_message: own.message,
                    },
                );
            }
            arns.push(arn);
        }

        let (operation, request) = match &row.change {
            Change::Add(tags) => (
                "TagResources",
                json!({ "ResourceARNList": &arns, "Tags": tag_map(tags) }),
            ),
            Change::Remove(keys) => (
                "UntagResources",
                json!({ "ResourceARNList": &arns, "TagKeys": keys }),
            ),
        };
        let answered = tagging_ok(&router, operation, request).await;
        let failed = failures(&answered);
        assert_eq!(
            failed, expected,
            "case {case:?}: expected {operation} over {arns:?} to report {expected:?}, got \
             {failed:?}"
        );

        // The served half of the batch was really written, read back through each owner's
        // OWN tag reader.
        let want = pairs(row.settled);
        if live.contains(&Addressed::LiveBucket) {
            let answer = bucket_tagging(&router, &live_bucket).await;
            if want.is_empty() {
                assert_eq!(
                    (answer.status, answer.code.as_str()),
                    (404, "NoSuchTagSet"),
                    "case {case:?}: expected the live bucket to hold no tag set, got {} / {} \
                     with {:?}",
                    answer.status,
                    answer.code,
                    answer.tags
                );
            } else {
                assert_eq!(
                    answer.tags, want,
                    "case {case:?}: expected S3 to report {want:?} for the live bucket, got \
                     {:?} (HTTP {} / {})",
                    answer.tags, answer.status, answer.code
                );
            }
        }
        if live.contains(&Addressed::LiveTable) {
            let arn = table_arn(&live_table);
            poll_until(case, "the live table's own tag set", || async {
                let (status, body) = listed_tags(&router, &arn).await;
                if status != 200 {
                    return Err(format!("HTTP {status} with {body}"));
                }
                let got = reported_tags(&body);
                if got == want {
                    Ok(())
                } else {
                    Err(format!("{got:?}, expected {want:?}"))
                }
            })
            .await;
        }

        // Nothing may have brought an absent resource into being: its owner still holds
        // nothing under that name.
        for member in row.addressed {
            match member {
                Addressed::AbsentBucket => {
                    let answer = bucket_tagging(&router, &absent_bucket).await;
                    assert_eq!(
                        (answer.status, answer.code.as_str()),
                        (404, "NoSuchBucket"),
                        "case {case:?}: the absent bucket must still be absent, got {} / {}",
                        answer.status,
                        answer.code
                    );
                }
                Addressed::AbsentTable => {
                    let (status, body) = listed_tags(&router, &table_arn(&absent_table)).await;
                    let kind = body["__type"].as_str().unwrap_or_default();
                    assert!(
                        status == 400 && kind.ends_with("ResourceNotFoundException"),
                        "case {case:?}: the absent table must still be absent, got {status} \
                         with {body}"
                    );
                }
                Addressed::LiveBucket | Addressed::LiveTable => {}
            }
        }
    }
}
