// SPDX-License-Identifier: CC0-1.0

//! Where the log records of a web ACL in front of a deployed REST API stage ARRIVE when
//! its logging destination names an Amazon S3 BUCKET, end to end over the wire.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > While a web ACL carries an enabled logging configuration whose
//! > `LogDestinationConfigs` names a bucket, every request that web ACL inspects leaves one
//! > record in that bucket, written in the form a reader of a bucket of vended logs fetches
//! > — gzipped newline-delimited JSON under WAF's published key layout — and each record
//! > reports the request as WAF inspected it: its `httpRequest.headers` reproduce the
//! > inspected request's headers, and every rule that matched WITHOUT terminating the
//! > evaluation is listed under `nonTerminatingMatchingRules` with the action it applied.
//! > The configuration is a live association and not a document: once
//! > `DeleteLoggingConfiguration` has removed it, nothing more is delivered and the read of
//! > it is refused.
//!
//! How many OBJECTS those records are batched into, and the keys' own minute fields, are
//! per-run values rather than the requirement, so the objects are asserted to exist, to be no
//! more than one per inspected request and to be keyed the way a reader of a bucket of vended
//! logs reads them — while the count that carries the rule is the count of RECORDS.
//!
//! "You can use an Amazon CloudWatch Logs log group, an Amazon Simple Storage Service
//! (Amazon S3) bucket, or an Amazon Kinesis Data Firehose"
//! (`smithy_query(query="doc",shape="LoggingConfiguration",
//! model_path="services/service-com-amazonaws-wafv2-awswaf-20190729/smithy-model/com-amazonaws-wafv2-awswaf-20190729.json")`)
//! is where the bucket destination comes from, and the sentence a refused read answers with
//! is `WAFNonexistentItemException`'s own.
//!
//! Only a cross-service suite can pin this: the api and its stage are API Gateway's, the
//! web ACL and its logging configuration are WAFv2's, and the objects land in Amazon S3 —
//! all three on ONE engine, as `core/router/src/lib.rs` mounts them. Every assertion reads
//! the outcome back through Amazon S3's own readers (`ListObjectsV2`, `GetObject`), so a
//! record that never reached the object store cannot pass. The sibling suite
//! `stage_web_acl_logging.rs` pins the same rule for a log-group destination, and the WAFv2
//! crate's `tests/all/logging_destination_delivery.rs` pins the matrix of destination kinds;
//! what this one adds is the bucket destination reached by traffic through a STAGE.
//!
//! The rows differ along what the rule generalises over: how many requests are inspected
//! (one, two, three), how many rules matched without terminating (none, one, two), and
//! whether the request was settled by a terminating rule or by the web ACL's
//! `DefaultAction`.

use std::io::Read;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region the invoke host, both control planes and every ARN name. Amazon S3's own
/// default Region, so a bucket is created here without a `CreateBucketConfiguration`.
const REGION: &str = "us-east-1";

/// The `Scope` an API Gateway stage's web ACL lives in: a stage is a REGIONAL resource.
const SCOPE: &str = "REGIONAL";

/// The name every logging destination below starts with: "The name that you give the
/// destination must start with aws-waf-logs-" (`LoggingConfiguration`).
const WAF_LOGS_PREFIX: &str = "aws-waf-logs-";

/// The resource path every case drives, and the query string it carries.
const RESOURCE_PATH: &str = "orders";
const QUERY: &str = "id=7";

/// The header every request carries and the rules below inspect, and the value they match.
const PROBE_HEADER: &str = "x-waf-bucket-probe";
const PROBE_VALUE: &str = "log-me";

/// A value the same header carries in the row where no rule matches at all.
const UNMATCHED_VALUE: &str = "leave-me-alone";

/// The `action` a rule with a `CountAction` reports, from `SampledHTTPRequest$Action`'s own
/// value set.
const COUNT_ACTION: &str = "COUNT";

/// The `terminatingRuleId` a record names for a request no rule terminated, which the web
/// ACL's `DefaultAction` decided.
const DEFAULT_ACTION_RULE: &str = "Default_Action";

/// The `httpSourceName` of a request that arrived at an API Gateway REST API stage.
const APIGW_SOURCE: &str = "APIGW";

/// What a delivered log file's key ends with: WAF's records, gzipped.
const LOG_FILE_SUFFIX: &str = ".log.gz";

/// `WAFNonexistentItemException`'s one sentence, as WAF words it.
const NONEXISTENT_ITEM: &str =
    "AWS WAF couldn’t perform the operation because your resource doesn’t exist.";

/// One emulator with API Gateway, WAFv2 and Amazon S3 mounted on a single shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite exercises the services, not SigV4: the control-plane calls carry a
        // credential scope so the router can name the target service, and the invokes carry
        // no credential at all, as a client of a published invoke URL sends none.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_wafv2_awswaf_20190729::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// Dispatch one request as it arrives on the wire, answering its status, its parsed
    /// body and its body verbatim.
    async fn dispatch(
        &self,
        method: &str,
        host: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Bytes,
    ) -> (u16, Value, Vec<u8>) {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = self
            .router
            .dispatch(builder.body(body).expect("a well-formed request"))
            .await;
        let status = response.status().as_u16();
        let bytes = response.into_body().to_vec();
        let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, parsed, bytes)
    }

    /// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
    /// routes the request. The signature itself is never checked here.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host, Signature=0000"
        )
    }

    /// One API Gateway control-plane call that has to be accepted with `expected`.
    async fn api_ok(&self, method: &str, path: &str, body: Option<Value>, expected: u16) -> Value {
        let payload = body.map(|value| value.to_string()).unwrap_or_default();
        let (status, parsed, bytes) = self
            .dispatch(
                method,
                &format!("apigateway.{REGION}.amazonaws.com"),
                path,
                &[
                    ("content-type", "application/json"),
                    ("authorization", &Self::authorization("apigateway")),
                ],
                Bytes::from(payload),
            )
            .await;
        assert_eq!(
            status,
            expected,
            "setup {method} {path}: expected HTTP {expected}, got {status} with {:?}",
            String::from_utf8_lossy(&bytes)
        );
        parsed
    }

    /// One WAFv2 call, answering its status and parsed body.
    async fn waf(&self, operation: &str, body: Value) -> (u16, Value) {
        let (status, parsed, _) = self
            .dispatch(
                "POST",
                &format!("wafv2.{REGION}.amazonaws.com"),
                "/",
                &[
                    ("content-type", "application/x-amz-json-1.1"),
                    ("authorization", &Self::authorization("wafv2")),
                    ("x-amz-target", &format!("AWSWAF_20190729.{operation}")),
                ],
                Bytes::from(body.to_string()),
            )
            .await;
        (status, parsed)
    }

    /// One WAFv2 call that has to succeed.
    async fn waf_ok(&self, operation: &str, body: Value) -> Value {
        let (status, parsed) = self.waf(operation, body).await;
        assert_eq!(
            status, 200,
            "setup {operation}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// Create the bucket `name` through Amazon S3's own (REST, path-style) wire, so the
    /// destination the configuration names is a bucket the account really holds.
    async fn given_bucket(&self, name: &str) {
        let (status, _, bytes) = self
            .dispatch(
                "PUT",
                &format!("s3.{REGION}.amazonaws.com"),
                &format!("/{name}"),
                &[("authorization", &Self::authorization("s3"))],
                Bytes::new(),
            )
            .await;
        assert_eq!(
            status,
            200,
            "setup CreateBucket({name}): expected HTTP 200, got {status} with {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }

    /// Every object key `bucket` holds, read through Amazon S3's own `ListObjectsV2`.
    async fn keys_in_bucket(&self, bucket: &str) -> Vec<String> {
        let (status, _, bytes) = self
            .dispatch(
                "GET",
                &format!("s3.{REGION}.amazonaws.com"),
                &format!("/{bucket}?list-type=2"),
                &[("authorization", &Self::authorization("s3"))],
                Bytes::new(),
            )
            .await;
        let listing = String::from_utf8_lossy(&bytes).into_owned();
        assert_eq!(
            status, 200,
            "ListObjectsV2({bucket}): expected HTTP 200, got {status} with {listing:?}"
        );
        let mut keys: Vec<String> = listing
            .split("<Key>")
            .skip(1)
            .filter_map(|tail| tail.split("</Key>").next())
            .map(str::to_string)
            .collect();
        keys.sort();
        keys
    }

    /// The records the object `key` of `bucket` carries: its bytes, decompressed, read as
    /// the newline-delimited JSON a delivered WAF log file is.
    async fn records_in_object(&self, case: &str, bucket: &str, key: &str) -> Vec<Value> {
        let (status, _, bytes) = self
            .dispatch(
                "GET",
                &format!("s3.{REGION}.amazonaws.com"),
                &format!("/{bucket}/{key}"),
                &[("authorization", &Self::authorization("s3"))],
                Bytes::new(),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetObject({bucket}/{key}) must serve the object, got {status}"
        );
        let mut decoded = String::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .read_to_string(&mut decoded)
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the object {key:?} must be a gzip stream: {error}")
            });
        decoded
            .lines()
            .map(|line| {
                serde_json::from_str(line).unwrap_or_else(|error| {
                    panic!("case {case:?}: every line of {key:?} must be JSON: {error}")
                })
            })
            .collect()
    }

    /// Every record delivered into `bucket`, in the order of the keys they were read under.
    async fn records_in_bucket(&self, case: &str, bucket: &str) -> Vec<Value> {
        let mut records: Vec<Value> = Vec::new();
        for key in self.keys_in_bucket(bucket).await {
            records.extend(self.records_in_object(case, bucket, &key).await);
        }
        records
    }

    /// One unsigned request to a deployed stage's invoke URL, carrying `probe` as the
    /// header the web ACL's rules inspect.
    async fn invoke(&self, api: &str, stage: &str, probe: &str) -> u16 {
        let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
        let (status, _, _) = self
            .dispatch(
                "GET",
                &host,
                &format!("/{stage}/{RESOURCE_PATH}?{QUERY}"),
                &[
                    (PROBE_HEADER, probe),
                    ("content-type", "application/json"),
                ],
                Bytes::new(),
            )
            .await;
        status
    }
}

/// The ARN `AssociateWebACL` binds a web ACL to a REST API stage by, the one form
/// `AssociateWebACLRequest$ResourceArn` documents for an API Gateway REST API stage.
fn stage_arn(api: &str, stage: &str) -> String {
    format!("arn:aws:apigateway:{REGION}::/restapis/{api}/stages/{stage}")
}

/// The ARN of the bucket `name`, the form `PutLoggingConfiguration` takes an Amazon S3
/// destination in: a bucket's name is global, so such an ARN carries no Region and no
/// account.
fn bucket_arn(name: &str) -> String {
    format!("arn:aws:s3:::{name}")
}

/// A `VisibilityConfig` for a dimension, which every rule and web ACL needs.
fn visibility(metric: &str) -> Value {
    json!({
        "SampledRequestsEnabled": true,
        "CloudWatchMetricsEnabled": true,
        "MetricName": metric,
    })
}

/// A `Blob` member travels base64-encoded in an awsJson body, which is what
/// `ByteMatchStatement$SearchString` is.
fn base64_of(value: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
}

/// A rule named `name` at `priority` that applies `action` to a request whose probe header
/// carries exactly the matched value.
fn rule_on_the_header(name: &str, priority: u32, action: Value) -> Value {
    json!({
        "Name": name,
        "Priority": priority,
        "Statement": {
            "ByteMatchStatement": {
                "SearchString": base64_of(PROBE_VALUE),
                "FieldToMatch": { "SingleHeader": { "Name": PROBE_HEADER } },
                "TextTransformations": [{ "Priority": 0, "Type": "NONE" }],
                "PositionalConstraint": "EXACTLY",
            }
        },
        "Action": action,
        "VisibilityConfig": visibility(name),
    })
}

/// A rule that COUNTS the probed request: a match that does not terminate the evaluation.
fn count_rule(name: &str, priority: u32) -> Value {
    rule_on_the_header(name, priority, json!({ "Count": {} }))
}

/// A rule that BLOCKS the probed request, which terminates the evaluation.
fn block_rule(name: &str, priority: u32) -> Value {
    rule_on_the_header(name, priority, json!({ "Block": {} }))
}

/// The api every case is driven against: one `GET` resource on a `MOCK` integration that
/// answers `200`, so the answer a request gets says whether it reached the origin.
async fn deployed_api(emulator: &Emulator, name: &str, stages: &[&str]) -> String {
    let created = emulator
        .api_ok("POST", "/restapis", Some(json!({ "name": name })), 201)
        .await;
    let api = created["id"].as_str().expect("the api id").to_string();
    let root = created["rootResourceId"]
        .as_str()
        .expect("the root resource id")
        .to_string();
    let resource = emulator
        .api_ok(
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": RESOURCE_PATH })),
            201,
        )
        .await;
    let resource = resource["id"]
        .as_str()
        .expect("the resource id")
        .to_string();
    emulator
        .api_ok(
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET"),
            Some(json!({ "authorizationType": "NONE" })),
            201,
        )
        .await;
    emulator
        .api_ok(
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET/integration"),
            Some(json!({ "type": "MOCK" })),
            201,
        )
        .await;
    emulator
        .api_ok(
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET/integration/responses/200"),
            Some(json!({
                "responseTemplates": { "application/json": r#"{"at":"origin"}"# }
            })),
            201,
        )
        .await;
    for stage in stages {
        emulator
            .api_ok(
                "POST",
                &format!("/restapis/{api}/deployments"),
                Some(json!({ "stageName": stage })),
                201,
            )
            .await;
    }
    api
}

/// Create a web ACL named `name` with `rules` and the default `Allow`, put it in front of
/// the stage, and answer its ARN.
async fn web_acl_in_front(
    emulator: &Emulator,
    name: &str,
    rules: Value,
    api: &str,
    stage: &str,
) -> String {
    let created = emulator
        .waf_ok(
            "CreateWebACL",
            json!({
                "Name": name,
                "Scope": SCOPE,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility(name),
                "Rules": rules,
            }),
        )
        .await;
    let arn = created["Summary"]["ARN"]
        .as_str()
        .expect("Summary.ARN")
        .to_string();
    emulator
        .waf_ok(
            "AssociateWebACL",
            json!({ "WebACLArn": arn, "ResourceArn": stage_arn(api, stage) }),
        )
        .await;
    arn
}

/// Put a logging configuration naming `destination` in front of the web ACL `arn`.
async fn configure_logging(emulator: &Emulator, arn: &str, destination: &str) {
    assert!(
        destination.contains(WAF_LOGS_PREFIX),
        "setup PutLoggingConfiguration: the destination name must start with \
         {WAF_LOGS_PREFIX}, got {destination:?}"
    );
    emulator
        .waf_ok(
            "PutLoggingConfiguration",
            json!({
                "LoggingConfiguration": {
                    "ResourceArn": arn,
                    "LogDestinationConfigs": [destination],
                }
            }),
        )
        .await;
}

/// What stands between the log-file prefix and the file's own name: the MINUTE the records
/// belong to, as the `<Y>/<m>/<d>/<H>/<M>/` path of WAF's published key layout.
///
/// Each segment is asserted to be the decimal field it is (four digits for the year, two for
/// each of the others), so a key that carried the minute in any other form — or not at all —
/// fails the case rather than passing on the prefix and the suffix alone.
fn assert_minute_path(case: &str, prefix: &str, key: &str) {
    let under = key.strip_prefix(prefix).unwrap_or_default();
    let segments: Vec<&str> = under.split('/').collect();
    let widths = [4_usize, 2, 2, 2, 2];
    assert_eq!(
        segments.len(),
        widths.len() + 1,
        "case {case:?}: expected the minute as {} path segments and then the file name, got \
         {under:?}",
        widths.len()
    );
    for (segment, width) in segments.iter().zip(widths) {
        assert!(
            segment.len() == width && segment.chars().all(|digit| digit.is_ascii_digit()),
            "case {case:?}: expected a {width}-digit segment of the minute path, got \
             {segment:?} in {under:?}"
        );
    }
}

/// The prefix every log file of the web ACL `web_acl_name` is delivered under:
/// `AWSLogs/<account-id>/WAFLogs/<region>/<web-acl-name>/`.
fn log_file_prefix(web_acl_name: &str) -> String {
    format!(
        "AWSLogs/{account}/WAFLogs/{REGION}/{web_acl_name}/",
        account = engine::emulator_account_id()
    )
}

/// The `value` the header `name` carries in a record's `httpRequest.headers`.
fn logged_header(record: &Value, name: &str) -> String {
    record["httpRequest"]["headers"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .find(|header| {
            header["name"]
                .as_str()
                .is_some_and(|header| header.eq_ignore_ascii_case(name))
        })
        .and_then(|header| header["value"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// One row of the delivery table: the evaluation the stage's requests are driven through,
/// and what each delivered record must say about them.
struct DeliveryRow {
    case: &'static str,
    /// The stage, the web ACL and the destination bucket this row drives.
    name: &'static str,
    /// The rules the web ACL carries.
    rules: Value,
    /// The value the probe header carries, which decides what matches.
    probe: &'static str,
    /// How many requests are driven through the stage.
    requests: usize,
    /// The status the stage answers each of them with.
    status: u16,
    /// The `action` every delivered record names.
    action: &'static str,
    /// The `terminatingRuleId` every delivered record names.
    terminating: &'static str,
    /// The rules every record lists under `nonTerminatingMatchingRules`, in the order they
    /// ran.
    non_terminating: &'static [&'static str],
}

/// The rows: a single non-terminating match, two of them, a non-terminating match ahead of
/// a rule that terminated the evaluation, and a request no rule matched — driven one, two
/// and three times, because the count of records tracks the count of requests inspected.
fn delivery_rows() -> Vec<DeliveryRow> {
    vec![
        DeliveryRow {
            case: "one counted match, settled by the default action",
            name: "counted-once",
            rules: json!([count_rule("count-the-probe", 0)]),
            probe: PROBE_VALUE,
            requests: 1,
            status: 200,
            action: "ALLOW",
            terminating: DEFAULT_ACTION_RULE,
            non_terminating: &["count-the-probe"],
        },
        DeliveryRow {
            case: "two counted matches, in the order the rules ran",
            name: "counted-twice",
            rules: json!([count_rule("count-first", 0), count_rule("count-second", 1)]),
            probe: PROBE_VALUE,
            requests: 2,
            status: 200,
            action: "ALLOW",
            terminating: DEFAULT_ACTION_RULE,
            non_terminating: &["count-first", "count-second"],
        },
        DeliveryRow {
            case: "a counted match ahead of the rule that blocked the request",
            name: "counted-then-blocked",
            rules: json!([count_rule("count-before-block", 0), block_rule("block-it", 1)]),
            probe: PROBE_VALUE,
            requests: 3,
            status: 403,
            action: "BLOCK",
            terminating: "block-it",
            non_terminating: &["count-before-block"],
        },
        DeliveryRow {
            case: "a request no rule matched",
            name: "unmatched",
            rules: json!([count_rule("count-never", 0)]),
            probe: UNMATCHED_VALUE,
            requests: 1,
            status: 200,
            action: "ALLOW",
            terminating: DEFAULT_ACTION_RULE,
            non_terminating: &[],
        },
    ]
}

/// Every request a stage's web ACL inspects leaves one record in the destination bucket, in
/// gzipped newline-delimited JSON objects under WAF's published key layout, reproducing the
/// request's headers and naming every rule that matched without terminating.
#[tokio::test]
async fn a_bucket_destination_takes_one_record_per_request_inspected_at_a_stage() {
    let emulator = Emulator::new();
    let rows = delivery_rows();
    let stages: Vec<&str> = rows.iter().map(|row| row.name).collect();
    let api = deployed_api(&emulator, "waf-bucket-delivery-api", &stages).await;

    for row in &rows {
        let case = row.case;
        let bucket = format!("{WAF_LOGS_PREFIX}{}", row.name);
        let acl = format!("acl-{}", row.name);
        emulator.given_bucket(&bucket).await;
        let web_acl = web_acl_in_front(&emulator, &acl, row.rules.clone(), &api, row.name).await;
        configure_logging(&emulator, &web_acl, &bucket_arn(&bucket)).await;

        for request in 1..=row.requests {
            let status = emulator.invoke(&api, row.name, row.probe).await;
            assert_eq!(
                status, row.status,
                "case {case:?}: request {request} must be answered {}, got {status}",
                row.status
            );
        }

        // The requirement is a RECORD per inspected request (asserted below); how many
        // objects those records are batched into is a per-run value, so the objects are
        // only required to exist, to be no more than one per request, and to be keyed the
        // way a reader of a bucket of vended logs reads them.
        let keys = emulator.keys_in_bucket(&bucket).await;
        assert!(
            !keys.is_empty() && keys.len() <= row.requests,
            "case {case:?}: {} inspected requests must leave between one and {} objects, \
             got {keys:?}",
            row.requests,
            row.requests
        );
        let prefix = log_file_prefix(&acl);
        for key in &keys {
            assert!(
                key.starts_with(&prefix),
                "case {case:?}: expected the object under {prefix:?}, got {key:?}"
            );
            assert!(
                key.ends_with(LOG_FILE_SUFFIX),
                "case {case:?}: expected a gzipped log file, got {key:?}"
            );
            assert_minute_path(case, &prefix, key);
        }

        let records = emulator.records_in_bucket(case, &bucket).await;
        assert_eq!(
            records.len(),
            row.requests,
            "case {case:?}: one record per inspected request, got {records:?}"
        );
        let expected_non_terminating = json!(row
            .non_terminating
            .iter()
            .map(|rule| json!({ "ruleId": rule, "action": COUNT_ACTION }))
            .collect::<Vec<Value>>());
        for record in &records {
            assert_eq!(
                record["action"],
                json!(row.action),
                "case {case:?}: expected the action {:?}, got {record}",
                row.action
            );
            assert_eq!(
                record["terminatingRuleId"],
                json!(row.terminating),
                "case {case:?}: expected the terminating rule {:?}, got {record}",
                row.terminating
            );
            assert_eq!(
                record["nonTerminatingMatchingRules"], expected_non_terminating,
                "case {case:?}: expected the non-terminating matches \
                 {expected_non_terminating}, got {record}"
            );
            assert_eq!(
                record["webaclId"],
                json!(web_acl),
                "case {case:?}: the record must name the web ACL that decided, got {record}"
            );
            assert_eq!(
                record["httpSourceName"],
                json!(APIGW_SOURCE),
                "case {case:?}: the record must name the kind of resource the request \
                 arrived at, got {record}"
            );
            let probed = logged_header(record, PROBE_HEADER);
            assert_eq!(
                probed, row.probe,
                "case {case:?}: the record must reproduce the request's headers, got \
                 {probed:?} in {record}"
            );
            assert_eq!(
                record["httpRequest"]["uri"],
                json!(format!("/{}/{RESOURCE_PATH}", row.name)),
                "case {case:?}: the record must report the path inspected, got {record}"
            );
        }
    }
}

/// The configuration is a live association: while it stands, every request delivers another
/// object and the ones already delivered stand; once `DeleteLoggingConfiguration` has
/// removed it, nothing more is delivered and the read of it is refused with
/// `WAFNonexistentItemException`.
#[tokio::test]
async fn deleting_the_configuration_stops_delivery_and_the_read_is_then_refused() {
    let case = "a bucket destination whose configuration is deleted";
    let stage = "deleted";
    let bucket = format!("{WAF_LOGS_PREFIX}{stage}");
    let acl = format!("acl-{stage}");
    let emulator = Emulator::new();
    let api = deployed_api(&emulator, "waf-bucket-delete-api", &[stage]).await;
    emulator.given_bucket(&bucket).await;
    let web_acl = web_acl_in_front(
        &emulator,
        &acl,
        json!([count_rule("count-the-probe", 0)]),
        &api,
        stage,
    )
    .await;
    configure_logging(&emulator, &web_acl, &bucket_arn(&bucket)).await;

    let stored = emulator
        .waf_ok(
            "GetLoggingConfiguration",
            json!({ "ResourceArn": web_acl }),
        )
        .await;
    assert_eq!(
        stored["LoggingConfiguration"]["LogDestinationConfigs"],
        json!([bucket_arn(&bucket)]),
        "case {case:?}: the stored configuration must name the bucket, got {stored}"
    );

    // While the configuration stands, each request leaves another RECORD and the objects
    // already delivered are left alone (how many objects those records are batched into is a
    // per-run value, so only their keys are tracked, never their count).
    let mut delivered: Vec<String> = Vec::new();
    for request in 1..=2usize {
        let status = emulator.invoke(&api, stage, PROBE_VALUE).await;
        assert_eq!(
            status, 200,
            "case {case:?}: request {request} must be answered 200, got {status}"
        );
        let records = emulator.records_in_bucket(case, &bucket).await;
        assert_eq!(
            records.len(),
            request,
            "case {case:?}: request {request} must leave the {request}th record, got \
             {records:?}"
        );
        let keys = emulator.keys_in_bucket(&bucket).await;
        for earlier in &delivered {
            assert!(
                keys.contains(earlier),
                "case {case:?}: the object {earlier:?} already delivered must still be \
                 there, got {keys:?}"
            );
        }
        delivered = keys;
    }

    emulator
        .waf_ok(
            "DeleteLoggingConfiguration",
            json!({ "ResourceArn": web_acl }),
        )
        .await;

    // The read of a configuration that no longer exists is refused with WAF's own sentence.
    let (status, refused) = emulator
        .waf("GetLoggingConfiguration", json!({ "ResourceArn": web_acl }))
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the read must answer HTTP 400, got {status} with {refused}"
    );
    let code = refused["__type"].as_str().unwrap_or_default();
    assert!(
        code.ends_with("WAFNonexistentItemException"),
        "case {case:?}: expected WAFNonexistentItemException, got {code:?} in {refused}"
    );
    let message = refused
        .get("Message")
        .or_else(|| refused.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        message, NONEXISTENT_ITEM,
        "case {case:?}: expected WAF's own sentence, got {message:?}"
    );

    // ... and the web ACL goes on deciding requests while delivering nothing more: the
    // objects already in the bucket stand and no record joins them.
    let status = emulator.invoke(&api, stage, PROBE_VALUE).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the stage must go on serving requests, got {status}"
    );
    let keys = emulator.keys_in_bucket(&bucket).await;
    assert_eq!(
        keys, delivered,
        "case {case:?}: a deleted configuration delivers nothing more, got {keys:?}"
    );
    let records = emulator.records_in_bucket(case, &bucket).await;
    assert_eq!(
        records.len(),
        2,
        "case {case:?}: the records delivered while the configuration stood must be all \
         there are, got {records:?}"
    );
}
