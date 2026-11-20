// SPDX-License-Identifier: CC0-1.0

//! The `errorCode` a Firehose stream reports for a document its Amazon OpenSearch Service
//! destination did not take.
//!
//! The rule under test, one level above the recorded case: **Firehose writes the error
//! output of a failed delivery in the DESTINATION KIND's own vocabulary, never in the
//! cluster's — every document of a failed batch parked in the destination's Amazon S3 backup
//! ("Firehose writes any documents that could not be indexed to the configured Amazon S3
//! destination, with `AmazonOpenSearchService-failed/` appended to the key prefix",
//! `S3BackupMode`) carries one `errorCode` in that kind's error namespace — `OS.` for an
//! `Amazonopensearchservice` destination and `ES.` for the same offering under its legacy
//! `Elasticsearch` name — followed by the Firehose exception name the class of the refusal
//! falls under, names the index it used in that kind's own member (`osIndexName` /
//! `esIndexName`), and leaves the cluster's own error type and wording in `errorMessage`.**
//!
//! Only a cross-service suite can pin that: the refusal belongs to Amazon OpenSearch
//! Service, the delivery and its error output to Firehose, and the parked objects to Amazon
//! S3, all on ONE engine exactly as `core/router/src/lib.rs` mounts them.
//!
//! One test per class a delivery can fail in — the index's mappings would not take the
//! document, the cluster would not let the destination's role write it, and nothing answered
//! at the destination at all — each driven against BOTH destination kinds, which is what the
//! namespace is read off.
//!
//! Nothing here writes into a store to arrange what it then asserts: the domain comes from
//! `CreateDomain`, its mappings from the domain's own index-template API, the streams from
//! `CreateDeliveryStream`, the records from `PutRecordBatch`, and every reading is taken
//! through `ListObjectsV2` and `GetObject`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::elements;

/// The Region every call below is signed for, and the one every ARN here names.
const REGION: &str = "us-west-2";

/// The engine release every domain here runs.
const ENGINE_VERSION: &str = "OpenSearch_2.11";

/// The bucket every stream's backup parks refused documents in.
const BUCKET: &str = "failed-document-code-backup";

/// The folder a document the domain did not index is written under, appended to the backup's
/// own `Prefix` (`S3BackupMode`).
const FAILED_FOLDER: &str = "AmazonOpenSearchService-failed/";

/// How long a phase waits for the condition it is about — the domain reporting itself
/// provisioned, the stream reporting itself `ACTIVE`, the delivery landing.
///
/// Generous on purpose: nothing here asserts on how long anything took, so this is only the
/// point at which waiting is called a failure.
const PHASE_DEADLINE: Duration = Duration::from_secs(60);

/// How long each look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// An ARN no domain of this emulator carries, which is how a destination that answers
/// NOTHING is addressed.
const ABSENT_DOMAIN_ARN: &str = "arn:aws:es:us-west-2:000000000000:domain/absent-domain";

/// The index template the mapping-conflict case installs, and the prefix of the indexes it
/// applies to.
const STRICT_TEMPLATE: &str = "failed-document-codes";
const STRICT_INDEX_PREFIX: &str = "strict-codes-";

/// One Amazon OpenSearch Service destination kind, and the vocabulary its error output is
/// written in.
struct Kind {
    case: &'static str,
    /// The `CreateDeliveryStream` member this kind's configuration is sent as.
    member: &'static str,
    /// The namespace every code of this kind begins with.
    namespace: &'static str,
    /// The code a document the index's mappings would not take is reported under.
    unparsable: &'static str,
    /// The code a document the cluster would not let the role write is reported under.
    denied: &'static str,
    /// The code a buffer the destination never answered is reported under.
    unavailable: &'static str,
    /// The member this kind's error output names the delivery's index in.
    index_member: &'static str,
    /// The member this kind's error output names the delivery's document id in.
    document_id_member: &'static str,
}

/// Both destination kinds: the same offering under its current and its legacy name.
const KINDS: [Kind; 2] = [
    Kind {
        case: "an Amazonopensearchservice destination",
        member: "AmazonopensearchserviceDestinationConfiguration",
        namespace: "OS.",
        unparsable: "OS.AmazonOpenSearchServiceParseException",
        denied: "OS.BulkWritePermissionDenied",
        unavailable: "OS.ServiceUnavailable",
        index_member: "osIndexName",
        document_id_member: "osDocumentId",
    },
    Kind {
        case: "an Elasticsearch destination",
        member: "ElasticsearchDestinationConfiguration",
        namespace: "ES.",
        unparsable: "ES.AmazonElasticsearchParseException",
        denied: "ES.BulkWritePermissionDenied",
        unavailable: "ES.ServiceUnavailable",
        index_member: "esIndexName",
        document_id_member: "esDocumentId",
    },
];

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

/// One Amazon OpenSearch Service request over its `restJson1` wire.
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

/// The role every destination here delivers under — one of this emulator's own account, which
/// is the account a domain's access policy names it by.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/firehose-failed-document-codes",
        engine::emulator_account_id()
    )
}

/// `CreateDomain` for a single-node domain carrying `access_policies`, answering the ARN its
/// status published — the value a destination configuration names it by.
async fn given_domain(
    router: &MultiServiceRouter,
    domain: &str,
    access_policies: Option<Value>,
) -> String {
    let mut request = json!({
        "DomainName": domain,
        "EngineVersion": ENGINE_VERSION,
        "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
        "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
    });
    if let Some(policy) = access_policies {
        // `AccessPolicies` is the policy DOCUMENT as text, which is how a client sends it.
        request["AccessPolicies"] = json!(policy.to_string());
    }
    let answer = es(
        router,
        "POST",
        "/2021-01-01/opensearch/domain",
        Some(request),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup CreateDomain {domain}: expected 200, got {} with {}",
        answer.status,
        answer.text()
    );
    let described = answer.json();
    described["DomainStatus"]["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateDomain {domain}: expected an ARN, got {described}"))
        .to_string()
}

/// Wait until `DescribeDomain` reports `domain` created with nothing in flight — the
/// condition a client's readiness poll waits for, rather than a span of time.
async fn provisioned(router: &MultiServiceRouter, domain: &str) {
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        let described = es(
            router,
            "GET",
            &format!("/2021-01-01/opensearch/domain/{domain}"),
            None,
        )
        .await
        .json();
        let status = &described["DomainStatus"];
        if status["Created"].as_bool() == Some(true) && status["Processing"].as_bool() == Some(false)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup DescribeDomain {domain}: it never reported itself provisioned, last read \
             {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Install, on `domain`, the mappings the delivery's own index is created with: nothing but
/// the one field they declare may be introduced, so a document carrying another field is a
/// mapping conflict. A template applies to an index as it is CREATED, which is why it is
/// installed before the producer starts.
async fn given_strict_template(router: &MultiServiceRouter, domain: &str) {
    let answer = es(
        router,
        "PUT",
        &format!("/_aws/es/{domain}/_index_template/{STRICT_TEMPLATE}"),
        Some(json!({
            "index_patterns": [format!("{STRICT_INDEX_PREFIX}*")],
            "priority": 100,
            "template": {
                "mappings": {
                    "dynamic": "strict",
                    "properties": { "level": { "type": "keyword" } },
                },
            },
        })),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup the index template on {domain}: expected 200, got {} with {}",
        answer.status,
        answer.text()
    );
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

/// Create a stream whose only destination is `member`: the domain `domain_arn` addresses, the
/// index `index`, and an Amazon S3 backup under `prefix`; then wait until it reports itself
/// `ACTIVE`, which is the condition a producer waits for rather than a span of time.
///
/// `retry_in_seconds` is the destination's `RetryOptions$DurationInSeconds`: a case about a
/// document the CLUSTER refused leaves it at the documented default, so a flush that lands
/// before the domain is provisioned is attempted again rather than parked, and the case about
/// a destination that answers nothing sets it to the zero its own range admits, which parks
/// that buffer on the first attempt.
async fn given_active_stream(
    router: &MultiServiceRouter,
    stream: &str,
    kind: &Kind,
    domain_arn: &str,
    index: &str,
    prefix: &str,
    retry_in_seconds: Option<i32>,
) {
    let role_arn = role_arn();
    let mut destination = json!({
        "RoleARN": role_arn,
        "DomainARN": domain_arn,
        "IndexName": index,
        // No rotation, so the index the documents are offered to is the one named here.
        "IndexRotationPeriod": "NoRotation",
        "S3BackupMode": "FailedDocumentsOnly",
        // The shortest wait this destination's `IntervalInSeconds` admits: these cases are
        // about which code a refusal is reported under, not about how long a buffer is held.
        "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
        "S3Configuration": {
            "RoleARN": role_arn,
            "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
            "Prefix": prefix,
            "CompressionFormat": "UNCOMPRESSED",
        },
    });
    if let Some(seconds) = retry_in_seconds {
        destination["RetryOptions"] = json!({ "DurationInSeconds": seconds });
    }
    let mut request = json!({
        "DeliveryStreamName": stream,
        "DeliveryStreamType": "DirectPut",
    });
    request[kind.member] = destination;
    let created = firehose(router, "CreateDeliveryStream", request).await;
    assert_eq!(
        created.status,
        200,
        "setup CreateDeliveryStream {stream}: expected 200, got {} with {}",
        created.status,
        created.text()
    );
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        let described = firehose(
            router,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await
        .json();
        if described["DeliveryStreamDescription"]["DeliveryStreamStatus"].as_str() == Some("ACTIVE")
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup {stream}: it never reported itself ACTIVE, last read {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
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

/// The documents the backup holds under `prefix`, one per line of each object there, once it
/// holds at least `want` of them — or a failure naming `case` once [`PHASE_DEADLINE`] has
/// passed.
async fn parked_documents(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
    want: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        let mut documents = Vec::new();
        for key in keys_under(router, prefix).await {
            let answer =
                send(router, "s3", "GET", &format!("/{BUCKET}/{key}"), &[], Vec::new()).await;
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
            "case {case:?}: expected at least {want} parked document(s) under {prefix:?}, got \
             {documents:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The two documents every case delivers: a batch, so the "one code per failed batch" half of
/// the rule is observed and not assumed. Each carries a field the strict mappings do not map,
/// which is what the mapping-conflict case refuses them for.
fn documents() -> Vec<Value> {
    vec![
        json!({ "level": "info", "surprise": "one" }),
        json!({ "level": "warn", "surprise": "two" }),
    ]
}

/// `PutRecordBatch` with [`documents`] as the records, asserting every one of them was
/// accepted — a failed delivery is reported in the backup, not to the producer.
async fn put_batch(router: &MultiServiceRouter, case: &str, stream: &str) -> Vec<Value> {
    let delivered = documents();
    let entries: Vec<Value> = delivered
        .iter()
        .map(|document| json!({ "Data": aws_smithy_types::base64::encode(document.to_string()) }))
        .collect();
    let put = firehose(
        router,
        "PutRecordBatch",
        json!({ "DeliveryStreamName": stream, "Records": entries }),
    )
    .await;
    let answered = put.json();
    assert_eq!(
        (put.status, answered["FailedPutCount"].as_i64()),
        (200, Some(0)),
        "case {case:?}: every record of the batch must be accepted, got {} with {answered}",
        put.status
    );
    delivered
}

/// Assert the whole rule over one case's parked batch: one code for the batch, that code in
/// the destination kind's namespace and naming the class rather than the engine's own type,
/// the index named in that kind's own member, the cluster's reason beside it, and each
/// record's own bytes intact.
fn carries(
    case: &str,
    parked: &[Value],
    kind: &Kind,
    expected: &str,
    index: &str,
    delivered: &[Value],
) {
    let codes: std::collections::BTreeSet<String> = parked
        .iter()
        .map(|document| {
            document["errorCode"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        codes.len(),
        1,
        "case {case:?}: expected exactly one errorCode for the batch, got {codes:?}"
    );
    let code = codes.iter().next().expect("one code").clone();
    assert_eq!(
        code, expected,
        "case {case:?}: expected Firehose's own code, got {code:?}"
    );
    assert!(
        code.starts_with(kind.namespace),
        "case {case:?}: expected a code in {:?}, got {code:?}",
        kind.namespace
    );
    // The cluster's own error types are the `snake_case` names its answers carry, every one
    // of them ending in `_exception`; none of them is what Firehose reports.
    assert!(
        !code.contains("_exception"),
        "case {case:?}: expected the cluster's own error type kept out of the code, got {code:?}"
    );
    for document in parked {
        assert!(
            document["errorMessage"]
                .as_str()
                .is_some_and(|message| !message.is_empty()),
            "case {case:?}: expected the cluster's reason beside the code, got {document}"
        );
        assert_eq!(
            document[kind.index_member].as_str(),
            Some(index),
            "case {case:?}: expected the index named in {:?}, got {document}",
            kind.index_member
        );
        assert!(
            document.get(kind.document_id_member).is_some(),
            "case {case:?}: expected the document id named in {:?}, got {document}",
            kind.document_id_member
        );
        // The OTHER kind's members belong to the other destination's vocabulary, so this
        // envelope carries neither of them.
        for absent in ["osIndexName", "esIndexName"]
            .into_iter()
            .filter(|member| *member != kind.index_member)
        {
            assert!(
                document.get(absent).is_none(),
                "case {case:?}: expected no {absent:?} on this destination kind, got {document}"
            );
        }
    }
    for document in delivered {
        let raw = aws_smithy_types::base64::encode(document.to_string());
        assert!(
            parked
                .iter()
                .any(|parked| parked["rawData"].as_str() == Some(raw.as_str())),
            "case {case:?}: expected the record's own bytes parked, got {parked:?}"
        );
    }
}

/// RULE: a document the destination index's mappings would not take is parked under
/// Firehose's own code for the PARSE class, in the destination kind's namespace, with the
/// cluster's own reason in `errorMessage`.
#[tokio::test]
async fn a_mapping_conflict_is_parked_under_the_destinations_parse_code() {
    let router = emulator();
    given_bucket(&router).await;
    let domain = "codes-strict-domain";
    let domain_arn = given_domain(&router, domain, None).await;
    provisioned(&router, domain).await;
    given_strict_template(&router, domain).await;

    for (ordinal, kind) in KINDS.iter().enumerate() {
        let stream = format!("codes-strict-{ordinal}");
        let index = format!("{STRICT_INDEX_PREFIX}{ordinal}");
        let prefix = format!("strict-{ordinal}/");
        given_active_stream(
            &router, &stream, kind, &domain_arn, &index, &prefix, None,
        )
        .await;
        let delivered = put_batch(&router, kind.case, &stream).await;
        let parked = parked_documents(
            &router,
            kind.case,
            &format!("{prefix}{FAILED_FOLDER}"),
            delivered.len(),
        )
        .await;
        carries(
            kind.case,
            &parked,
            kind,
            kind.unparsable,
            &index,
            &delivered,
        );
    }
}

/// RULE: a document the cluster would not let the destination's role write is parked under
/// Firehose's own code for the DENIED class, in the destination kind's namespace.
#[tokio::test]
async fn a_denied_write_is_parked_under_the_destinations_permission_code() {
    let router = emulator();
    given_bucket(&router).await;
    let domain = "codes-denied-domain";
    // An access policy that refuses the delivery role outright: an explicit `Deny` is what
    // wins over every allow, whichever side states it.
    let denying = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": { "AWS": role_arn() },
            "Action": "es:*",
            "Resource": "*",
        }],
    });
    let domain_arn = given_domain(&router, domain, Some(denying)).await;
    provisioned(&router, domain).await;

    for (ordinal, kind) in KINDS.iter().enumerate() {
        let stream = format!("codes-denied-{ordinal}");
        let index = format!("denied-codes-{ordinal}");
        let prefix = format!("denied-{ordinal}/");
        given_active_stream(
            &router, &stream, kind, &domain_arn, &index, &prefix, None,
        )
        .await;
        let delivered = put_batch(&router, kind.case, &stream).await;
        let parked = parked_documents(
            &router,
            kind.case,
            &format!("{prefix}{FAILED_FOLDER}"),
            delivered.len(),
        )
        .await;
        carries(
            kind.case,
            &parked,
            kind,
            kind.denied,
            &index,
            &delivered,
        );
    }
}

/// RULE: a buffer whose destination domain answered NOTHING is parked, once the destination's
/// retry window is spent, under Firehose's own code for the UNAVAILABLE class — a verdict on
/// the destination, in the same namespace as every other code of that kind.
#[tokio::test]
async fn a_destination_that_answers_nothing_is_parked_under_the_unavailable_code() {
    let router = emulator();
    given_bucket(&router).await;

    for (ordinal, kind) in KINDS.iter().enumerate() {
        let stream = format!("codes-unreachable-{ordinal}");
        let index = format!("unreachable-codes-{ordinal}");
        let prefix = format!("unreachable-{ordinal}/");
        // A destination that is not retried at all, which `DurationInSeconds`'s own range
        // admits: the buffer is parked on the first attempt.
        given_active_stream(
            &router,
            &stream,
            kind,
            ABSENT_DOMAIN_ARN,
            &index,
            &prefix,
            Some(0),
        )
        .await;
        let delivered = put_batch(&router, kind.case, &stream).await;
        let parked = parked_documents(
            &router,
            kind.case,
            &format!("{prefix}{FAILED_FOLDER}"),
            delivered.len(),
        )
        .await;
        carries(
            kind.case,
            &parked,
            kind,
            kind.unavailable,
            &index,
            &delivered,
        );
    }
}
