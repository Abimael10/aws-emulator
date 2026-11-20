// SPDX-License-Identifier: CC0-1.0

//! A Firehose stream whose destination is an Amazon OpenSearch Service domain indexes only
//! the documents that domain's index mappings ACCEPT, and parks every document the cluster
//! refused in the destination's Amazon S3 backup.
//!
//! The rule under test, one level above the recorded case: **an index's mappings decide which
//! documents the cluster takes — a field the mappings do not map is refused where that level
//! is `dynamic: strict`, and a value is read into a mapped field only as far as that field's
//! own parameters allow, so a numeric field CLEANS a value up to fit its type (a number read
//! out of a string, a fraction truncated into a whole-number type) only while its `coerce`
//! parameter is on, which it is by default, and a field declaring `coerce: false` takes only a
//! value already of its own type — and a producer delivering into such an index learns that
//! verdict per document: Firehose indexes the documents the cluster took and writes each one
//! it refused to the destination's Amazon S3 backup ("Firehose writes any documents that could
//! not be indexed to the configured Amazon S3 destination, with
//! `AmazonOpenSearchService-failed/` appended to the key prefix", `S3BackupMode`), carrying the
//! record's own bytes and the cluster's reason.**
//!
//! Only a cross-service suite can pin the whole of that: the mappings and the documents belong
//! to Amazon OpenSearch Service, the delivery and its backup to Firehose, and the parked
//! objects to Amazon S3, all on ONE engine exactly as `core/router/src/lib.rs` mounts them.
//!
//! Two readings of the rule, each over a table rather than one call:
//!
//! * through the DOMAIN's own REST data plane, one row per form a value arrives in and per
//!   setting of the parameter, which is where the verdict is taken;
//! * through a Firehose `PutRecordBatch` into a template-created index, one row per way a
//!   document can disagree with those mappings, which is where the verdict becomes a delivery
//!   or a parked record.
//!
//! Nothing here writes into a store to arrange what it then asserts: the domain is created
//! through `CreateDomain`, the mappings through the domain's own template and index APIs, the
//! stream through `CreateDeliveryStream`, the records through `PutRecordBatch`, and every
//! reading is taken through `GET /<index>/_count`, `ListObjectsV2` and `GetObject`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::elements;

/// The Region every call below is signed for, and the one every ARN here names.
const REGION: &str = "us-west-2";

/// The domain both readings are taken on.
const DOMAIN: &str = "mapping-verdict-domain";

/// The engine release that domain runs.
const ENGINE_VERSION: &str = "OpenSearch_2.11";

/// The bucket the stream's backup parks refused documents in.
const BUCKET: &str = "mapping-verdict-backup";

/// The stream the batch is put on, and the index its destination is configured with.
const STREAM: &str = "mapping-verdict-stream";
const INDEX_PREFIX: &str = "quarantine";

/// The folder a document the domain did not index is written under, when the backup
/// configures no `Prefix` and no `ErrorOutputPrefix` of its own ("with
/// `AmazonOpenSearchService-failed/` appended to the key prefix", `S3BackupMode`).
const FAILED_FOLDER: &str = "AmazonOpenSearchService-failed/";

/// How long a phase waits for the condition it is about — the domain reporting itself
/// provisioned, the stream reporting itself `ACTIVE`, the delivery landing.
///
/// Generous on purpose: nothing here asserts on how long anything took, so this is only the
/// point at which waiting is called a failure.
const PHASE_DEADLINE: Duration = Duration::from_secs(60);

/// How long each look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// What a dispatched request answered.
struct Answer {
    status: u16,
    body: Vec<u8>,
}

impl Answer {
    /// The answer's body as JSON, or `Value::Null` when it is not JSON at all.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The answer's body as text, for a message naming what came back.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` — the routing
/// key the router claims a request by. Nothing here is a credential: this router runs with
/// request authentication off, so no signature is verified.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One request to the service `signing_name` serves.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(signing_name));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: response.body().to_vec(),
    }
}

/// One Amazon OpenSearch Service request over its `restJson1` wire: the control plane under
/// `/2021-01-01/opensearch`, and a domain's own REST surface under the prefixed path a client
/// that cannot resolve the published endpoint host reaches it at.
async fn es(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    send(
        router,
        "es",
        method,
        path,
        &[("content-type", "application/json")],
        payload.into_bytes(),
    )
    .await
}

/// One request to a domain's REST data plane.
async fn rest(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Answer {
    es(router, method, &format!("/_aws/es/{DOMAIN}{path}"), body).await
}

/// One Firehose operation over its `awsJson1_1` wire.
async fn firehose(router: &MultiServiceRouter, operation: &str, body: Value) -> Answer {
    send(
        router,
        "firehose",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("Firehose_20150804.{operation}")),
        ],
        body.to_string().into_bytes(),
    )
    .await
}

/// The emulator the suite drives: the three services the flow spans, on ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_opensearch_amazonopensearchservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// `CreateDomain` for a single-node domain, answering the ARN its status published.
async fn given_domain(router: &MultiServiceRouter) -> String {
    let answer = es(
        router,
        "POST",
        "/2021-01-01/opensearch/domain",
        Some(json!({
            "DomainName": DOMAIN,
            "EngineVersion": ENGINE_VERSION,
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        })),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup CreateDomain: expected 200, got {} with {}",
        answer.status,
        answer.text()
    );
    let described = answer.json();
    described["DomainStatus"]["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateDomain: expected an ARN, got {described}"))
        .to_string()
}

/// Wait until `DescribeDomain` reports the domain created with nothing in flight — the
/// condition a client's readiness poll waits for, rather than a span of time.
async fn provisioned(router: &MultiServiceRouter) {
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        let answer = es(
            router,
            "GET",
            &format!("/2021-01-01/opensearch/domain/{DOMAIN}"),
            None,
        )
        .await;
        let described = answer.json();
        let status = &described["DomainStatus"];
        if status["Created"].as_bool() == Some(true) && status["Processing"].as_bool() == Some(false)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup DescribeDomain: the domain never reported itself provisioned, last read \
             {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `PUT /<bucket>` — Amazon S3's `CreateBucket`, naming the Region it is created in.
async fn given_bucket(router: &MultiServiceRouter) {
    let answer = send(
        router,
        "s3",
        "PUT",
        &format!("/{BUCKET}"),
        &[],
        format!(
            "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
        )
        .into_bytes(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup CreateBucket: expected 200, got {} with {}",
        answer.status,
        answer.text()
    );
}

/// Every key the backup bucket holds under `prefix`, read through `ListObjectsV2` with no
/// delimiter — the recursive read a client makes for a prefix whose objects sit under further
/// segments of their own.
async fn keys_under(router: &MultiServiceRouter, prefix: &str) -> Vec<String> {
    let answer = send(
        router,
        "s3",
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "listing {prefix:?}: expected 200, got {} with {}",
        answer.status,
        answer.text()
    );
    let mut keys = elements(&answer.text(), "Key");
    keys.sort();
    keys
}

/// The documents the backup holds under the failed-document folder, one per line of each
/// object there, once it holds at least `want` of them — or a failure naming `case` once
/// [`PHASE_DEADLINE`] has passed.
async fn parked_documents(router: &MultiServiceRouter, case: &str, want: usize) -> Vec<Value> {
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        let mut documents = Vec::new();
        for key in keys_under(router, FAILED_FOLDER).await {
            let answer = send(router, "s3", "GET", &format!("/{BUCKET}/{key}"), &[], Vec::new())
                .await;
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: reading {key:?} must succeed, got {} with {}",
                answer.status,
                answer.text()
            );
            for line in answer.text().lines().filter(|line| !line.trim().is_empty()) {
                documents.push(serde_json::from_str(line).unwrap_or_else(|error| {
                    panic!("case {case:?}: a parked document must be JSON: {error} in {line:?}")
                }));
            }
        }
        if documents.len() >= want {
            return documents;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected at least {want} parked document(s) under {FAILED_FOLDER}, \
             got {documents:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The documents `index` holds, counted through the domain's own REST endpoint.
async fn counted(router: &MultiServiceRouter, case: &str, index: &str) -> i64 {
    let answer = rest(router, "GET", &format!("/{index}/_count"), None).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected the count to be served, got {} with {}",
        answer.status,
        answer.text()
    );
    answer.json()["count"].as_i64().unwrap_or(-1)
}

/// The mappings the index the delivery lands in is created with, written as the index template
/// a client installs before the producer starts: nothing but the three fields it declares may
/// be introduced, and the whole-number field reads no value it would have to clean up.
fn strict_template() -> Value {
    json!({
        "index_patterns": [format!("{INDEX_PREFIX}-*")],
        "priority": 100,
        "template": {
            "mappings": {
                "dynamic": "strict",
                "properties": {
                    "seq": { "type": "integer" },
                    "label": { "type": "keyword" },
                    "qty": { "type": "integer", "coerce": false },
                },
            },
        },
    })
}

/// One record of the batch: the document it carries and whether the index's mappings take it.
struct Record {
    case: &'static str,
    document: Value,
    indexed: bool,
    /// The field the cluster's reason names, for a document the mappings refuse.
    names: &'static str,
}

/// The batch the producer puts: documents the mappings take, and one per way a document can
/// disagree with them.
fn records() -> Vec<Record> {
    vec![
        Record {
            case: "a document of the mapped fields",
            document: json!({ "seq": 1, "label": "alpha", "qty": 3 }),
            indexed: true,
            names: "",
        },
        Record {
            case: "a document leaving a mapped field out",
            document: json!({ "seq": 2, "label": "beta" }),
            indexed: true,
            names: "",
        },
        Record {
            case: "a document whose whole-number field carries a whole number",
            document: json!({ "seq": 3, "label": "gamma", "qty": 0 }),
            indexed: true,
            names: "",
        },
        Record {
            case: "a numeric string for a field that does not coerce",
            document: json!({ "seq": 100, "label": "bad-coerce", "qty": "12" }),
            indexed: false,
            names: "[qty]",
        },
        Record {
            case: "a fraction for a whole-number field that does not coerce",
            document: json!({ "seq": 101, "label": "bad-fraction", "qty": 1.5 }),
            indexed: false,
            names: "[qty]",
        },
        Record {
            case: "an object for a field mapped whole",
            document: json!({ "seq": 102, "label": { "not": "a-keyword" }, "qty": 1 }),
            indexed: false,
            names: "[label]",
        },
        Record {
            case: "a field the strict mappings do not map",
            document: json!({ "seq": 103, "label": "extra", "qty": 1, "unmapped": "x" }),
            indexed: false,
            names: "[unmapped]",
        },
    ]
}

/// `-####-##-##`: the shape of the suffix `OneDay` rotation appends, with `#` standing for a
/// digit — the calendar fields the period names, asserted as a shape rather than recomputed.
fn has_day_suffix(index: &str) -> bool {
    let Some(suffix) = index.strip_prefix(INDEX_PREFIX) else {
        return false;
    };
    suffix.len() == "-0000-00-00".len()
        && suffix.chars().zip("-####-##-##".chars()).all(|(got, want)| {
            if want == '#' {
                got.is_ascii_digit()
            } else {
                got == want
            }
        })
}

/// The one index the domain holds, once the delivery has created it — read through the
/// cluster's own `_cat/indices`, which is where a client looks for it.
async fn rotated_index(router: &MultiServiceRouter, case: &str) -> String {
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        let answer = rest(router, "GET", "/_cat/indices?format=json", None).await;
        let listed = answer.json();
        let names: Vec<String> = listed
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry["index"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if let [one] = names.as_slice() {
            return one.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the delivery to create exactly one index, got {names:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE (through the domain's own data plane): a numeric field cleans a value up to fit its
/// type while it coerces — a number read out of a string, a fraction truncated into a
/// whole-number type — and a field declaring `coerce: false` takes only a value already of its
/// own type.
///
/// The rows differ along what the rule generalises over: the numeric family (whole-number,
/// floating-point), the form the value arrives in (a number, a fraction, a numeric string, a
/// string holding no number, an array, no value at all), and the setting of the parameter
/// (absent, `true`, `false`). Source of the parameter and of the field types: the engine's own
/// `coerce` mapping parameter and its supported field types.
#[tokio::test]
async fn the_coerce_parameter_decides_which_values_a_numeric_field_takes() {
    let router = emulator();
    given_domain(&router).await;
    provisioned(&router).await;

    // (the case, the mapping of the field, the value the document carries, whether it is taken)
    let rows: &[(&str, Value, Value, bool)] = &[
        (
            "a numeric string for a non-coercing integer",
            json!({ "type": "integer", "coerce": false }),
            json!("12"),
            false,
        ),
        (
            "a number for a non-coercing integer",
            json!({ "type": "integer", "coerce": false }),
            json!(12),
            true,
        ),
        (
            "a fraction for a non-coercing integer",
            json!({ "type": "integer", "coerce": false }),
            json!(12.5),
            false,
        ),
        (
            "an array holding a numeric string, for a non-coercing integer",
            json!({ "type": "integer", "coerce": false }),
            json!([1, "2"]),
            false,
        ),
        (
            "no value at all, for a non-coercing integer",
            json!({ "type": "integer", "coerce": false }),
            json!(null),
            true,
        ),
        (
            "a numeric string for an integer that coerces by default",
            json!({ "type": "integer" }),
            json!("12"),
            true,
        ),
        (
            "a fraction for an integer that coerces by default",
            json!({ "type": "integer" }),
            json!(12.5),
            true,
        ),
        (
            "a numeric string for an integer that coerces explicitly",
            json!({ "type": "integer", "coerce": true }),
            json!("12"),
            true,
        ),
        (
            "a string holding no number, for an integer that coerces",
            json!({ "type": "integer" }),
            json!("many"),
            false,
        ),
        (
            "a numeric string for a non-coercing long",
            json!({ "type": "long", "coerce": false }),
            json!("7"),
            false,
        ),
        (
            "a fraction for a non-coercing double",
            json!({ "type": "double", "coerce": false }),
            json!(1.5),
            true,
        ),
        (
            "a numeric string for a non-coercing double",
            json!({ "type": "double", "coerce": false }),
            json!("1.5"),
            false,
        ),
    ];

    for (ordinal, (case, mapping, value, taken)) in rows.iter().enumerate() {
        // One index per row, so a row's verdict is read on an index nothing else wrote to.
        let index = format!("coercion-{ordinal}");
        let created = rest(
            &router,
            "PUT",
            &format!("/{index}"),
            Some(json!({ "mappings": { "properties": { "qty": mapping } } })),
        )
        .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: the index must be created, got {} with {}",
            created.status,
            created.text()
        );

        let written = rest(
            &router,
            "PUT",
            &format!("/{index}/_doc/row"),
            Some(json!({ "qty": value })),
        )
        .await;
        let answered = written.json();
        if *taken {
            assert_eq!(
                (written.status, answered["result"].as_str()),
                (201, Some("created")),
                "case {case:?}: expected {value} to be taken, got {} with {answered}",
                written.status
            );
        } else {
            assert_eq!(
                (written.status, answered["error"]["type"].as_str()),
                (400, Some("mapper_parsing_exception")),
                "case {case:?}: expected {value} to be refused as a parse failure, got {} with \
                 {answered}",
                written.status
            );
            let reason = answered["error"]["reason"].as_str().unwrap_or_default();
            assert!(
                reason.contains("[qty]"),
                "case {case:?}: expected the reason to name the field, got {reason:?}"
            );
        }

        let count = counted(&router, case, &index).await;
        assert_eq!(
            count,
            i64::from(*taken),
            "case {case:?}: expected the index to hold only what the mappings took, got {count}"
        );
    }
}

/// RULE (through the producer): Firehose indexes the documents the destination index's
/// mappings take and parks every document the cluster refused in the destination's Amazon S3
/// backup, each parked record carrying the record's own bytes, the index the delivery used and
/// the cluster's reason.
///
/// The rows differ along what the rule generalises over: documents the mappings take (with and
/// without the optional field), and one per way a document can disagree with them — a numeric
/// string and a fraction for a field that does not coerce, an object for a field mapped whole,
/// and a field the strict mappings do not map.
#[tokio::test]
async fn a_delivery_indexes_only_the_documents_the_index_mappings_take() {
    let router = emulator();
    let case = "a strict destination index";
    given_bucket(&router).await;
    let domain_arn = given_domain(&router).await;
    provisioned(&router).await;

    // The mappings the delivery's own index is created with, installed before the producer
    // starts: a template applies to an index as it is created.
    let template = rest(
        &router,
        "PUT",
        &format!("/_index_template/{INDEX_PREFIX}"),
        Some(strict_template()),
    )
    .await;
    assert_eq!(
        template.status,
        200,
        "setup the index template: expected 200, got {} with {}",
        template.status,
        template.text()
    );

    let role_arn = format!(
        "arn:aws:iam::{}:role/firehose-mapping-verdict",
        engine::emulator_account_id()
    );
    let created = firehose(
        &router,
        "CreateDeliveryStream",
        json!({
            "DeliveryStreamName": STREAM,
            "DeliveryStreamType": "DirectPut",
            "AmazonopensearchserviceDestinationConfiguration": {
                "RoleARN": role_arn,
                "DomainARN": domain_arn,
                "IndexName": INDEX_PREFIX,
                "IndexRotationPeriod": "OneDay",
                "S3BackupMode": "FailedDocumentsOnly",
                // The shortest wait this destination's `IntervalInSeconds` admits: this case is
                // about which documents are indexed, not about how long a buffer is held.
                "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
                "RetryOptions": { "DurationInSeconds": 0 },
                "S3Configuration": {
                    "RoleARN": role_arn,
                    "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
                    "CompressionFormat": "UNCOMPRESSED",
                },
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup CreateDeliveryStream: expected 200, got {} with {}",
        created.status,
        created.text()
    );

    // The stream takes records only once it reports itself ACTIVE, which is the condition a
    // producer waits for rather than a span of time.
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        let described = firehose(
            &router,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": STREAM }),
        )
        .await
        .json();
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == "ACTIVE" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the stream never reported itself ACTIVE, last read {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }

    let records = records();
    let entries: Vec<Value> = records
        .iter()
        .map(|record| {
            json!({ "Data": aws_smithy_types::base64::encode(record.document.to_string()) })
        })
        .collect();
    let put = firehose(
        &router,
        "PutRecordBatch",
        json!({ "DeliveryStreamName": STREAM, "Records": entries }),
    )
    .await;
    let answered = put.json();
    assert_eq!(
        (put.status, answered["FailedPutCount"].as_i64()),
        (200, Some(0)),
        "case {case:?}: every record of the batch must be accepted, got {} with {answered}",
        put.status
    );

    let refused: Vec<&Record> = records.iter().filter(|record| !record.indexed).collect();
    let parked = parked_documents(&router, case, refused.len()).await;
    assert_eq!(
        parked.len(),
        refused.len(),
        "case {case:?}: expected exactly the refused documents parked, got {parked:?}"
    );

    // The index the delivery created, and the documents it holds: only the ones the mappings
    // took. The backup is already written at this point, so the delivery is over and the count
    // is settled.
    let index = rotated_index(&router, case).await;
    assert!(
        has_day_suffix(&index),
        "case {case:?}: expected the index named for the day of the delivery, got {index:?}"
    );
    let taken = records.iter().filter(|record| record.indexed).count();
    let count = counted(&router, case, &index).await;
    assert_eq!(
        count,
        i64::try_from(taken).unwrap_or_default(),
        "case {case:?}: expected only the conforming documents indexed, got {count}"
    );

    // Every refused record is parked as itself: its own bytes, the index the delivery wrote to,
    // and a reason naming the field the mappings disagreed about.
    for record in &refused {
        let raw = aws_smithy_types::base64::encode(record.document.to_string());
        let found = parked
            .iter()
            .find(|document| document["rawData"].as_str() == Some(raw.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the refused record's own bytes parked, got {parked:?}",
                    record.case
                )
            });
        assert_eq!(
            found["osIndexName"].as_str(),
            Some(index.as_str()),
            "case {:?}: expected the index the delivery used, got {found}",
            record.case
        );
        assert!(
            found["errorCode"]
                .as_str()
                .is_some_and(|code| !code.is_empty()),
            "case {:?}: expected the cluster's error code beside the record, got {found}",
            record.case
        );
        let message = found["errorMessage"].as_str().unwrap_or_default();
        assert!(
            message.contains(record.names),
            "case {:?}: expected a reason naming {}, got {message:?}",
            record.case,
            record.names
        );
    }

    // And nothing of this delivery was written anywhere but the failed-document folder: the
    // mode parks the refused documents alone.
    let keys = keys_under(&router, "").await;
    assert!(
        keys.iter().all(|key| key.starts_with(FAILED_FOLDER)),
        "case {case:?}: expected the backup to hold only failed documents, got {keys:?}"
    );
}
