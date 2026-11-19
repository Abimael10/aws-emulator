// SPDX-License-Identifier: CC0-1.0

//! WHERE a flow run's output object lands in the object store, and what a listing of it
//! reports.
//!
//! The rules under test:
//!
//! * a run of a flow with an Amazon S3 destination stores its output object at the
//!   destination prefix, then the flow name, then the run's own id — three path elements,
//!   the execution id LAST, with no further segment below it and no file-name extension —
//!   so a client that composes the key the documented way (the configured prefix, the flow
//!   name and the `executionId` `StartFlow` returned) reads the run's output, and a
//!   prefix/delimiter walk of the flow's prefix reports the object itself rather than a
//!   directory level above it; a destination naming no prefix writes the same two
//!   remaining elements at the top of the bucket;
//! * a run writes that object WHOLE and names no checksum algorithm, so the object store
//!   records the checksum Amazon S3 adds to such a write — its default algorithm, over all
//!   of the bytes — and `ListObjectsV2` reports that algorithm (`Object$ChecksumAlgorithm`,
//!   "the algorithm that was used to create a checksum of the object") and
//!   `FULL_OBJECT` (`Object$ChecksumType`) for the entry, with the value a `PutObject` of
//!   the same bytes reports;
//! * the bytes it stores present each transferred record's fields in the order the SOURCE
//!   record presented them — a transfer hands the destination the records it read, so
//!   nothing between the read and the write re-orders a record's fields, whichever file
//!   type the destination declares;
//! * that object is stored with KMS server-side encryption under the flow's own encryption
//!   key — the key its definition named, or the key AppFlow manages for the account when
//!   the definition named none — so a read of it reports `aws:kms` together with the key id
//!   `DescribeFlow` reports for the flow.
//!
//! Both services are mounted on the ONE shared engine the router mounts them on, so every
//! assertion is about what a client sees on the wire: the AppFlow calls as the `restJson1`
//! requests a client sends, the destination reads as the path-style Amazon S3 requests a
//! client sends.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use crate::flow_lifecycle::{
    appflow_bucket_policy, appflow_with_engine, call, map_task, on_demand_trigger, settled_runs,
    EVERY_RUN_ACTION, FIXTURE_ACCESS_KEY_ID, REGION,
};

/// The `ChecksumAlgorithm` Amazon S3 itself adds to a write that names none: a checksum
/// member "is present if the object was uploaded with the `CRC64NVME` checksum algorithm,
/// or if it was uploaded without a checksum (and Amazon S3 added the default checksum,
/// `CRC64NVME`, to the uploaded object)" (`PutObjectOutput$ChecksumCRC64NVME`).
const DEFAULT_CHECKSUM_ALGORITHM: &str = "CRC64NVME";

/// The `ChecksumType` a single whole-object write settles — one of the two values the
/// member publishes (`smithy_query(query="shape", shape="ChecksumType")` lists `COMPOSITE`
/// and `FULL_OBJECT`), and the one a write of all the bytes at once carries ("for
/// `PutObject` uploads, the checksum type is always `FULL_OBJECT`",
/// `PutObjectRequest$ChecksumType`).
const FULL_OBJECT: &str = "FULL_OBJECT";

/// The response header the default algorithm's value travels on: `x-amz-checksum-`
/// followed by the algorithm in lower case (`ChecksumCRC64NVME` ->
/// `x-amz-checksum-crc64nvme`).
const DEFAULT_CHECKSUM_HEADER: &str = "x-amz-checksum-crc64nvme";

/// What an Amazon S3 request answered: the status, the response headers and the body.
struct S3Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl S3Answer {
    /// The body as text, which is what every XML answer here is read out of.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// AppFlow and Amazon S3 on ONE engine, so what a flow run writes is what the object
/// store then serves.
fn appflow_and_object_store() -> MultiServiceRouter {
    let (mut router, engine) = appflow_with_engine();
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// One request signed (in the routing sense) for the object store.
async fn s3_request(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> S3Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://s3.{REGION}.amazonaws.com{path}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={FIXTURE_ACCESS_KEY_ID}/20260101/{REGION}/s3/\
                 aws4_request, SignedHeaders=host, Signature=unverified"
            ),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("the request builds");
    let (parts, body) = router.dispatch(request).await.into_parts();
    S3Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// `PUT /<bucket>` and `PUT /<bucket>?policy` — the bucket a flow reads or writes, with
/// the policy AppFlow asks a caller to place on it.
async fn stand_up_bucket(router: &MultiServiceRouter, bucket: &str) {
    let created = s3_request(router, "PUT", &format!("/{bucket}"), &[], b"").await;
    assert_eq!(
        created.status,
        200,
        "expected CreateBucket of {bucket} to succeed, got {} {:?}",
        created.status,
        created.text()
    );
    let policy = appflow_bucket_policy(bucket, &EVERY_RUN_ACTION).to_string();
    let placed = s3_request(
        router,
        "PUT",
        &format!("/{bucket}?policy"),
        &[],
        policy.as_bytes(),
    )
    .await;
    assert_eq!(
        placed.status,
        204,
        "expected PutBucketPolicy on {bucket} to succeed, got {} {:?}",
        placed.status,
        placed.text()
    );
}

/// `GET /<bucket>?list-type=2&...` — `ListObjectsV2`, with the query the case asks for.
async fn list_objects(router: &MultiServiceRouter, bucket: &str, query: &str) -> S3Answer {
    s3_request(
        router,
        "GET",
        &format!("/{bucket}?list-type=2{query}"),
        &[],
        b"",
    )
    .await
}

/// Every value `<name>` carries in `document`, in document order.
fn elements(document: &str, name: &str) -> Vec<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let mut found = Vec::new();
    let mut rest = document;
    while let Some((_, tail)) = rest.split_once(open.as_str()) {
        match tail.split_once(close.as_str()) {
            Some((value, remainder)) => {
                found.push(value.to_string());
                rest = remainder;
            }
            None => break,
        }
    }
    found
}

/// One row of the output-key table.
struct Layout {
    case: &'static str,
    /// The `bucketPrefix` the destination names, or `None` for a destination naming none.
    bucket_prefix: Option<&'static str>,
    /// The `fileType` the destination's output format config names, or `None` for a
    /// destination naming none.
    file_type: Option<&'static str>,
    /// The bytes the source location holds.
    source: &'static str,
    /// The key elements the output object is expected to sit under, ahead of the flow name
    /// and the execution id — empty for a destination that names no prefix.
    expected_parent: &'static str,
}

/// The table. The rows differ along everything the key rule generalises over: whether the
/// destination names a prefix at all, how many elements that prefix has, whether it is
/// written with a trailing separator, and which file type the run writes — none of which
/// changes the three-element layout.
fn layouts() -> Vec<Layout> {
    vec![
        Layout {
            case: "a destination naming no prefix",
            bucket_prefix: None,
            file_type: None,
            source: r#"[{"id":"1"},{"id":"2"}]"#,
            expected_parent: "",
        },
        Layout {
            case: "a one-element destination prefix",
            bucket_prefix: Some("output"),
            file_type: Some("JSON"),
            source: r#"[{"id":"1"}]"#,
            expected_parent: "output",
        },
        Layout {
            case: "a destination prefix written with a trailing separator",
            bucket_prefix: Some("output/"),
            file_type: Some("JSON"),
            source: r#"[{"id":"1"}]"#,
            expected_parent: "output",
        },
        Layout {
            case: "a nested destination prefix",
            bucket_prefix: Some("archive/raw/2026"),
            file_type: None,
            source: r#"[{"id":"1"},{"id":"2"},{"id":"3"}]"#,
            expected_parent: "archive/raw/2026",
        },
        Layout {
            case: "a CSV destination",
            bucket_prefix: Some("output"),
            file_type: Some("CSV"),
            source: r#"[{"id":"1"},{"id":"2"}]"#,
            expected_parent: "output",
        },
        Layout {
            case: "a PARQUET destination",
            bucket_prefix: Some("output"),
            file_type: Some("PARQUET"),
            source: r#"[{"id":"1"}]"#,
            expected_parent: "output",
        },
    ]
}

/// An Amazon S3 destination naming `bucket`, the prefix the row configures and the file
/// type it configures.
fn destination(bucket: &str, bucket_prefix: Option<&str>, file_type: Option<&str>) -> Value {
    let mut properties = json!({ "bucketName": bucket });
    if let Some(prefix) = bucket_prefix {
        properties["bucketPrefix"] = json!(prefix);
    }
    if let Some(file_type) = file_type {
        properties["s3OutputFormatConfig"] = json!({ "fileType": file_type });
    }
    json!({
        "connectorType": "S3",
        "destinationConnectorProperties": { "S3": properties }
    })
}

#[tokio::test]
async fn a_run_stores_its_output_at_the_prefix_flow_and_execution_id() {
    for (index, row) in layouts().into_iter().enumerate() {
        let case = row.case;
        let router = appflow_and_object_store();
        let source_bucket = "run-output-source";
        let destination_bucket = "run-output-destination";
        stand_up_bucket(&router, source_bucket).await;
        stand_up_bucket(&router, destination_bucket).await;
        let written = s3_request(
            &router,
            "PUT",
            &format!("/{source_bucket}/inbound/orders.json"),
            &[],
            row.source.as_bytes(),
        )
        .await;
        assert_eq!(
            written.status,
            200,
            "case {case:?}: expected the source object to be stored, got {} {:?}",
            written.status,
            written.text()
        );

        let flow_name = format!("run-output-flow-{index}");
        let created = call(
            &router,
            "/create-flow",
            &json!({
                "flowName": flow_name,
                "triggerConfig": on_demand_trigger(),
                "sourceFlowConfig": {
                    "connectorType": "S3",
                    "sourceConnectorProperties": {
                        "S3": { "bucketName": source_bucket, "bucketPrefix": "inbound" }
                    }
                },
                "destinationFlowConfigList": [destination(
                    destination_bucket, row.bucket_prefix, row.file_type)],
                "tasks": [map_task("id", "id")],
            }),
        )
        .await;
        assert!(
            created.is_ok(),
            "case {case:?}: expected CreateFlow to succeed, got {} {:?}",
            created.status,
            created.body
        );
        let started = call(&router, "/start-flow", &json!({ "flowName": flow_name })).await;
        assert!(
            started.is_ok(),
            "case {case:?}: expected StartFlow to succeed, got {} {:?}",
            started.status,
            started.body
        );
        let execution_id = started.string("executionId").to_string();
        assert!(
            !execution_id.is_empty(),
            "case {case:?}: expected StartFlow to report the run's id, got {:?}",
            started.body
        );
        let runs = settled_runs(&router, &flow_name).await;
        assert_eq!(
            runs.first()
                .and_then(|run| run.get("executionStatus"))
                .and_then(Value::as_str),
            Some("Successful"),
            "case {case:?}: expected the run to succeed, got {runs:?}"
        );

        // The key a client composes the documented way: the configured prefix, the flow
        // name, the execution id — and nothing else.
        let flow_prefix = if row.expected_parent.is_empty() {
            flow_name.clone()
        } else {
            format!("{}/{flow_name}", row.expected_parent)
        };
        let expected_key = format!("{flow_prefix}/{execution_id}");

        // 1. The listing of the flow's prefix reports exactly that one key.
        let listed = list_objects(&router, destination_bucket, &format!("&prefix={flow_prefix}"))
            .await;
        assert_eq!(
            listed.status,
            200,
            "case {case:?}: expected ListObjectsV2 to succeed, got {} {:?}",
            listed.status,
            listed.text()
        );
        let listing = listed.text();
        assert_eq!(
            elements(&listing, "Key"),
            vec![expected_key.clone()],
            "case {case:?}: expected the run's output listed at {expected_key:?} and nothing \
             else, got body {listing:?}"
        );
        assert_eq!(
            elements(&listing, "KeyCount"),
            vec!["1".to_string()],
            "case {case:?}: expected one listed key, got body {listing:?}"
        );

        // 2. And that entry carries the default full-object checksum the write settled.
        assert_eq!(
            elements(&listing, "ChecksumAlgorithm"),
            vec![DEFAULT_CHECKSUM_ALGORITHM.to_string()],
            "case {case:?}: expected the entry to name {DEFAULT_CHECKSUM_ALGORITHM:?}, got body \
             {listing:?}"
        );
        assert_eq!(
            elements(&listing, "ChecksumType"),
            vec![FULL_OBJECT.to_string()],
            "case {case:?}: expected the entry to report {FULL_OBJECT:?}, got body {listing:?}"
        );

        // 3. A delimiter walk of the flow's prefix reports the object ITSELF: there is no
        //    directory level below the flow name, so no common prefix is rolled up.
        let walked = list_objects(
            &router,
            destination_bucket,
            &format!("&prefix={flow_prefix}/&delimiter=%2F"),
        )
        .await;
        let walk = walked.text();
        assert_eq!(
            elements(&walk, "Key"),
            vec![expected_key.clone()],
            "case {case:?}: expected the walk to report the object itself, got body {walk:?}"
        );
        assert!(
            elements(&walk, "Prefix")
                .iter()
                .all(|prefix| prefix == &format!("{flow_prefix}/")),
            "case {case:?}: expected no directory level below the flow name, got body {walk:?}"
        );

        // 4. The object is readable AT that key, and the checksum a read reports is the
        //    digest of the bytes it carries: the same value a `PutObject` of those bytes
        //    reports, which is what makes the listing's pair the object's own and not a
        //    constant.
        let enabled = [("x-amz-checksum-mode", "ENABLED")];
        let read = s3_request(
            &router,
            "GET",
            &format!("/{destination_bucket}/{expected_key}"),
            &enabled,
            b"",
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected GetObject of {expected_key} to be served, got {} {:?}",
            read.status,
            read.text()
        );
        assert!(
            !read.body.is_empty(),
            "case {case:?}: expected the run's output to carry the records it moved, got an \
             empty object"
        );
        let mirrored = s3_request(
            &router,
            "PUT",
            &format!("/{destination_bucket}/mirror/{index}"),
            &[],
            &read.body,
        )
        .await;
        assert_eq!(
            mirrored.status,
            200,
            "case {case:?}: expected the mirror write to succeed, got {} {:?}",
            mirrored.status,
            mirrored.text()
        );
        assert_eq!(
            read.header(DEFAULT_CHECKSUM_HEADER),
            mirrored.header(DEFAULT_CHECKSUM_HEADER),
            "case {case:?}: expected the run's object to carry the digest of its own bytes, got \
             {:?} against {:?}",
            read.header(DEFAULT_CHECKSUM_HEADER),
            mirrored.header(DEFAULT_CHECKSUM_HEADER)
        );
        assert_eq!(
            read.header("x-amz-checksum-type").as_deref(),
            Some(FULL_OBJECT),
            "case {case:?}: expected a whole-object checksum, got {:?}",
            read.header("x-amz-checksum-type")
        );
    }
}

/// Two runs of ONE flow are two objects, each at its own execution id under the same flow
/// prefix: the run id is the last key element, so neither overwrites the other and a
/// listing of the flow's prefix reports both, each with its own checksum pair.
#[tokio::test]
async fn each_run_of_a_flow_is_stored_at_its_own_execution_id() {
    let router = appflow_and_object_store();
    let source_bucket = "repeat-run-source";
    let destination_bucket = "repeat-run-destination";
    stand_up_bucket(&router, source_bucket).await;
    stand_up_bucket(&router, destination_bucket).await;
    let seeded = s3_request(
        &router,
        "PUT",
        &format!("/{source_bucket}/inbound/orders.json"),
        &[],
        br#"[{"id":"1"}]"#,
    )
    .await;
    assert_eq!(
        seeded.status,
        200,
        "expected the source object to be stored, got {} {:?}",
        seeded.status,
        seeded.text()
    );

    let flow_name = "repeat-run-flow";
    let created = call(
        &router,
        "/create-flow",
        &json!({
            "flowName": flow_name,
            "triggerConfig": on_demand_trigger(),
            "sourceFlowConfig": {
                "connectorType": "S3",
                "sourceConnectorProperties": {
                    "S3": { "bucketName": source_bucket, "bucketPrefix": "inbound" }
                }
            },
            "destinationFlowConfigList": [destination(destination_bucket, Some("output"), None)],
            "tasks": [map_task("id", "id")],
        }),
    )
    .await;
    assert!(
        created.is_ok(),
        "expected CreateFlow to succeed, got {} {:?}",
        created.status,
        created.body
    );

    let mut expected_keys = Vec::new();
    for run in ["the first run", "the second run"] {
        let started = call(&router, "/start-flow", &json!({ "flowName": flow_name })).await;
        assert!(
            started.is_ok(),
            "case {run:?}: expected StartFlow to succeed, got {} {:?}",
            started.status,
            started.body
        );
        let execution_id = started.string("executionId").to_string();
        let runs = settled_runs(&router, flow_name).await;
        assert_eq!(
            runs.first()
                .and_then(|record| record.get("executionStatus"))
                .and_then(Value::as_str),
            Some("Successful"),
            "case {run:?}: expected the run to succeed, got {runs:?}"
        );
        expected_keys.push(format!("output/{flow_name}/{execution_id}"));
    }
    expected_keys.sort();

    let listed = list_objects(
        &router,
        destination_bucket,
        &format!("&prefix=output/{flow_name}"),
    )
    .await;
    let listing = listed.text();
    assert_eq!(
        elements(&listing, "Key"),
        expected_keys,
        "expected one object per run, each at its own execution id, got body {listing:?}"
    );
    assert_eq!(
        elements(&listing, "ChecksumAlgorithm"),
        vec![DEFAULT_CHECKSUM_ALGORITHM.to_string(); expected_keys.len()],
        "expected every listed run output to name {DEFAULT_CHECKSUM_ALGORITHM:?}, got body \
         {listing:?}"
    );
    assert_eq!(
        elements(&listing, "ChecksumType"),
        vec![FULL_OBJECT.to_string(); expected_keys.len()],
        "expected every listed run output to report {FULL_OBJECT:?}, got body {listing:?}"
    );
}

/// The destination prefix every case below writes under.
const OUTPUT_PREFIX: &str = "output";

/// The source prefix every case below reads from.
const SOURCE_PREFIX: &str = "inbound";

/// The `ServerSideEncryption` value an object encrypted under a KMS key reports: one of the
/// values the enum publishes (`smithy_query(query="shape",
/// shape="com.amazonaws.s3#ServerSideEncryption")` lists `AES256`, `aws:kms` and
/// `aws:kms:dsse` among them), and the single-layer one of its two KMS values.
const SSE_KMS: &str = "aws:kms";

/// The response header `GetObject`'s `SSEKMSKeyId` travels on
/// (`@httpHeader("x-amz-server-side-encryption-aws-kms-key-id")`).
const SSE_KMS_KEY_ID_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// The response header `GetObject`'s `ServerSideEncryption` travels on
/// (`@httpHeader("x-amz-server-side-encryption")`).
const SSE_HEADER: &str = "x-amz-server-side-encryption";

/// A `Map_all` task: every field a record carries is written, and none is excluded.
fn map_all_task() -> Value {
    json!({ "taskType": "Map_all", "sourceFields": [] })
}

/// An Amazon S3 source naming `bucket`, the source prefix and the input file type the case
/// declares (`None` for a source declaring none).
fn source(bucket: &str, input_file_type: Option<&str>) -> Value {
    let mut properties = json!({ "bucketName": bucket, "bucketPrefix": SOURCE_PREFIX });
    if let Some(file_type) = input_file_type {
        properties["s3InputFormatConfig"] = json!({ "s3InputFileType": file_type });
    }
    json!({
        "connectorType": "S3",
        "sourceConnectorProperties": { "S3": properties }
    })
}

/// The flow one case defines and runs: the variable parts of its definition.
struct Run<'case> {
    case: &'case str,
    flow_name: &'case str,
    source_bucket: &'case str,
    destination_bucket: &'case str,
    input_file_type: Option<&'case str>,
    output_file_type: Option<&'case str>,
    /// The `kmsArn` the definition names, or `None` for a definition naming none.
    kms_arn: Option<&'case str>,
    tasks: Value,
}

/// Create the flow `run` describes, start it, wait for its run to settle, and answer the
/// key the run's output object landed at.
async fn stored_output_key(router: &MultiServiceRouter, run: &Run<'_>) -> String {
    let case = run.case;
    let mut definition = json!({
        "flowName": run.flow_name,
        "triggerConfig": on_demand_trigger(),
        "sourceFlowConfig": source(run.source_bucket, run.input_file_type),
        "destinationFlowConfigList": [destination(
            run.destination_bucket, Some(OUTPUT_PREFIX), run.output_file_type)],
        "tasks": run.tasks,
    });
    if let Some(kms_arn) = run.kms_arn {
        definition["kmsArn"] = json!(kms_arn);
    }
    let created = call(router, "/create-flow", &definition).await;
    assert!(
        created.is_ok(),
        "case {case:?}: expected CreateFlow to succeed, got {} {:?}",
        created.status,
        created.body
    );
    let started = call(router, "/start-flow", &json!({ "flowName": run.flow_name })).await;
    assert!(
        started.is_ok(),
        "case {case:?}: expected StartFlow to succeed, got {} {:?}",
        started.status,
        started.body
    );
    let execution_id = started.string("executionId").to_string();
    let runs = settled_runs(router, run.flow_name).await;
    assert_eq!(
        runs.first()
            .and_then(|record| record.get("executionStatus"))
            .and_then(Value::as_str),
        Some("Successful"),
        "case {case:?}: expected the run to succeed, got {runs:?}"
    );
    format!("{OUTPUT_PREFIX}/{}/{execution_id}", run.flow_name)
}

/// One row of the field-order table.
struct Order {
    case: &'static str,
    /// The bytes the one source object holds.
    source: &'static str,
    input_file_type: Option<&'static str>,
    output_file_type: Option<&'static str>,
    tasks: Value,
    /// The bytes the destination object is expected to hold, exactly.
    expected: &'static str,
}

/// The field-order table. The rows differ along everything the rule generalises over: the
/// shape of the source document (an array, one record per line, a single record), the
/// source and destination file types, how many records there are and how many fields each
/// carries, whether a record's fields are in alphabetical order to begin with, whether two
/// records carry their fields in the SAME order, whether a field is renamed on the way, and
/// whether a later record adds a field the first did not carry.
fn orders() -> Vec<Order> {
    vec![
        Order {
            case: "a json array whose records carry their fields out of alphabetical order",
            source: r#"[{"id":"1","city":"lisbon","temp":"21"},{"id":"2","city":"oslo","temp":"4"}]"#,
            input_file_type: Some("JSON"),
            output_file_type: Some("JSON"),
            tasks: json!([map_all_task()]),
            expected: "{\"id\":\"1\",\"city\":\"lisbon\",\"temp\":\"21\"}\n\
                       {\"id\":\"2\",\"city\":\"oslo\",\"temp\":\"4\"}\n",
        },
        Order {
            case: "one record per line, fields in reverse alphabetical order",
            source: "{\"z\":\"1\",\"m\":\"2\",\"a\":\"3\"}\n{\"z\":\"4\",\"m\":\"5\",\"a\":\"6\"}\n",
            input_file_type: None,
            output_file_type: None,
            tasks: json!([map_all_task()]),
            expected: "{\"z\":\"1\",\"m\":\"2\",\"a\":\"3\"}\n{\"z\":\"4\",\"m\":\"5\",\"a\":\"6\"}\n",
        },
        Order {
            case: "a single record whose values are not all strings",
            source: r#"{"name":"ada","amount":42,"paid":true}"#,
            input_file_type: None,
            output_file_type: None,
            tasks: json!([map_all_task()]),
            expected: "{\"name\":\"ada\",\"amount\":42,\"paid\":true}\n",
        },
        Order {
            case: "two records that carry their fields in different orders",
            source: r#"[{"b":"1","a":"2"},{"a":"3","b":"4"}]"#,
            input_file_type: None,
            output_file_type: None,
            tasks: json!([map_all_task()]),
            expected: "{\"b\":\"1\",\"a\":\"2\"}\n{\"a\":\"3\",\"b\":\"4\"}\n",
        },
        Order {
            case: "a renamed field keeps the position the source record gave it",
            source: r#"[{"id":"1","city":"lisbon","temp":"21"}]"#,
            input_file_type: None,
            output_file_type: None,
            tasks: json!([map_all_task(), map_task("city", "town")]),
            expected: "{\"id\":\"1\",\"town\":\"lisbon\",\"temp\":\"21\"}\n",
        },
        Order {
            case: "a csv source read into a json destination",
            source: "temp,city,id\n21,lisbon,1\n4,oslo,2\n",
            input_file_type: Some("CSV"),
            output_file_type: Some("JSON"),
            tasks: json!([map_all_task()]),
            expected: "{\"temp\":\"21\",\"city\":\"lisbon\",\"id\":\"1\"}\n\
                       {\"temp\":\"4\",\"city\":\"oslo\",\"id\":\"2\"}\n",
        },
        Order {
            case: "a csv destination takes its columns in the order the records carry them",
            source: r#"[{"temp":"21","city":"lisbon","id":"1"}]"#,
            input_file_type: None,
            output_file_type: Some("CSV"),
            tasks: json!([map_all_task()]),
            expected: "temp,city,id\n21,lisbon,1\n",
        },
        Order {
            case: "a later record adds a field the first did not carry",
            source: r#"[{"b":"1"},{"b":"2","a":"3"}]"#,
            input_file_type: None,
            output_file_type: Some("CSV"),
            tasks: json!([map_all_task()]),
            expected: "b,a\n1,\n2,3\n",
        },
    ]
}

/// The destination object holds each transferred record with its fields in the order the
/// SOURCE record presented them: a transfer hands on the records it read, so a client
/// comparing the destination bytes (or a digest of them) against the source sees what AWS
/// produces rather than an alphabetised re-serialization.
#[tokio::test]
async fn the_output_object_keeps_each_records_source_field_order() {
    for (index, row) in orders().into_iter().enumerate() {
        let case = row.case;
        let router = appflow_and_object_store();
        let source_bucket = "field-order-source";
        let destination_bucket = "field-order-destination";
        stand_up_bucket(&router, source_bucket).await;
        stand_up_bucket(&router, destination_bucket).await;
        let seeded = s3_request(
            &router,
            "PUT",
            &format!("/{source_bucket}/{SOURCE_PREFIX}/records"),
            &[],
            row.source.as_bytes(),
        )
        .await;
        assert_eq!(
            seeded.status,
            200,
            "case {case:?}: expected the source object to be stored, got {} {:?}",
            seeded.status,
            seeded.text()
        );

        let flow_name = format!("field-order-flow-{index}");
        let key = stored_output_key(
            &router,
            &Run {
                case,
                flow_name: &flow_name,
                source_bucket,
                destination_bucket,
                input_file_type: row.input_file_type,
                output_file_type: row.output_file_type,
                kms_arn: None,
                tasks: row.tasks,
            },
        )
        .await;

        let read = s3_request(
            &router,
            "GET",
            &format!("/{destination_bucket}/{key}"),
            &[],
            b"",
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected GetObject of {key} to be served, got {} {:?}",
            read.status,
            read.text()
        );
        let served = String::from_utf8_lossy(&read.body).to_string();
        assert_eq!(
            served, row.expected,
            "case {case:?}: expected the destination object to hold {:?}, got {served:?}",
            row.expected
        );
    }
}

/// One row of the destination-encryption table.
struct Encrypted {
    case: &'static str,
    /// The `kmsArn` the flow's definition names, or `None` for a definition naming none —
    /// the case AppFlow settles with the key it manages for the account.
    kms_arn: Option<&'static str>,
}

/// The encryption table. The rows differ along what the rule generalises over: whether the
/// definition names a key at all, and which key it names — never which value is reported,
/// because the reported key is the flow's own.
fn encrypted() -> Vec<Encrypted> {
    vec![
        Encrypted {
            case: "a flow naming no key, so AppFlow's own managed key encrypts the output",
            kms_arn: None,
        },
        Encrypted {
            case: "a flow naming a key of its own",
            kms_arn: Some("arn:aws:kms:us-east-1:000000000000:key/11111111-2222-3333-4444-555555555555"),
        },
        Encrypted {
            case: "a flow naming another key, so two flows report different keys",
            kms_arn: Some("arn:aws:kms:us-east-1:000000000000:key/66666666-7777-8888-9999-aaaaaaaaaaaa"),
        },
    ]
}

/// A run's output object is stored with KMS server-side encryption under the flow's own
/// encryption key, so a read of it reports `aws:kms` together with that key — the one
/// `DescribeFlow` reports for the flow — rather than the bucket's SSE-S3 default with no key
/// at all, and reports it to every reader of the object.
#[tokio::test]
async fn the_output_object_is_encrypted_under_the_flows_own_kms_key() {
    for (index, row) in encrypted().into_iter().enumerate() {
        let case = row.case;
        let router = appflow_and_object_store();
        let source_bucket = "encrypted-output-source";
        let destination_bucket = "encrypted-output-destination";
        stand_up_bucket(&router, source_bucket).await;
        stand_up_bucket(&router, destination_bucket).await;
        let seeded = s3_request(
            &router,
            "PUT",
            &format!("/{source_bucket}/{SOURCE_PREFIX}/records"),
            &[],
            br#"[{"id":"1","city":"lisbon"}]"#,
        )
        .await;
        assert_eq!(
            seeded.status,
            200,
            "case {case:?}: expected the source object to be stored, got {} {:?}",
            seeded.status,
            seeded.text()
        );

        let flow_name = format!("encrypted-output-flow-{index}");
        let key = stored_output_key(
            &router,
            &Run {
                case,
                flow_name: &flow_name,
                source_bucket,
                destination_bucket,
                input_file_type: None,
                output_file_type: None,
                kms_arn: row.kms_arn,
                tasks: json!([map_all_task()]),
            },
        )
        .await;

        // The key the FLOW records, which is the one its output has to be encrypted under:
        // the definition's own, or the one AppFlow settled for a definition naming none.
        let described = call(&router, "/describe-flow", &json!({ "flowName": flow_name })).await;
        let flow_key = described.string("kmsArn").to_string();
        assert!(
            flow_key.starts_with("arn:aws:kms:"),
            "case {case:?}: expected the flow to record a KMS key, got {:?}",
            described.body
        );
        if let Some(named) = row.kms_arn {
            assert_eq!(
                flow_key, named,
                "case {case:?}: expected the flow to keep the key it named, got {flow_key:?}"
            );
        }

        let read = s3_request(
            &router,
            "GET",
            &format!("/{destination_bucket}/{key}"),
            &[("x-amz-checksum-mode", "ENABLED")],
            b"",
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected GetObject of {key} to be served, got {} {:?}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.header(SSE_HEADER).as_deref(),
            Some(SSE_KMS),
            "case {case:?}: expected the read to report {SSE_KMS:?}, got {:?}",
            read.header(SSE_HEADER)
        );
        assert_eq!(
            read.header(SSE_KMS_KEY_ID_HEADER).as_deref(),
            Some(flow_key.as_str()),
            "case {case:?}: expected the read to report the flow's key {flow_key:?}, got {:?}",
            read.header(SSE_KMS_KEY_ID_HEADER)
        );
        // The write recorded a full-object checksum of the bytes it stored, which the same
        // read reports beside the encryption it was stored under.
        assert!(
            read.header(DEFAULT_CHECKSUM_HEADER)
                .is_some_and(|value| !value.is_empty()),
            "case {case:?}: expected the read to report {DEFAULT_CHECKSUM_HEADER}, got headers \
             {:?}",
            read.headers
        );
        assert_eq!(
            read.header("x-amz-checksum-type").as_deref(),
            Some(FULL_OBJECT),
            "case {case:?}: expected a whole-object checksum, got {:?}",
            read.header("x-amz-checksum-type")
        );

        // The same pair reaches the object's OTHER reader, from the one record the write
        // left behind rather than from anything this read computed.
        let headed = s3_request(
            &router,
            "HEAD",
            &format!("/{destination_bucket}/{key}"),
            &[],
            b"",
        )
        .await;
        assert_eq!(
            headed.status,
            200,
            "case {case:?}: expected HeadObject of {key} to be served, got {}",
            headed.status
        );
        assert_eq!(
            (
                headed.header(SSE_HEADER),
                headed.header(SSE_KMS_KEY_ID_HEADER)
            ),
            (Some(SSE_KMS.to_string()), Some(flow_key.clone())),
            "case {case:?}: expected HeadObject to report {SSE_KMS:?} under {flow_key:?}, got \
             {:?} / {:?}",
            headed.header(SSE_HEADER),
            headed.header(SSE_KMS_KEY_ID_HEADER)
        );
    }
}
