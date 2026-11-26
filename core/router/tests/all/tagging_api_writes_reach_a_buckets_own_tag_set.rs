// SPDX-License-Identifier: CC0-1.0

//! A write the central tagging API accepts for an Amazon S3 bucket is the BUCKET's own tag
//! set, on the WHOLE assembled emulator.
//!
//! The rule under test: a resource whose tag set the owning service models has exactly ONE
//! tag set, whichever API writes it, so once `TagResources` answers 200 with an ARN absent
//! from `FailedResourcesMap` ("A map containing a key-value pair for each failed item that
//! couldn't be tagged … If there are no errors, the FailedResourcesMap is empty",
//! `TagResources$FailedResourcesMap`) that ARN's owner reports the upserted pairs merged
//! with the pairs it already held, and `UntagResources` removes exactly the keys it names
//! from that same set. For a general purpose bucket the owner's read is `GetBucketTagging`,
//! whose `NoSuchTagSet` refusal therefore means the bucket carries no tag at all — not that
//! a write landed somewhere else; for a table it is `ListTagsOfResource`.
//!
//! Two sibling suites pin that rule already, and this one does NOT claim their coverage:
//! `tagging_api_batch_reaches_each_owners_tag_set.rs` spans how many resources a batch
//! addresses, which owners it mixes, the pairs a write carries and the refusals an
//! unresolvable ARN earns, and `s3_bucket_tags_reach_resource_group_discovery.rs` spans the
//! bucket leg inside a Resource Groups / Application Insights scenario. What this one adds
//! is the FORM OF THE CREATE the issue's own first step takes: a bucket created in the
//! Region whose creates carry no `CreateBucketConfiguration` at all, beside one created with
//! a location constraint — the two forms a client sends — each then tagged, untagged and
//! read back through its owner, with the account's tagged-resource view read over a mixed
//! batch as the second reader of the same records.
//!
//! The rows differ along what the rule generalises over: whether S3's own `PutBucketTagging`
//! seeded the bucket first, the Region the bucket lives in, an empty tag value
//! (`TagValueString` is len 0..256, so a value and not an absence), and an untag that
//! empties the set, after which the owner's own read is the refusal again. The single-key
//! case the issue cites is the first row.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread. Nothing waits on the wall clock: a tag change an
//! owning service applies asynchronously is polled for.

use std::collections::BTreeMap;
use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::applicationinsights_wire::{poll_until, Answer, BOOT_STACK_SIZE};
use crate::cloudformation_provisions_stack_resources::xml_texts;
use crate::query_wire::xml_escape;

/// The Region most rows are served in, whose creates carry a location constraint.
const REGION: &str = "us-west-2";

/// The Region whose creates carry no `CreateBucketConfiguration`.
const HOME_REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The XML namespace S3's own model declares, which a client's payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The `X-Amz-Target` prefixes the tagging API's awsJson1_1 wire and DynamoDB's awsJson1_0
/// wire name their operations under.
const TAGGING_TARGET: &str = "ResourceGroupsTaggingAPI_20170126";
const DYNAMODB_TARGET: &str = "DynamoDB_20120810";

/// The code and the fixed sentence S3 publishes for a bucket that carries no tag set.
const NO_SUCH_TAG_SET: &str = "NoSuchTagSet";
const NO_SUCH_TAG_SET_MESSAGE: &str = "The TagSet does not exist";

/// The whole emulator over one engine, exactly as the binary assembles it, with signature
/// verification off: the `Authorization` header below is what routes a request and tells a
/// handler its Region.
fn router() -> MultiServiceRouter {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    router.set_request_authentication(false);
    router
}

/// What one S3 call answered: the status and the body as text.
struct XmlAnswer {
    status: u16,
    body: String,
}

/// One S3 request, addressed to the regional endpoint the credential scope names.
fn s3(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    method: &str,
    path: &str,
    body: &str,
) -> XmlAnswer {
    let host = format!("s3.{region}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/xml")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{region}/s3/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    XmlAnswer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// One awsJson call to `signing_name` in `region`, under the target prefix and content type
/// that wire names.
fn json_call(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    region: &str,
    content_type: &str,
    target: &str,
    body: Value,
) -> Answer {
    let host = format!("{signing_name}.{region}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{region}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let payload = response.into_body();
    let body = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    Answer { status, body }
}

/// One resource-tagging operation, which must answer the modelled 200.
fn tagging_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    operation: &str,
    body: Value,
) -> Value {
    let answer = json_call(
        router,
        runtime,
        "tagging",
        region,
        "application/x-amz-json-1.1",
        &format!("{TAGGING_TARGET}.{operation}"),
        body,
    );
    assert_eq!(
        answer.status, 200,
        "case {operation} in {region}: expected HTTP 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// One DynamoDB operation, which must answer 200.
fn dynamodb_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    operation: &str,
    body: Value,
) -> Value {
    let answer = json_call(
        router,
        runtime,
        "dynamodb",
        region,
        "application/x-amz-json-1.0",
        &format!("{DYNAMODB_TARGET}.{operation}"),
        body,
    );
    assert_eq!(
        answer.status, 200,
        "case {operation} in {region}: expected HTTP 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// The `<Tag>` elements of a tag set, the element name S3's model gives `TagSet`'s member.
fn tag_elements(pairs: &BTreeMap<String, String>) -> String {
    pairs
        .iter()
        .map(|(key, value)| {
            format!(
                "<Tag><Key>{}</Key><Value>{}</Value></Tag>",
                xml_escape(key),
                xml_escape(value)
            )
        })
        .collect()
}

/// The tag pairs a row names, as the one ordered map this suite handles tags in.
fn pairs(of: &[(&str, &str)]) -> BTreeMap<String, String> {
    of.iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// An S3 bucket's ARN: `arn:aws:s3:::<bucket>`, whose Region and account fields are empty
/// because a bucket name is globally unique.
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// A DynamoDB table's ARN of the serving Region and account.
fn table_arn(region: &str, table: &str) -> String {
    format!("arn:aws:dynamodb:{region}:{ACCOUNT}:table/{table}")
}

/// `PUT /<bucket>` — S3's `CreateBucket`. A create addressed outside the API's home Region
/// names its location constraint; one addressed to the home Region carries no configuration
/// at all, which is the form a client sends there.
fn create_bucket(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    bucket: &str,
) {
    let body = if region == HOME_REGION {
        String::new()
    } else {
        format!(
            "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
             <LocationConstraint>{region}</LocationConstraint></CreateBucketConfiguration>"
        )
    };
    let answer = s3(router, runtime, region, "PUT", &format!("/{bucket}"), &body);
    assert_eq!(
        answer.status, 200,
        "setup: CreateBucket({bucket:?}) in {region} must succeed, got {} / {}",
        answer.status, answer.body
    );
}

/// `PUT /<bucket>?tagging` — S3's `PutBucketTagging`, which REPLACES the whole set.
fn put_bucket_tagging(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    bucket: &str,
    tags: &BTreeMap<String, String>,
) {
    let answer = s3(
        router,
        runtime,
        region,
        "PUT",
        &format!("/{bucket}?tagging"),
        &format!(
            "<Tagging xmlns=\"{S3_XMLNS}\"><TagSet>{}</TagSet></Tagging>",
            tag_elements(tags)
        ),
    );
    assert_eq!(
        answer.status, 204,
        "setup: PutBucketTagging({bucket:?}) must succeed, got {} / {}",
        answer.status, answer.body
    );
}

/// What S3's own `GetBucketTagging` reports for one bucket: the status, the pairs in key
/// order (a tag set is a set, so no member order is part of the contract), and the `<Code>`
/// and `<Message>` of a refusal.
struct Reported {
    status: u16,
    tags: BTreeMap<String, String>,
    code: String,
    message: String,
}

/// `GET /<bucket>?tagging` — S3's `GetBucketTagging`.
fn bucket_tagging(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    bucket: &str,
) -> Reported {
    let answer = s3(
        router,
        runtime,
        region,
        "GET",
        &format!("/{bucket}?tagging"),
        "",
    );
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
    Reported {
        status: answer.status,
        tags,
        code,
        message,
    }
}

/// The tag map DynamoDB's own `ListTagsOfResource` reports for one ARN.
fn table_tags(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    arn: &str,
) -> BTreeMap<String, String> {
    let body = dynamodb_ok(
        router,
        runtime,
        region,
        "ListTagsOfResource",
        json!({ "ResourceArn": arn }),
    );
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

/// A table of the serving Region, `PAY_PER_REQUEST` with one `HASH` key, carrying `tags`, as
/// the ARN its own `TableDescription` reports.
fn create_table(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    table: &str,
    tags: &BTreeMap<String, String>,
) -> String {
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
    let created = dynamodb_ok(router, runtime, region, "CreateTable", request);
    let minted = created["TableDescription"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: a CreateTable body with no TableArn: {created}"))
        .to_string();
    assert_eq!(
        minted,
        table_arn(region, table),
        "setup: expected CreateTable to mint {}, got {minted}",
        table_arn(region, table)
    );
    minted
}

/// The `(ARN -> tags)` map one `GetResources` answer reports.
fn discovered(answer: &Value) -> BTreeMap<String, BTreeMap<String, String>> {
    answer["ResourceTagMappingList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|mapping| {
            let arn = mapping["ResourceARN"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let tags = mapping["Tags"]
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
                .collect();
            (arn, tags)
        })
        .collect()
}

/// Run `body` on a thread with the stack the whole router's assembly needs.
fn on_a_sized_thread(name: &str, body: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(body)
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}

/// One tagging path onto a bucket: what S3's own API seeded, what this API upserts, what it
/// then withdraws, and the whole set the bucket must carry afterwards.
struct Row {
    case: &'static str,
    bucket: &'static str,
    region: &'static str,
    seeded: &'static [(&'static str, &'static str)],
    upserted: &'static [(&'static str, &'static str)],
    withdrawn: &'static [&'static str],
    settled: &'static [(&'static str, &'static str)],
}

/// The rows: a single key onto a bucket nothing tagged in the Region whose creates carry no
/// configuration (the case the issue cites), the seed-add-remove sequence in a Region whose
/// creates name a location constraint, an empty tag value, and an untag that empties the set.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one key onto a bucket nothing tagged, created without a configuration",
            bucket: "facade-first-write",
            region: HOME_REGION,
            seeded: &[],
            upserted: &[("owner", "platform")],
            withdrawn: &[],
            settled: &[("owner", "platform")],
        },
        Row {
            case: "a seeded set in another Region, one key added and one removed",
            bucket: "facade-seeded-add-remove",
            region: REGION,
            seeded: &[("keep", "kept"), ("drop", "dropped")],
            upserted: &[("added", "new")],
            withdrawn: &["drop"],
            settled: &[("added", "new"), ("keep", "kept")],
        },
        Row {
            case: "an empty tag value",
            bucket: "facade-empty-value",
            region: HOME_REGION,
            seeded: &[],
            upserted: &[("owner", "")],
            withdrawn: &[],
            settled: &[("owner", "")],
        },
        Row {
            case: "an untag that empties the set",
            bucket: "facade-emptied",
            region: REGION,
            seeded: &[("only", "one")],
            upserted: &[("second", "two")],
            withdrawn: &["only", "second"],
            settled: &[],
        },
    ]
}

/// A bucket has ONE tag set: a `TagResources` write this API accepts is what S3's own
/// `GetBucketTagging` reports, merged with whatever `PutBucketTagging` had already set, and
/// an `UntagResources` removes exactly the keys it names from that same set. Only a bucket
/// left carrying no tag at all is the `NoSuchTagSet` refusal.
#[test]
fn a_tagging_api_write_is_the_buckets_own_tag_set() {
    on_a_sized_thread("tagging-facade-bucket", || {
        let router = router();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");

        for row in rows() {
            create_bucket(&router, &runtime, row.region, row.bucket);
            let seeded = pairs(row.seeded);
            if !seeded.is_empty() {
                put_bucket_tagging(&router, &runtime, row.region, row.bucket, &seeded);
            }
            let arn = bucket_arn(row.bucket);

            if !row.upserted.is_empty() {
                let answered = tagging_ok(
                    &router,
                    &runtime,
                    row.region,
                    "TagResources",
                    json!({ "ResourceARNList": [&arn], "Tags": pairs(row.upserted) }),
                );
                assert_eq!(
                    answered["FailedResourcesMap"],
                    json!({}),
                    "case {:?}: TagResources on {arn} must fail for nothing, got {answered}",
                    row.case
                );
            }
            if !row.withdrawn.is_empty() {
                let answered = tagging_ok(
                    &router,
                    &runtime,
                    row.region,
                    "UntagResources",
                    json!({ "ResourceARNList": [&arn], "TagKeys": row.withdrawn }),
                );
                assert_eq!(
                    answered["FailedResourcesMap"],
                    json!({}),
                    "case {:?}: UntagResources on {arn} must fail for nothing, got {answered}",
                    row.case
                );
            }

            // Read once, not in a loop: the call this API answered 200 for is the call that
            // edited the bucket's own tag set, so nothing about the set is still owed by the
            // time that answer arrives — a set this read does not report is a set the write
            // did not reach, which is the whole subject here.
            let want = pairs(row.settled);
            let seen = bucket_tagging(&router, &runtime, row.region, row.bucket);

            if want.is_empty() {
                // Nothing is left on the bucket, which is the ONE condition that refusal
                // reports: its code, its status and the sentence S3 publishes for it.
                assert_eq!(
                    (seen.status, seen.code.as_str(), seen.message.as_str()),
                    (404, NO_SUCH_TAG_SET, NO_SUCH_TAG_SET_MESSAGE),
                    "case {:?}: expected GetBucketTagging to refuse an empty tag set, got \
                     {} / {:?} / {:?}",
                    row.case,
                    seen.status,
                    seen.code,
                    seen.message
                );
            } else {
                assert_eq!(
                    (seen.status, &seen.tags),
                    (200, &want),
                    "case {:?}: expected GetBucketTagging to report {want:?}, got {} / {:?}",
                    row.case,
                    seen.status,
                    seen.tags
                );
            }
        }
    });
}

/// ONE batch addressing two services writes each ARN's tags where that service keeps them,
/// and an ARN whose owner holds nothing is reported per RESOURCE — with that service's own
/// code and status — while the rest of the batch is served. Afterwards each owner's own read
/// reports the settled set, and so does the account's tagged-resource view.
#[test]
fn one_batch_reaches_a_bucket_and_a_table_and_names_what_it_could_not_tag() {
    on_a_sized_thread("tagging-facade-batch", || {
        let router = router();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");

        let bucket = "facade-batch-bucket";
        let seeded = pairs(&[("keep", "kept"), ("drop", "dropped")]);
        create_bucket(&router, &runtime, REGION, bucket);
        put_bucket_tagging(&router, &runtime, REGION, bucket, &seeded);
        let bucket = bucket_arn(bucket);
        let table = create_table(&router, &runtime, REGION, "facade-batch-table", &seeded);

        // An ARN of the other owner naming a resource that was never created: ITS own
        // not-found identity, per resource, with the live ARNs of the same batch written.
        let ghost = table_arn(REGION, "facade-batch-ghost");
        let answered = tagging_ok(
            &router,
            &runtime,
            REGION,
            "TagResources",
            json!({
                "ResourceARNList": [&bucket, &table, &ghost],
                "Tags": { "added": "new" },
            }),
        );
        let failures = answered["FailedResourcesMap"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            failures.keys().cloned().collect::<Vec<String>>(),
            vec![ghost.clone()],
            "case a batch carrying an absent resource: expected only {ghost} to fail, got \
             {answered}"
        );
        assert_eq!(
            (
                failures[&ghost]["ErrorCode"].clone(),
                failures[&ghost]["StatusCode"].clone(),
                failures[&ghost]["ErrorMessage"].clone(),
            ),
            (
                json!("ResourceNotFoundException"),
                json!(400),
                json!(format!("Requested resource not found: ResourceArn: {ghost} not found")),
            ),
            "case a batch carrying an absent resource: expected that service's own failure \
             for {ghost}, got {answered}"
        );

        let answered = tagging_ok(
            &router,
            &runtime,
            REGION,
            "UntagResources",
            json!({ "ResourceARNList": [&bucket, &table], "TagKeys": ["drop"] }),
        );
        assert_eq!(
            answered["FailedResourcesMap"],
            json!({}),
            "case a batch over both services: UntagResources must fail for nothing, got \
             {answered}"
        );

        // Each owner reports the settled set out of its OWN tag plane.
        let want = pairs(&[("added", "new"), ("keep", "kept")]);
        let settled = bucket_tagging(&router, &runtime, REGION, "facade-batch-bucket");
        assert_eq!(
            settled.tags, want,
            "case a batch over both services: expected GetBucketTagging to report {want:?}, \
             got {} / {:?} / {:?}",
            settled.status, settled.tags, settled.code
        );
        // A DynamoDB tag change is eventually consistent, so that owner's read is owed
        // rather than immediate: the condition is looked for rather than waited out, and the
        // last answer seen is what the assertion below names.
        let mut listed = BTreeMap::new();
        poll_until(
            "a batch over both services",
            "the table's own tag set to report what the batch wrote",
            || {
                listed = table_tags(&router, &runtime, REGION, &table);
                listed == want
            },
        );
        assert_eq!(
            listed, want,
            "case a batch over both services: expected ListTagsOfResource to report \
             {want:?}, got {listed:?}"
        );

        // And the tagged-resource view reports both resources with that same set, because
        // there is one tag set per resource and this is the view over them.
        let expected: BTreeMap<String, BTreeMap<String, String>> =
            [(bucket.clone(), want.clone()), (table.clone(), want.clone())]
                .into_iter()
                .collect();
        let mut seen = BTreeMap::new();
        poll_until(
            "a batch over both services",
            "the tagged-resource view to report both resources",
            || {
                seen = discovered(&tagging_ok(
                    &router,
                    &runtime,
                    REGION,
                    "GetResources",
                    json!({ "ResourceARNList": [&bucket, &table] }),
                ));
                seen == expected
            },
        );
        assert_eq!(
            seen, expected,
            "case a batch over both services: expected GetResources to report {expected:?}, \
             got {seen:?}"
        );

        // The absent resource was brought into being by nothing: it is still absent, and the
        // view does not report it.
        let answer = tagging_ok(
            &router,
            &runtime,
            REGION,
            "GetResources",
            json!({ "ResourceARNList": [&ghost] }),
        );
        let seen = discovered(&answer);
        assert!(
            seen.is_empty(),
            "case a batch carrying an absent resource: expected the view to report nothing \
             for {ghost}, got {seen:?}"
        );
    });
}
