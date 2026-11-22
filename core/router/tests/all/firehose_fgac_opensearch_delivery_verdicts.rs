// SPDX-License-Identifier: CC0-1.0

//! A Firehose stream delivering into an Amazon OpenSearch Service domain that runs
//! FINE-GRAINED ACCESS CONTROL, read back at the address that domain published.
//!
//! The rule under test, one level above the recorded case: **on a domain with fine-grained
//! access control switched on, the CLUSTER's own security configuration decides what a
//! producer may write, whatever IAM allows — the master user the create named may do
//! everything, a principal a role mapping binds may do only what one of the roles it is bound
//! to permits on the indexes that role's patterns name, and a principal no mapping binds may
//! do nothing at all — so a Firehose destination delivering into such a domain indexes
//! exactly the documents the cluster TOOK, into the index its `IndexRotationPeriod` names, and
//! writes every document the cluster refused — whether the refusal is the index mappings' or
//! the security configuration's — to the destination's Amazon S3 backup ("Firehose writes any
//! documents that could not be indexed to the configured Amazon S3 destination, with
//! `AmazonOpenSearchService-failed/` appended to the key prefix", `S3BackupMode`), in Firehose's
//! own error output naming the index the delivery used; and the producer is told none of it:
//! `PutRecordBatch` accepts the whole batch.**
//!
//! Only a cross-service suite can pin that: the security configuration, the mappings and the
//! documents belong to Amazon OpenSearch Service, the delivery and its error output to
//! Firehose, and the parked objects to Amazon S3, all on ONE engine exactly as
//! `core/router/src/lib.rs` mounts them.
//!
//! The rows differ along what the rule generalises over — the CLUSTER's verdict on the
//! destination's delivery role (bound to a role that permits the destination's indexes, bound
//! to a role that permits only another index, bound by nothing at all) — and, inside each row,
//! along the documents of one batch (three the index's `dynamic: strict` mappings take, three
//! disagreeing with them in a different way).
//!
//! Every reading is taken the way a client takes it: at the host `DescribeDomain` published,
//! presenting the master user's own credential, through `GET /_cat/indices/<pattern>` and
//! `GET /<index>/_count`, and through `ListObjectsV2` + `GetObject` on the backup bucket.
//! Nothing here writes into a store to arrange what it then asserts.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::elements;

/// The Region every call below is signed for, and the one every ARN here names.
const REGION: &str = "us-west-2";

/// The one domain every row delivers into.
const DOMAIN: &str = "fgac-delivery-domain";

/// The engine release that domain runs.
const ENGINE_VERSION: &str = "OpenSearch_2.11";

/// The master user the domain is created with, and its password — the credentials
/// `MasterUserOptions$MasterUserName` / `MasterUserPassword` name, within that member's
/// modelled length (8..128).
const MASTER: &str = "fh-os-master";
const MASTER_PASSWORD: &str = "Master-pass-1!";

/// The bucket every destination's backup parks refused documents in.
const BUCKET: &str = "fgac-delivery-backup";

/// The folder a document the domain did not index is written under, appended to the backup's
/// own `Prefix` (`S3BackupMode`).
const FAILED_FOLDER: &str = "AmazonOpenSearchService-failed/";

/// The index template every row's destination index is created from, and the mapped fields it
/// declares.
const TEMPLATE: &str = "fgac-delivery-template";

/// The namespace every error code of an `Amazonopensearchservice` destination's error output
/// is written in.
const ERROR_NAMESPACE: &str = "OS.";

/// How long a phase waits for the condition it is about — the domain reporting itself
/// provisioned, the stream reporting itself `ACTIVE`, the delivery landing.
///
/// Generous on purpose: nothing here asserts on how long anything took, so this is only the
/// point at which waiting is called a failure.
const PHASE_DEADLINE: Duration = Duration::from_secs(120);

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
/// key the router claims an AWS API request by. Nothing here is a credential: this router runs
/// with request authentication off, so no signature is verified.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One request to `host`, with the headers it carries.
async fn dispatch(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
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

/// One AWS API request to the service `signing_name` serves.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let authorization = authorization(signing_name);
    let mut carried: Vec<(&str, &str)> = vec![("authorization", authorization.as_str())];
    carried.extend_from_slice(headers);
    dispatch(router, &host, method, path, &carried, body).await
}

/// One Amazon OpenSearch Service control-plane request over its `restJson1` wire.
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

/// One request to the domain's own REST data plane, at the host `DescribeDomain` published and
/// as the MASTER user — the addressing and the credential a client reads its domain through.
async fn as_master(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let encoded = aws_smithy_types::base64::encode(format!("{MASTER}:{MASTER_PASSWORD}"));
    dispatch(
        router,
        host,
        method,
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &format!("Basic {encoded}")),
        ],
        payload.into_bytes(),
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

/// The ARN of one delivery role — a role of this emulator's own account, which is the account
/// a role mapping's `backend_roles` entry names it by.
fn role_arn(role: &str) -> String {
    format!("arn:aws:iam::{}:role/{role}", engine::emulator_account_id())
}

/// The ARN and the published endpoint host of a domain created with fine-grained access
/// control over an internal master user and NO access policy of its own, so what decides a
/// delivery is the cluster's security configuration alone.
async fn given_fgac_domain(router: &MultiServiceRouter) -> (String, String) {
    let created = es(
        router,
        "POST",
        "/2021-01-01/opensearch/domain",
        Some(json!({
            "DomainName": DOMAIN,
            "EngineVersion": ENGINE_VERSION,
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
            "EncryptionAtRestOptions": { "Enabled": true },
            "NodeToNodeEncryptionOptions": { "Enabled": true },
            "DomainEndpointOptions": { "EnforceHTTPS": true },
            "AdvancedSecurityOptions": {
                "Enabled": true,
                "InternalUserDatabaseEnabled": true,
                "MasterUserOptions": {
                    "MasterUserName": MASTER,
                    "MasterUserPassword": MASTER_PASSWORD,
                },
            },
        })),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup CreateDomain: expected 200, got {} with {}",
        created.status,
        created.text()
    );
    let arn = created.json()["DomainStatus"]["ARN"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "setup CreateDomain: expected an ARN, got {}",
                created.text()
            )
        })
        .to_string();
    // The condition a client's readiness poll waits for — the domain reporting itself created
    // with nothing in flight — rather than a span of time.
    let deadline = Instant::now() + PHASE_DEADLINE;
    let host = loop {
        let described = es(
            router,
            "GET",
            &format!("/2021-01-01/opensearch/domain/{DOMAIN}"),
            None,
        )
        .await
        .json();
        let status = &described["DomainStatus"];
        let endpoint = status["Endpoint"].as_str().unwrap_or_default().to_string();
        if status["Created"].as_bool() == Some(true)
            && status["Processing"].as_bool() == Some(false)
            && !endpoint.is_empty()
        {
            break endpoint;
        }
        assert!(
            Instant::now() < deadline,
            "setup DescribeDomain: the domain never published an endpoint while reporting \
             itself provisioned, last read {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    // The address the control plane published is the address the cluster answers its master
    // at: the first thing a client of this pipeline waits for, and what every reading below
    // is taken through.
    let health = as_master(router, &host, "GET", "/_cluster/health", None).await;
    assert_eq!(
        health.status,
        200,
        "setup the master reading the cluster health at the published endpoint {host:?}: \
         expected 200, got {} with {}",
        health.status,
        health.text()
    );
    (arn, host)
}

/// `PUT /<bucket>` — Amazon S3's `CreateBucket`, naming the Region it is created in.
async fn given_bucket(router: &MultiServiceRouter) {
    let created = send(
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
        created.status,
        200,
        "setup CreateBucket: expected 200, got {} with {}",
        created.status,
        created.text()
    );
}

/// Install, as the master user, the mappings every row's destination index is created with:
/// three mapped fields, nothing else admitted (`dynamic: strict`), and a whole-number field
/// that reads no value it would have to clean up (`coerce: false`). A template applies to an
/// index as it is CREATED, which is why it is installed before any producer starts.
async fn given_strict_template(router: &MultiServiceRouter, host: &str, patterns: Vec<String>) {
    let written = as_master(
        router,
        host,
        "PUT",
        &format!("/_index_template/{TEMPLATE}"),
        Some(json!({
            "index_patterns": patterns,
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
        })),
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "setup the index template: expected 200, got {} with {}",
        written.status,
        written.text()
    );
}

/// Write, as the master user, the security role `role` permitting the write and create actions
/// on the indexes `pattern` names, and bind `principal` to it with a role mapping — the two
/// writes a client makes to let a producer's role deliver into its domain.
///
/// The `allowed_actions` entries are the security plugin's own default action groups.
async fn given_mapped_role(
    router: &MultiServiceRouter,
    host: &str,
    role: &str,
    pattern: &str,
    principal: &str,
) {
    let written = as_master(
        router,
        host,
        "PUT",
        &format!("/_plugins/_security/api/roles/{role}"),
        Some(json!({
            "index_permissions": [{
                "index_patterns": [pattern],
                "allowed_actions": ["crud", "create_index", "manage"],
            }],
        })),
    )
    .await;
    assert_eq!(
        written.status,
        201,
        "setup the security role {role:?}: expected 201, got {} with {}",
        written.status,
        written.text()
    );
    let mapped = as_master(
        router,
        host,
        "PUT",
        &format!("/_plugins/_security/api/rolesmapping/{role}"),
        Some(json!({ "backend_roles": [principal] })),
    )
    .await;
    assert_eq!(
        mapped.status,
        201,
        "setup the role mapping {role:?}: expected 201, got {} with {}",
        mapped.status,
        mapped.text()
    );
}

/// One row: the cluster's verdict on a destination's delivery role, and the destination that
/// is delivered through under it.
struct Row {
    case: &'static str,
    /// The delivery role the destination is configured with.
    role: &'static str,
    /// The stream the batch is put on.
    stream: &'static str,
    /// `IndexName`, before the rotation suffix the period appends.
    index_prefix: &'static str,
    /// The backup's own `Prefix`, which the failed folder is appended to.
    backup_prefix: &'static str,
    /// Whether the cluster lets this role write the destination's indexes at all.
    writes: bool,
}

/// The three verdicts a destination's delivery role can meet on such a domain.
const ROWS: [Row; 3] = [
    Row {
        case: "a delivery role bound to a role permitting the destination's own indexes",
        role: "firehose-fgac-bound",
        stream: "fgac-bound-stream",
        index_prefix: "fgac-bound",
        backup_prefix: "bound/",
        writes: true,
    },
    Row {
        case: "a delivery role bound to a role permitting only another index",
        role: "firehose-fgac-narrow",
        stream: "fgac-narrow-stream",
        index_prefix: "fgac-narrow",
        backup_prefix: "narrow/",
        writes: false,
    },
    Row {
        case: "a delivery role no role mapping binds",
        role: "firehose-fgac-unbound",
        stream: "fgac-unbound-stream",
        index_prefix: "fgac-unbound",
        backup_prefix: "unbound/",
        writes: false,
    },
];

/// One document of the batch every row delivers, and whether the index's mappings take it.
struct Document {
    case: &'static str,
    source: Value,
    /// Whether the mappings themselves accept it — what the cluster is left to decide once the
    /// security configuration has admitted the write.
    conforming: bool,
}

/// The batch: documents the mappings take, and one per way a document can disagree with them.
fn documents() -> Vec<Document> {
    vec![
        Document {
            case: "a document of the mapped fields",
            source: json!({ "seq": 1, "label": "alpha", "qty": 3 }),
            conforming: true,
        },
        Document {
            case: "a document leaving a mapped field out",
            source: json!({ "seq": 2, "label": "beta" }),
            conforming: true,
        },
        Document {
            case: "a document whose whole-number field carries a whole number",
            source: json!({ "seq": 3, "label": "gamma", "qty": 0 }),
            conforming: true,
        },
        Document {
            case: "a numeric string for a field that does not coerce",
            source: json!({ "seq": 100, "label": "bad-coerce", "qty": "12" }),
            conforming: false,
        },
        Document {
            case: "an object for a field mapped whole",
            source: json!({ "seq": 101, "label": { "not": "a-keyword" }, "qty": 1 }),
            conforming: false,
        },
        Document {
            case: "a field the strict mappings do not map",
            source: json!({ "seq": 102, "label": "delta", "qty": 1, "unmapped": "x" }),
            conforming: false,
        },
    ]
}

/// Create the row's stream. Every row's is created before any of them is waited on, so the
/// provisioning of the three runs alongside each other rather than one after the next.
async fn given_stream(router: &MultiServiceRouter, row: &Row, domain_arn: &str) {
    let role_arn = role_arn(row.role);
    let created = firehose(
        router,
        "CreateDeliveryStream",
        json!({
            "DeliveryStreamName": row.stream,
            "DeliveryStreamType": "DirectPut",
            "AmazonopensearchserviceDestinationConfiguration": {
                "RoleARN": role_arn,
                "DomainARN": domain_arn,
                "IndexName": row.index_prefix,
                "IndexRotationPeriod": "OneDay",
                "S3BackupMode": "FailedDocumentsOnly",
                // The shortest wait and the smallest buffer this destination's own ranges
                // admit (`IntervalInSeconds` 0..900, `SizeInMBs` 1..100): these rows are about
                // which documents are indexed, not about how long a buffer is held.
                "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
                // No retry window, which `DurationInSeconds`'s own range (0..7200) admits: a
                // buffer the cluster refused is parked on the first attempt.
                "RetryOptions": { "DurationInSeconds": 0 },
                "S3Configuration": {
                    "RoleARN": role_arn,
                    "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
                    "Prefix": row.backup_prefix,
                    "CompressionFormat": "UNCOMPRESSED",
                },
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {:?}: setup CreateDeliveryStream expected 200, got {} with {}",
        row.case,
        created.status,
        created.text()
    );
}

/// Wait until the row's stream reports itself `ACTIVE` — the condition a producer waits for
/// rather than a span of time — and assert that reporting read echoes the destination settings
/// the create supplied.
async fn active_and_echoed(router: &MultiServiceRouter, row: &Row) {
    let deadline = Instant::now() + PHASE_DEADLINE;
    let described = loop {
        let described = firehose(
            router,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": row.stream }),
        )
        .await
        .json();
        if described["DeliveryStreamDescription"]["DeliveryStreamStatus"].as_str() == Some("ACTIVE")
        {
            break described;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: the stream never reported itself ACTIVE, last read {described}",
            row.case
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    // What the create supplied is what the read reports back: a client comparing desired
    // against actual configuration reads its own zeros, not the kind's documented defaults.
    let destination = &described["DeliveryStreamDescription"]["Destinations"][0]
        ["AmazonopensearchserviceDestinationDescription"];
    for (member, expected) in [
        ("IndexName", json!(row.index_prefix)),
        ("IndexRotationPeriod", json!("OneDay")),
        ("S3BackupMode", json!("FailedDocumentsOnly")),
    ] {
        assert_eq!(
            destination[member], expected,
            "case {:?}: expected the described destination to echo {member} as {expected}, got \
             {destination}",
            row.case
        );
    }
    assert_eq!(
        (
            destination["BufferingHints"]["IntervalInSeconds"].as_i64(),
            destination["BufferingHints"]["SizeInMBs"].as_i64(),
            destination["RetryOptions"]["DurationInSeconds"].as_i64(),
        ),
        (Some(0), Some(1), Some(0)),
        "case {:?}: expected the described destination to echo the buffering and retry it was \
         created with, got {destination}",
        row.case
    );
}

/// `PutRecordBatch` with [`documents`] as the records, asserting the producer is told nothing
/// about what the cluster will make of them: the whole batch is accepted.
async fn put_batch(router: &MultiServiceRouter, row: &Row, batch: &[Document]) {
    let entries: Vec<Value> = batch
        .iter()
        .map(|document| {
            json!({ "Data": aws_smithy_types::base64::encode(document.source.to_string()) })
        })
        .collect();
    let put = firehose(
        router,
        "PutRecordBatch",
        json!({ "DeliveryStreamName": row.stream, "Records": entries }),
    )
    .await;
    let answered = put.json();
    assert_eq!(
        (put.status, answered["FailedPutCount"].as_i64()),
        (200, Some(0)),
        "case {:?}: every record of the batch must be accepted, got {} with {answered}",
        row.case,
        put.status
    );
}

/// Every key the backup bucket holds under `prefix`, read through `ListObjectsV2` with no
/// delimiter — the recursive read a client makes for a prefix whose objects sit under further
/// segments of their own.
async fn keys_under(router: &MultiServiceRouter, prefix: &str) -> Vec<String> {
    let listed = send(
        router,
        "s3",
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "listing {prefix:?}: expected 200, got {} with {}",
        listed.status,
        listed.text()
    );
    let mut keys = elements(&listed.text(), "Key");
    keys.sort();
    keys
}

/// The documents the backup holds under the row's failed folder right now, one per line of
/// each object there.
async fn backup_now(router: &MultiServiceRouter, row: &Row, prefix: &str) -> Vec<Value> {
    let mut parked = Vec::new();
    for key in keys_under(router, prefix).await {
        let read = send(
            router,
            "s3",
            "GET",
            &format!("/{BUCKET}/{key}"),
            &[],
            Vec::new(),
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {:?}: reading {key:?} must succeed, got {} with {}",
            row.case,
            read.status,
            read.text()
        );
        for line in read.text().lines().filter(|line| !line.trim().is_empty()) {
            parked.push(serde_json::from_str(line).unwrap_or_else(|error| {
                panic!(
                    "case {:?}: a parked document must be JSON: {error} in {line:?}",
                    row.case
                )
            }));
        }
    }
    parked
}

/// The documents the backup holds under the row's failed folder, once it holds at least `want`
/// of them AND a second look finds the same ones — so a caller comparing the count against what
/// the cluster refused is comparing a settled read and not the first part of a write in flight.
async fn parked_documents(router: &MultiServiceRouter, row: &Row, want: usize) -> Vec<Value> {
    let prefix = format!("{}{FAILED_FOLDER}", row.backup_prefix);
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        let parked = backup_now(router, row, &prefix).await;
        if parked.len() >= want {
            tokio::time::sleep(LOOK_AGAIN).await;
            let again = backup_now(router, row, &prefix).await;
            if again == parked {
                return parked;
            }
            continue;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected at least {want} parked document(s) under {prefix:?}, got \
             {parked:?}",
            row.case
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The indexes the domain holds whose name starts with the row's own `IndexName`, read through
/// the cluster's own `_cat/indices` — where a client looks for them. Empty where the cluster
/// holds none, which it reports as an index-not-found refusal.
async fn indexes_of(router: &MultiServiceRouter, host: &str, row: &Row) -> Vec<String> {
    let listed = as_master(
        router,
        host,
        "GET",
        &format!("/_cat/indices/{}*?format=json", row.index_prefix),
        None,
    )
    .await;
    if listed.status == 404 {
        return Vec::new();
    }
    assert_eq!(
        listed.status,
        200,
        "case {:?}: expected the index listing to be served, got {} with {}",
        row.case,
        listed.status,
        listed.text()
    );
    listed
        .json()
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["index"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The documents `index` holds, counted through the domain's own endpoint.
async fn counted(router: &MultiServiceRouter, host: &str, row: &Row, index: &str) -> i64 {
    let counted = as_master(router, host, "GET", &format!("/{index}/_count"), None).await;
    assert_eq!(
        counted.status,
        200,
        "case {:?}: expected the count to be served, got {} with {}",
        row.case,
        counted.status,
        counted.text()
    );
    counted.json()["count"].as_i64().unwrap_or(-1)
}

/// `<prefix>-####-##-##`: the shape of the index name `OneDay` rotation produces, with `#`
/// standing for a digit — the calendar fields the period names, asserted as a shape rather
/// than recomputed.
fn has_day_suffix(index: &str, prefix: &str) -> bool {
    let Some(suffix) = index.strip_prefix(prefix) else {
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

/// RULE: on a domain with fine-grained access control, the cluster's security configuration
/// decides what a destination's delivery role may write — so a Firehose destination indexes
/// exactly the documents the cluster took, into the index its rotation period names, and parks
/// every document it refused in the destination's Amazon S3 backup, whether the refusal was
/// the mappings' or the security configuration's.
#[tokio::test]
async fn fine_grained_access_control_decides_what_a_firehose_destination_indexes() {
    let router = emulator();
    given_bucket(&router).await;
    let (domain_arn, host) = given_fgac_domain(&router).await;
    given_strict_template(
        &router,
        &host,
        ROWS.iter()
            .map(|row| format!("{}*", row.index_prefix))
            .collect(),
    )
    .await;

    // The security configuration the rows are decided by: one role permitting the first row's
    // own indexes, one permitting an index no destination here delivers to, and — for the last
    // row — no mapping at all.
    given_mapped_role(
        &router,
        &host,
        "delivery-writer",
        &format!("{}*", ROWS[0].index_prefix),
        &role_arn(ROWS[0].role),
    )
    .await;
    given_mapped_role(
        &router,
        &host,
        "elsewhere-writer",
        "another-index*",
        &role_arn(ROWS[1].role),
    )
    .await;

    // One stream per row, every one of them created before any is waited on, and each taken to
    // the `ACTIVE` it reports before anything is put on it.
    for row in &ROWS {
        given_stream(&router, row, &domain_arn).await;
    }
    for row in &ROWS {
        active_and_echoed(&router, row).await;
    }

    let batch = documents();
    for row in &ROWS {
        put_batch(&router, row, &batch).await;
    }

    for row in &ROWS {
        // What the cluster refused: the documents its mappings would not take where the role
        // may write at all, and the WHOLE batch where it may not.
        let refused: Vec<&Document> = batch
            .iter()
            .filter(|document| !row.writes || !document.conforming)
            .collect();
        let parked = parked_documents(&router, row, refused.len()).await;
        assert_eq!(
            parked.len(),
            refused.len(),
            "case {:?}: expected exactly the refused documents parked, got {parked:?}",
            row.case
        );

        // The index the delivery used: the one its rotation period names, reported in the error
        // output of every document it could not index.
        let used: BTreeSet<String> = parked
            .iter()
            .map(|document| {
                document["osIndexName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            used.len(),
            1,
            "case {:?}: expected one index named across the batch's error output, got {used:?}",
            row.case
        );
        let named = used.iter().next().cloned().unwrap_or_default();
        assert!(
            has_day_suffix(&named, row.index_prefix),
            "case {:?}: expected the error output to name the index of the delivery's own day, \
             got {named:?}",
            row.case
        );

        // Each parked record is the record itself, in Firehose's own vocabulary: its bytes,
        // a code of this destination kind's namespace, and the cluster's reason beside it.
        for document in &refused {
            let raw = aws_smithy_types::base64::encode(document.source.to_string());
            let found = parked
                .iter()
                .find(|parked| parked["rawData"].as_str() == Some(raw.as_str()))
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}/{:?}: expected the refused record's own bytes parked, got \
                         {parked:?}",
                        row.case, document.case
                    )
                });
            assert!(
                found["errorCode"]
                    .as_str()
                    .is_some_and(|code| code.starts_with(ERROR_NAMESPACE)),
                "case {:?}/{:?}: expected a code in {ERROR_NAMESPACE:?}, got {found}",
                row.case,
                document.case
            );
            assert!(
                found["errorMessage"]
                    .as_str()
                    .is_some_and(|message| !message.is_empty()),
                "case {:?}/{:?}: expected the cluster's reason beside the code, got {found}",
                row.case,
                document.case
            );
        }

        // And what the domain HOLDS: one index of the delivery's day with the documents the
        // cluster took, where the role may write — and no index at all where it may not, the
        // backup being the only trace of the batch. The parked objects are already there, so
        // the delivery is over and both readings are settled.
        let held = indexes_of(&router, &host, row).await;
        let taken = batch.iter().filter(|document| document.conforming).count();
        if row.writes {
            assert_eq!(
                held,
                vec![named.clone()],
                "case {:?}: expected the one index the delivery created, got {held:?}",
                row.case
            );
            let count = counted(&router, &host, row, &named).await;
            assert_eq!(
                count,
                i64::try_from(taken).unwrap_or_default(),
                "case {:?}: expected only the conforming documents indexed, got {count}",
                row.case
            );
        } else {
            assert!(
                held.is_empty(),
                "case {:?}: expected the cluster to hold no index for a role it refuses, got \
                 {held:?}",
                row.case
            );
        }

        // Nothing of this delivery was written anywhere but the failed-document folder: the
        // mode parks the refused documents alone.
        let keys = keys_under(&router, row.backup_prefix).await;
        let folder = format!("{}{FAILED_FOLDER}", row.backup_prefix);
        assert!(
            keys.iter().all(|key| key.starts_with(&folder)),
            "case {:?}: expected the backup to hold only failed documents, got {keys:?}",
            row.case
        );
    }
}
