// SPDX-License-Identifier: CC0-1.0

//! HOW a completed Athena query's result objects are ENCRYPTED in the object store.
//!
//! The rule under test: the server-side encryption a query's result objects are stored
//! under is the one the execution's RESOLVED result configuration names
//! (`ResultConfiguration$EncryptionConfiguration` — "indicates the encryption option used
//! […] and key information"), applied to EVERY object the run materialises — the
//! `<QueryExecutionId>.csv` results object and the `.metadata` sibling beside it alike. Of
//! the three options `EncryptionConfiguration$EncryptionOption` declares
//! (`smithy_query(query="shape",shape="EncryptionOption")` reports `[SSE_S3, SSE_KMS,
//! CSE_KMS]`), two are server-side: `SSE_S3` stores the objects under Amazon S3-managed
//! keys, and `SSE_KMS` under the KMS key the configuration names — "the KMS key ARN or ID"
//! (`EncryptionConfiguration$KmsKey`), so every spelling of one key records that key's own
//! ARN. `CSE_KMS` is client-side, so the write asks the object store for no server-side
//! encryption of its own, which is also what a configuration naming no encryption asks for:
//! both are stored under the destination bucket's own default encryption configuration.
//!
//! And the FLOOR: a workgroup whose `EnableMinimumEncryptionConfiguration` is on "enforces a
//! minimal level of encryption for the workgroup for query and calculation results that are
//! written to Amazon S3. When enabled, workgroup users can set encryption only to the
//! minimum level set by the administrator or higher when they submit queries" — so a
//! submission naming a weaker option is refused and writes nothing, one naming the floor or
//! more runs, and one naming no encryption runs at the floor. `EnforceWorkGroupConfiguration`
//! takes precedence: with it on, "the `EnableMinimumEncryptionConfiguration` flag is ignored,
//! and the workgroup configuration for encryption is used".
//!
//! Athena, Amazon S3 and KMS are mounted on ONE shared engine and driven over the wire the
//! production front door uses, so what is asserted is what an SDK client observes of all
//! three: the keys are made with KMS's own `CreateKey`/`CreateAlias`, and the encryption is
//! read back with Amazon S3's own `HeadObject`.
//!
//! Each suite asserts the rule over a TABLE whose rows differ along what it generalises
//! over: which option the configuration names, which spelling the key reference takes, which
//! key it is, where the configuration came from (the request, a workgroup that overrides it,
//! a workgroup that floors it), and which object of the run is read back.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    below_minimum_encryption, query_results::RESULT_METADATA_SUFFIX, ENCRYPTION_OPTION_CSE_KMS,
    ENCRYPTION_OPTION_SSE_KMS, ENCRYPTION_OPTION_SSE_S3, ERROR_CODE_INVALID_INPUT,
    QUERY_STATE_SUCCEEDED, RESULT_OBJECT_ROWS_SUFFIX,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for, and the one the keys are resolved in.
const REGION: &str = "us-east-1";

/// The bucket the rows write their results into, left exactly as `CreateBucket` made it.
const BUCKET: &str = "athena-encrypted-results";

/// A second bucket, whose own DEFAULT encryption configuration names SSE-KMS: where a query
/// that asks for no server-side encryption of its own has its results stored.
const DEFAULTED_BUCKET: &str = "athena-bucket-defaulted-results";

/// The `ServerSideEncryption` value of Amazon S3-managed server-side encryption, which is
/// what `SSE_S3` asks for, and the encryption a general purpose bucket carries until a
/// `PutBucketEncryption` names another ("by default, all buckets have a default encryption
/// configuration that uses server-side encryption with Amazon S3 managed keys (SSE-S3)",
/// `GetBucketEncryption`).
const SSE_S3_ALGORITHM: &str = "AES256";

/// The `ServerSideEncryption` value of KMS-managed server-side encryption, which is what
/// `SSE_KMS` asks for.
const SSE_KMS_ALGORITHM: &str = "aws:kms";

/// The response header `HeadObject` reports an object's encryption under.
const SSE_HEADER: &str = "x-amz-server-side-encryption";

/// The response header it reports that encryption's KMS key under.
const SSE_KMS_KEY_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// The alias one of the rows names its key by — the second spelling `KmsKey` admits.
const KEY_ALIAS: &str = "alias/athena-results-cmk";

/// How long a wait for a started query to settle may take. Generous: what is asserted is the
/// state the execution reaches, never how fast the host reached it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one look waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A SigV4 `Authorization` header whose credential scope names `service`, which is the
/// routing key the router claims a request by. Neither the key id nor the signature is a
/// credential: this harness turns request authentication off, so nothing is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// Athena, Amazon S3 and KMS mounted on ONE shared engine, the way the router mounts every
/// service: the object store Athena saves results into is the one S3's own operations read,
/// and the keys it resolves are the ones KMS holds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        engine,
    ));
    router
}

/// One Athena call, as a client makes it, and its status and parsed body.
async fn athena(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    json_call(router, "athena", "AmazonAthena", operation, body).await
}

/// One KMS call: `awsJson1_1`, reached by `X-Amz-Target: TrentService.<Operation>`.
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    json_call(router, "kms", "TrentService", operation, body).await
}

/// The ONE request builder both JSON front doors are reached through.
async fn json_call(
    router: &MultiServiceRouter,
    service: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(service))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// What one Amazon S3 request answered: the status, the bound headers and the body as text.
struct S3Answer {
    status: u16,
    headers: smithy_server_runtime::http::HeaderMap,
    body: String,
}

impl S3Answer {
    /// The value of the response header `name`, or `None` for one the answer does not carry
    /// at all — which is the difference between an object stored under an encryption and one
    /// stored under none.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

/// One path-style request to the emulator's Amazon S3 front door.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> S3Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    S3Answer {
        status: response.status().as_u16(),
        headers: response.headers().clone(),
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// `PUT /<bucket>` — `CreateBucket` — so a query's results have somewhere to land.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let answer = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert!(
        (200..300).contains(&answer.status),
        "setup: creating s3://{bucket} must succeed, got {} with {}",
        answer.status,
        answer.body
    );
}

/// `PUT /<bucket>?encryption` — `PutBucketEncryption` — naming SSE-KMS under `key` as the
/// bucket's own default encryption configuration.
///
/// `ServerSideEncryptionConfiguration$Rules` is `xmlFlattened` with `xmlName` `Rule`
/// (`smithy_query(query="shape",shape="ServerSideEncryptionConfiguration")`), which is how a
/// client's XML writer spells the payload.
async fn default_bucket_encryption(router: &MultiServiceRouter, bucket: &str, key: &str) {
    let payload = format!(
        "<ServerSideEncryptionConfiguration \
         xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Rule>\
         <ApplyServerSideEncryptionByDefault><SSEAlgorithm>{SSE_KMS_ALGORITHM}</SSEAlgorithm>\
         <KMSMasterKeyID>{key}</KMSMasterKeyID></ApplyServerSideEncryptionByDefault>\
         </Rule></ServerSideEncryptionConfiguration>"
    );
    let answer = s3(router, "PUT", &format!("/{bucket}?encryption"), &payload).await;
    assert!(
        (200..300).contains(&answer.status),
        "setup: configuring the default encryption of s3://{bucket} must succeed, got {} with {}",
        answer.status,
        answer.body
    );
}

/// A customer managed KMS key, made with KMS's own `CreateKey`, as `(key id, key ARN)`.
async fn created_key(router: &MultiServiceRouter, description: &str) -> (String, String) {
    let (status, payload) = kms(router, "CreateKey", &json!({ "Description": description })).await;
    assert_eq!(
        status, 200,
        "setup: creating the key {description:?} must succeed, got {status} with {payload}"
    );
    let member = |name: &str| {
        payload["KeyMetadata"][name]
            .as_str()
            .unwrap_or_else(|| panic!("CreateKey must answer {name}, got {payload}"))
            .to_string()
    };
    (member("KeyId"), member("Arn"))
}

/// An alias of `key_id`, made with KMS's own `CreateAlias`.
async fn created_alias(router: &MultiServiceRouter, alias: &str, key_id: &str) {
    let (status, payload) = kms(
        router,
        "CreateAlias",
        &json!({ "AliasName": alias, "TargetKeyId": key_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: creating the alias {alias:?} must succeed, got {status} with {payload}"
    );
}

/// Create the workgroup `name` with `configuration`.
async fn create_work_group(router: &MultiServiceRouter, name: &str, configuration: Value) {
    let (status, created) = athena(
        router,
        "CreateWorkGroup",
        &json!({ "Name": name, "Configuration": configuration }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the workgroup {name} must be created, got {status} with {created:?}"
    );
}

/// Start one query, asserting it was accepted, and answer the `QueryExecutionId` it minted.
async fn start(router: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let (status, answer) = athena(router, "StartQueryExecution", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartQueryExecution({request}) must be accepted, got {status} with \
         {answer:?}"
    );
    answer["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an execution id, got {answer:?}"))
        .to_string()
}

/// Poll the execution `id` until it reports SUCCEEDED, and answer the location it names for
/// its results.
///
/// The CONDITION is what is waited on, not a span: every look asks the service what state the
/// execution is in, and the deadline is far above the span a completion is due after, so a
/// loaded host cannot turn a slow run into a failure.
async fn succeeded_output_location(router: &MultiServiceRouter, case: &str, id: &str) -> String {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = athena(
            router,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetQueryExecution({id}) must answer 200, got {status} with {body:?}"
        );
        let found = &body["QueryExecution"];
        if found["Status"]["State"].as_str() == Some(QUERY_STATE_SUCCEEDED) {
            return found["ResultConfiguration"]["OutputLocation"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a results location, got {found:?}")
                })
                .to_string();
        }
        assert!(
            found["Status"]["StateChangeReason"].is_null(),
            "case {case:?}: the query {id} must succeed, got {found:?}"
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the query {id} never reached {QUERY_STATE_SUCCEEDED}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The bucket and key an `s3://` location names: the address a consumer turns the location
/// the control plane reported into.
fn parse_location(case: &str, location: &str) -> (String, String) {
    let rest = location.strip_prefix("s3://").unwrap_or_else(|| {
        panic!("case {case:?}: expected an s3:// results location, got {location:?}")
    });
    let (bucket, key) = rest.split_once('/').unwrap_or_else(|| {
        panic!("case {case:?}: expected the location to name an object, got {location:?}")
    });
    (bucket.to_string(), key.to_string())
}

/// The keys `ListObjectsV2` reports under `prefix`.
async fn listed_keys(router: &MultiServiceRouter, bucket: &str, prefix: &str) -> Vec<String> {
    let answer = s3(
        router,
        "GET",
        &format!("/{bucket}?list-type=2&prefix={prefix}"),
        "",
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "ListObjectsV2(s3://{bucket}/{prefix}) must answer 200, got {} with {}",
        answer.status, answer.body
    );
    answer
        .body
        .split("<Key>")
        .skip(1)
        .filter_map(|tail| tail.split_once("</Key>"))
        .map(|(key, _)| key.to_string())
        .collect()
}

/// What the result objects of one row must report: the algorithm, and the KMS key where that
/// algorithm has one — each `None` for a header the answer must not carry at all.
#[derive(Debug, Clone)]
struct Reported {
    algorithm: Option<String>,
    key: Option<String>,
}

/// One row of the encryption table: what it is, the workgroup it runs in, the result
/// configuration it submits, and what every object of the run must then report.
struct Row {
    case: &'static str,
    request: Value,
    expected: Reported,
}

/// A `StartQueryExecution` result configuration naming `BUCKET` under `prefix`, with
/// `encryption` where the row names one.
fn result_configuration(prefix: &str, encryption: Option<Value>) -> Value {
    let mut configuration = json!({ "OutputLocation": format!("s3://{BUCKET}/{prefix}/") });
    if let Some(encryption) = encryption {
        configuration["EncryptionConfiguration"] = encryption;
    }
    configuration
}

/// An `EncryptionConfiguration` naming `option`, with `key` where it names one.
fn encryption(option: &str, key: Option<&str>) -> Value {
    let mut configuration = json!({ "EncryptionOption": option });
    if let Some(key) = key {
        configuration["KmsKey"] = json!(key);
    }
    configuration
}

/// Every object a SUCCEEDED query materialises is stored under the server-side encryption its
/// resolved result configuration named, and a read of each reports that encryption and its
/// key.
#[tokio::test]
async fn a_querys_result_objects_carry_the_encryption_its_result_configuration_named() {
    let router = emulator();
    create_bucket(&router, BUCKET).await;
    create_bucket(&router, DEFAULTED_BUCKET).await;
    let (key_id, key_arn) = created_key(&router, "the key the rows name").await;
    created_alias(&router, KEY_ALIAS, &key_id).await;
    // A SECOND key, so nothing below is keyed to one value: one row names it on the request,
    // and the bucket whose own default encryption names it is where a query asking for no
    // encryption of its own lands.
    let (_, other_key_arn) = created_key(&router, "a second key").await;
    default_bucket_encryption(&router, DEFAULTED_BUCKET, &other_key_arn).await;

    // The workgroup that OVERRIDES the client's configuration with its own, which names
    // SSE-KMS, and the one that merely FLOORS it at SSE-KMS.
    let work_group_results = json!({
        "OutputLocation": format!("s3://{BUCKET}/by-work-group/"),
        "EncryptionConfiguration": encryption(ENCRYPTION_OPTION_SSE_KMS, Some(&key_arn)),
    });
    create_work_group(
        &router,
        "overriding",
        json!({
            "ResultConfiguration": work_group_results,
            "EnforceWorkGroupConfiguration": true,
        }),
    )
    .await;
    create_work_group(
        &router,
        "flooring",
        json!({
            "ResultConfiguration": {
                "OutputLocation": format!("s3://{BUCKET}/by-floor/"),
                "EncryptionConfiguration": encryption(ENCRYPTION_OPTION_SSE_KMS, Some(&key_arn)),
            },
            "EnforceWorkGroupConfiguration": false,
            "EnableMinimumEncryptionConfiguration": true,
        }),
    )
    .await;

    let kms_managed = Reported {
        algorithm: Some(SSE_KMS_ALGORITHM.to_string()),
        key: Some(key_arn.clone()),
    };
    let s3_managed = Reported {
        algorithm: Some(SSE_S3_ALGORITHM.to_string()),
        key: None,
    };

    // The rows differ along which option the configuration names, which spelling of the key
    // reference it uses, which key it names, and where the configuration came from.
    let rows: Vec<Row> = vec![
        Row {
            case: "SSE_KMS naming the key by its ARN",
            request: json!({
                "QueryString": "SELECT 'by-arn' AS marker",
                "ResultConfiguration": result_configuration(
                    "by-arn",
                    Some(encryption(ENCRYPTION_OPTION_SSE_KMS, Some(&key_arn))),
                ),
            }),
            expected: kms_managed.clone(),
        },
        Row {
            case: "SSE_KMS naming that same key by its id",
            request: json!({
                "QueryString": "SELECT 'by-id' AS marker",
                "ResultConfiguration": result_configuration(
                    "by-id",
                    Some(encryption(ENCRYPTION_OPTION_SSE_KMS, Some(&key_id))),
                ),
            }),
            expected: kms_managed.clone(),
        },
        Row {
            case: "SSE_KMS naming that same key by an alias",
            request: json!({
                "QueryString": "SELECT 'by-alias' AS marker",
                "ResultConfiguration": result_configuration(
                    "by-alias",
                    Some(encryption(ENCRYPTION_OPTION_SSE_KMS, Some(KEY_ALIAS))),
                ),
            }),
            expected: kms_managed.clone(),
        },
        Row {
            case: "SSE_KMS naming a different key",
            request: json!({
                "QueryString": "SELECT 'other-key' AS marker",
                "ResultConfiguration": result_configuration(
                    "other-key",
                    Some(encryption(ENCRYPTION_OPTION_SSE_KMS, Some(&other_key_arn))),
                ),
            }),
            expected: Reported {
                algorithm: Some(SSE_KMS_ALGORITHM.to_string()),
                key: Some(other_key_arn.clone()),
            },
        },
        Row {
            case: "SSE_S3, which has no key of its own to report",
            request: json!({
                "QueryString": "SELECT 'sse-s3' AS marker",
                "ResultConfiguration": result_configuration(
                    "sse-s3",
                    Some(encryption(ENCRYPTION_OPTION_SSE_S3, None)),
                ),
            }),
            expected: s3_managed.clone(),
        },
        Row {
            case: "CSE_KMS, which asks for no server-side encryption, into an unconfigured \
                   bucket",
            request: json!({
                "QueryString": "SELECT 'cse-kms' AS marker",
                "ResultConfiguration": result_configuration(
                    "cse-kms",
                    Some(encryption(ENCRYPTION_OPTION_CSE_KMS, Some(&key_arn))),
                ),
            }),
            expected: s3_managed.clone(),
        },
        Row {
            case: "no encryption configuration at all, into an unconfigured bucket",
            request: json!({
                "QueryString": "SELECT 'unencrypted' AS marker",
                "ResultConfiguration": result_configuration("plain", None),
            }),
            expected: s3_managed.clone(),
        },
        Row {
            case: "no encryption configuration at all, into a bucket whose own default names \
                   SSE-KMS",
            request: json!({
                "QueryString": "SELECT 'bucket-default' AS marker",
                "ResultConfiguration": {
                    "OutputLocation": format!("s3://{DEFAULTED_BUCKET}/defaulted/"),
                },
            }),
            expected: Reported {
                algorithm: Some(SSE_KMS_ALGORITHM.to_string()),
                key: Some(other_key_arn.clone()),
            },
        },
        Row {
            case: "a workgroup whose configuration overrides the SSE_S3 the request named",
            request: json!({
                "QueryString": "SELECT 'overridden' AS marker",
                "WorkGroup": "overriding",
                "ResultConfiguration": result_configuration(
                    "overridden",
                    Some(encryption(ENCRYPTION_OPTION_SSE_S3, None)),
                ),
            }),
            expected: kms_managed.clone(),
        },
        Row {
            case: "a workgroup that floors the encryption, for a request naming none",
            request: json!({
                "QueryString": "SELECT 'floored' AS marker",
                "WorkGroup": "flooring",
            }),
            expected: kms_managed.clone(),
        },
    ];

    // Every row's query is started before any of them is read back, so the span a completion
    // is due after elapses once for the whole table rather than once per row.
    let mut started: Vec<(&Row, String)> = Vec::new();
    for row in &rows {
        started.push((row, start(&router, row.case, &row.request).await));
    }

    for (row, id) in &started {
        let case = row.case;
        let location = succeeded_output_location(&router, case, id).await;
        let (bucket, key) = parse_location(case, &location);
        assert!(
            key.ends_with(RESULT_OBJECT_ROWS_SUFFIX),
            "case {case:?}: expected the results object {RESULT_OBJECT_ROWS_SUFFIX:?}, got \
             {key:?}"
        );
        // BOTH objects the run materialises: the results and the metadata information saved
        // beside them.
        for suffix in ["", RESULT_METADATA_SUFFIX] {
            let read = format!("{key}{suffix}");
            let answer = s3(&router, "HEAD", &format!("/{bucket}/{read}"), "").await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: HeadObject(s3://{bucket}/{read}) must answer 200, got {}",
                answer.status
            );
            let algorithm = answer.header(SSE_HEADER).map(str::to_string);
            assert_eq!(
                algorithm, row.expected.algorithm,
                "case {case:?}: expected {SSE_HEADER} {:?} on s3://{bucket}/{read}, got \
                 {algorithm:?}",
                row.expected.algorithm
            );
            let reported_key = answer.header(SSE_KMS_KEY_HEADER).map(str::to_string);
            assert_eq!(
                reported_key, row.expected.key,
                "case {case:?}: expected {SSE_KMS_KEY_HEADER} {:?} on s3://{bucket}/{read}, \
                 got {reported_key:?}",
                row.expected.key
            );
        }
    }
}

/// One row of the floor table: the workgroup's settings, the option the request names, and
/// what the submission must then do.
struct FloorRow {
    case: &'static str,
    /// The `EncryptionOption` the workgroup's own result configuration names, which is the
    /// floor when the flag is on.
    floor: &'static str,
    /// `EnableMinimumEncryptionConfiguration`.
    minimum: bool,
    /// `EnforceWorkGroupConfiguration`, which takes precedence over it.
    enforce: bool,
    /// The `EncryptionOption` the request names, or `None` for a request naming no
    /// encryption of its own.
    requested: Option<&'static str>,
    /// The algorithm the results must be stored under, or `None` for a submission the floor
    /// REFUSES — which writes nothing at all.
    stored: Option<&'static str>,
}

/// A workgroup's minimum-encryption floor admits the option it names and every stronger one,
/// refuses a weaker one in Athena's own words, and is ignored where the workgroup's settings
/// override the client's.
#[tokio::test]
async fn a_submission_below_the_work_groups_minimum_encryption_is_refused_and_writes_nothing() {
    let router = emulator();
    create_bucket(&router, BUCKET).await;
    let (_, key_arn) = created_key(&router, "the key the floors name").await;

    // The rows differ along where the floor sits, which option the request names, and which
    // of the two workgroup switches is on.
    let rows: [FloorRow; 7] = [
        FloorRow {
            case: "an option below the floor",
            floor: ENCRYPTION_OPTION_SSE_KMS,
            minimum: true,
            enforce: false,
            requested: Some(ENCRYPTION_OPTION_SSE_S3),
            stored: None,
        },
        FloorRow {
            case: "the floor's own option",
            floor: ENCRYPTION_OPTION_SSE_KMS,
            minimum: true,
            enforce: false,
            requested: Some(ENCRYPTION_OPTION_SSE_KMS),
            stored: Some(SSE_KMS_ALGORITHM),
        },
        FloorRow {
            case: "an option above the floor, whose encryption is the client's own",
            floor: ENCRYPTION_OPTION_SSE_KMS,
            minimum: true,
            enforce: false,
            requested: Some(ENCRYPTION_OPTION_CSE_KMS),
            stored: Some(SSE_S3_ALGORITHM),
        },
        FloorRow {
            case: "a request naming no encryption, which runs at the floor",
            floor: ENCRYPTION_OPTION_SSE_KMS,
            minimum: true,
            enforce: false,
            requested: None,
            stored: Some(SSE_KMS_ALGORITHM),
        },
        FloorRow {
            case: "the strongest floor, which refuses the server-side options",
            floor: ENCRYPTION_OPTION_CSE_KMS,
            minimum: true,
            enforce: false,
            requested: Some(ENCRYPTION_OPTION_SSE_KMS),
            stored: None,
        },
        FloorRow {
            case: "the floor flag off, so the client's weaker option stands",
            floor: ENCRYPTION_OPTION_SSE_KMS,
            minimum: false,
            enforce: false,
            requested: Some(ENCRYPTION_OPTION_SSE_S3),
            stored: Some(SSE_S3_ALGORITHM),
        },
        FloorRow {
            case: "the workgroup's settings overriding the client's, which ignores the flag",
            floor: ENCRYPTION_OPTION_SSE_KMS,
            minimum: true,
            enforce: true,
            requested: Some(ENCRYPTION_OPTION_SSE_S3),
            stored: Some(SSE_KMS_ALGORITHM),
        },
    ];

    // Each row runs in a workgroup of its own, whose output location is a prefix of its own,
    // so what a refused submission wrote — nothing — is read off that prefix alone.
    for (position, row) in rows.iter().enumerate() {
        let name = format!("floor-{position}");
        create_work_group(
            &router,
            &name,
            json!({
                "ResultConfiguration": {
                    "OutputLocation": format!("s3://{BUCKET}/{name}/"),
                    "EncryptionConfiguration": encryption(row.floor, Some(&key_arn)),
                },
                "EnforceWorkGroupConfiguration": row.enforce,
                "EnableMinimumEncryptionConfiguration": row.minimum,
            }),
        )
        .await;

        let mut request = json!({
            "QueryString": format!("SELECT '{name}' AS marker"),
            "WorkGroup": name,
        });
        if let Some(option) = row.requested {
            request["ResultConfiguration"] =
                json!({ "EncryptionConfiguration": encryption(option, Some(&key_arn)) });
        }
        let case = row.case;
        let (status, answer) = athena(&router, "StartQueryExecution", &request).await;

        let Some(expected_algorithm) = row.stored else {
            assert_eq!(
                status, 400,
                "case {case:?}: the submission must be refused, got {status} with {answer:?}"
            );
            let expected_message = below_minimum_encryption(row.floor);
            let message = answer["Message"]
                .as_str()
                .or_else(|| answer["message"].as_str())
                .unwrap_or_default();
            assert_eq!(
                message, expected_message,
                "case {case:?}: expected the message {expected_message:?}, got {message:?}"
            );
            assert_eq!(
                answer["AthenaErrorCode"].as_str(),
                Some(ERROR_CODE_INVALID_INPUT),
                "case {case:?}: expected the refusal class {ERROR_CODE_INVALID_INPUT:?}, got \
                 {answer:?}"
            );
            let keys = listed_keys(&router, BUCKET, &name).await;
            assert!(
                keys.is_empty(),
                "case {case:?}: a refused submission must write nothing, found {keys:?}"
            );
            continue;
        };

        assert_eq!(
            status, 200,
            "case {case:?}: the submission must be accepted, got {status} with {answer:?}"
        );
        let id = answer["QueryExecutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an execution id, got {answer:?}"))
            .to_string();
        let location = succeeded_output_location(&router, case, &id).await;
        let (bucket, key) = parse_location(case, &location);
        let head = s3(&router, "HEAD", &format!("/{bucket}/{key}"), "").await;
        assert_eq!(
            head.status, 200,
            "case {case:?}: HeadObject(s3://{bucket}/{key}) must answer 200, got {}",
            head.status
        );
        let algorithm = head.header(SSE_HEADER);
        assert_eq!(
            algorithm,
            Some(expected_algorithm),
            "case {case:?}: expected {SSE_HEADER} {expected_algorithm:?} on \
             s3://{bucket}/{key}, got {algorithm:?}"
        );
    }
}
