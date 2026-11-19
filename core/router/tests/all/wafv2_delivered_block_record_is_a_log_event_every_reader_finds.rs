// SPDX-License-Identifier: CC0-1.0

//! How a record a WAFv2 logging configuration delivers about a BLOCKED request at a protected
//! API Gateway stage is READABLE in the CloudWatch Logs log group the configuration named.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > A record a web ACL's logging configuration delivers into a CloudWatch Logs log group is an
//! > INGESTION into that group, not a private store of the delivering service: it lands in a log
//! > stream of that group, so every reader CloudWatch Logs offers over that group reports it —
//! > an unnarrowed scan as much as one narrowed by stream name, by time range or by filter
//! > pattern, a scan paged by `limit` and `nextToken`, and the stream's own event reader — and
//! > every reader's copy of the record is the same record, carrying the action WAF applied, the
//! > rule that applied it, the path asked for, the components `RedactedFields` names shown as
//! > `REDACTED` and every component it does not name shown as the client sent it.
//!
//! Only a cross-service suite can pin this: the api and its stage are API Gateway's, the web ACL
//! and its logging configuration are WAFv2's, and the readers are CloudWatch Logs' own, all
//! reached through `aws_emulator_router::build_router`. A client that keys an audit pipeline on
//! WAF log delivery does not read the delivering service's store — it narrows, pages and
//! streams the destination group, which is what each row here does.
//!
//! The rows differ along what the rule generalises over: WHICH reader, and how it is narrowed —
//! no narrowing, the delivering stream named in full, a prefix of that stream's name, a prefix
//! no stream carries, a time range open at the record's own instant and one past it, a JSON
//! filter pattern on a field of the record, an unstructured one on a term the record carries,
//! and patterns naming what the record must NOT carry (the redacted credential, an action the
//! web ACL never applied). The recorded case — one unnarrowed scan over the destination group
//! after a blocked request — is one row.
//!
//! The sibling suites [`crate::apigateway_waf_block_record_redacts_every_named_component`] and
//! [`crate::apigateway_wafv2_waf_log_destination_gate`] pin WHAT the record shows and WHICH
//! destination takes it; this one pins that the delivered record is an event of the named group
//! for every one of that group's readers. Each keeps its own harness, as the suites beside them
//! do, so no suite's setup can be changed by another's.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region the invoke host, all three control planes and every ARN name.
const REGION: &str = "us-west-2";

/// The `Scope` an API Gateway stage's web ACL lives in: a stage is a REGIONAL resource.
const SCOPE: &str = "REGIONAL";

/// The prefix the NAME of a logging destination must begin with
/// (`smithy_query(query="doc",shape="LoggingConfiguration")`: "the name that you give the
/// destination must start with `aws-waf-logs-`").
const WAF_LOGS_PREFIX: &str = "aws-waf-logs-";

/// What a redacted component shows in a record instead of what the client sent, from
/// `LoggingConfiguration$RedactedFields`: "the `HEADER` field in the logs will be `REDACTED`".
const REDACTED: &str = "REDACTED";

/// The resource path every request drives, and the query string each one carries.
const MARKER_PATH: &str = "marker";
const QUERY: &str = "probe=1";

/// The two headers every request carries: the one the configuration keeps out of its records,
/// and one it does not name, which every reader must therefore show verbatim.
const CREDENTIAL_HEADER: &str = "authorization";
const CREDENTIAL_VALUE: &str = "Bearer a-caller-token-91ac";
const MARKER_HEADER: &str = "x-waf-keep";
const MARKER_VALUE: &str = "kept-verbatim";

/// The status the stage answers a request the web ACL blocked with.
const BLOCKED_STATUS: u16 = 403;

/// How many successive blocked requests the readers are driven over: more than one, so a reader
/// that reports only the first record fails, and small, so the suite stays quick.
const REQUESTS: usize = 3;

/// The name of the api, the stage, the web ACL, its rule and the destination group.
const NAME: &str = "waf-log-readers";

/// How long a delivered record is given to be readable in its destination group, and how long to
/// wait between looks. Generous: the suite waits for the RECORDS, never for a duration, and
/// fails only if they never arrive at all.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);
const DELIVERY_POLL: Duration = Duration::from_millis(20);

/// The emulator as it is ASSEMBLED, on one engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = aws_emulator_router::build_router(engine);
        // The suite exercises the services, not SigV4: a control-plane call carries a credential
        // scope so the router can name the target service, and the invoke of the published stage
        // URL carries no credential at all, as a client of one sends none.
        router.set_request_authentication(false);
        Self { router }
    }

    /// Dispatch one request as it arrives on the wire, answering its status, parsed body and raw
    /// text.
    async fn dispatch(
        &self,
        method: &str,
        host: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Bytes,
    ) -> (u16, Value, String) {
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
        let bytes = response.into_body();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, parsed, text)
    }

    /// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
    /// the request. The signature itself is never checked here.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host, Signature=0000"
        )
    }

    /// One API Gateway control-plane call that has to be accepted with `expected`.
    async fn api_ok(&self, method: &str, path: &str, body: Option<Value>, expected: u16) -> Value {
        let payload = body.map(|value| value.to_string()).unwrap_or_default();
        let (status, parsed, text) = self
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
            status, expected,
            "setup {method} {path}: expected HTTP {expected}, got {status} with {text:?}"
        );
        parsed
    }

    /// One WAFv2 call that has to succeed.
    async fn waf_ok(&self, operation: &str, body: Value) -> Value {
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
        assert_eq!(
            status, 200,
            "setup {operation}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// One CloudWatch Logs call, answering its status and parsed body.
    async fn logs(&self, operation: &str, body: Value) -> (u16, Value) {
        let (status, parsed, _) = self
            .dispatch(
                "POST",
                &format!("logs.{REGION}.amazonaws.com"),
                "/",
                &[
                    ("content-type", "application/x-amz-json-1.1"),
                    ("authorization", &Self::authorization("logs")),
                    ("x-amz-target", &format!("Logs_20140328.{operation}")),
                ],
                Bytes::from(body.to_string()),
            )
            .await;
        (status, parsed)
    }

    /// One CloudWatch Logs call that has to succeed.
    async fn logs_ok(&self, operation: &str, body: Value) -> Value {
        let (status, parsed) = self.logs(operation, body.clone()).await;
        assert_eq!(
            status, 200,
            "{operation} {body}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// One unsigned request to the deployed stage's invoke URL: the api's own host, no
    /// credential, and the two headers every request sends.
    async fn invoke(&self, api: &str, stage: &str) -> (u16, String) {
        let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
        let (status, _, text) = self
            .dispatch(
                "GET",
                &host,
                &format!("/{stage}/{MARKER_PATH}?{QUERY}"),
                &[
                    (CREDENTIAL_HEADER, CREDENTIAL_VALUE),
                    (MARKER_HEADER, MARKER_VALUE),
                    ("content-type", "application/json"),
                ],
                Bytes::new(),
            )
            .await;
        (status, text)
    }

    /// The events `FilterLogEvents` reports for `group` under `narrowing`, each as the event the
    /// reader answered it as.
    async fn filtered(&self, group: &str, narrowing: &Value) -> Vec<Value> {
        let mut body = json!({ "logGroupName": group });
        if let Some(members) = narrowing.as_object() {
            for (name, value) in members {
                body[name] = value.clone();
            }
        }
        self.logs_ok("FilterLogEvents", body)
            .await
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// The events in `group` once at least `expected` of them are readable there, looking again
    /// until they are or [`DELIVERY_DEADLINE`] passes: delivery is asynchronous, so the suite
    /// waits for the RECORDS rather than for any duration.
    async fn events_settled(&self, case: &str, group: &str, expected: usize) -> Vec<Value> {
        let deadline = Instant::now() + DELIVERY_DEADLINE;
        loop {
            let events = self.filtered(group, &json!({})).await;
            if events.len() >= expected {
                return events;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected at least {expected} event(s) in {group:?}, got {events:?}"
            );
            tokio::time::sleep(DELIVERY_POLL).await;
        }
    }
}

/// The ARN `AssociateWebACL` binds a web ACL to a REST API stage by, the one form
/// `AssociateWebACLRequest$ResourceArn` documents for an API Gateway REST API stage.
fn stage_arn(api: &str, stage: &str) -> String {
    format!("arn:aws:apigateway:{REGION}::/restapis/{api}/stages/{stage}")
}

/// The ARN of the log group `name`, as a `LogDestinationConfigs` entry names one.
fn group_arn(name: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{account}:log-group:{name}",
        account = engine::emulator_account_id()
    )
}

/// A `VisibilityConfig`, which every rule and web ACL needs.
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
    ::aws_smithy_types::base64::encode(value.as_bytes())
}

/// The rule named `name` that BLOCKS a request whose URI path contains the marker path.
fn blocking_rule(name: &str) -> Value {
    json!({
        "Name": name,
        "Priority": 0,
        "Statement": {
            "ByteMatchStatement": {
                "SearchString": base64_of(&format!("/{MARKER_PATH}")),
                "FieldToMatch": { "UriPath": {} },
                "TextTransformations": [{ "Priority": 0, "Type": "NONE" }],
                "PositionalConstraint": "CONTAINS",
            }
        },
        "Action": { "Block": {} },
        "VisibilityConfig": visibility(name),
    })
}

/// The message of a log event, parsed as the JSON document a WAF log record is.
fn record_of(event: &Value) -> Value {
    serde_json::from_str(event["message"].as_str().unwrap_or_default()).unwrap_or(Value::Null)
}

/// The `value` of the header `name` in a record's `httpRequest.headers`.
fn logged_header(record: &Value, name: &str) -> String {
    record["httpRequest"]["headers"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .find(|header| {
            header["name"]
                .as_str()
                .is_some_and(|logged| logged.eq_ignore_ascii_case(name))
        })
        .and_then(|header| header["value"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// What every reader's copy of a delivered record has to show: the action WAF applied, the rule
/// that applied it, the web ACL that decided, the path asked for, the named header redacted and
/// the unnamed one verbatim — and the redacted value nowhere at all.
fn assert_is_the_blocked_record(case: &str, record: &Value, rule: &str, web_acl: &str) {
    assert_eq!(
        record["action"],
        json!("BLOCK"),
        "case {case:?}: the record must name the action WAF applied, got {record}"
    );
    assert_eq!(
        record["terminatingRuleId"],
        json!(rule),
        "case {case:?}: the record must name the rule that applied it, got {record}"
    );
    assert_eq!(
        record["webaclId"],
        json!(web_acl),
        "case {case:?}: the record must name the web ACL that decided, got {record}"
    );
    let uri = record["httpRequest"]["uri"].as_str().unwrap_or_default();
    assert!(
        uri.contains(MARKER_PATH),
        "case {case:?}: the record must name the path asked for, got {uri:?} in {record}"
    );
    let redacted = logged_header(record, CREDENTIAL_HEADER);
    assert_eq!(
        redacted, REDACTED,
        "case {case:?}: the header the configuration names must show {REDACTED:?}, got \
         {redacted:?} in {record}"
    );
    let shown = logged_header(record, MARKER_HEADER);
    assert_eq!(
        shown, MARKER_VALUE,
        "case {case:?}: a header no redaction names must show what the client sent, got \
         {shown:?} in {record}"
    );
    let text = record.to_string();
    assert!(
        !text.contains(CREDENTIAL_VALUE),
        "case {case:?}: the redacted value must appear nowhere in the record, got {record}"
    );
}

/// The api every request is driven against: a `GET` method on the marker path over a `MOCK`
/// integration that answers `200`, so the status a request gets says whether it reached the
/// origin.
async fn deployed_api(emulator: &Emulator, stage: &str) -> String {
    let created = emulator
        .api_ok("POST", "/restapis", Some(json!({ "name": NAME })), 201)
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
            Some(json!({ "pathPart": MARKER_PATH })),
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
    emulator
        .api_ok(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": stage })),
            201,
        )
        .await;
    api
}

/// What one setup leaves behind for the readers to be driven over.
struct Delivered {
    /// The destination log group the configuration named.
    group: String,
    /// The web ACL's ARN, which each record names it by.
    web_acl: String,
    /// The name of the rule that blocked each request.
    rule: String,
    /// The events the destination group holds, in the order its reader answered them.
    events: Vec<Value>,
}

/// Deploy the stage, put a blocking web ACL in front of it, bind a `aws-waf-logs-`-prefixed log
/// group as the logging destination with the credential header named in `RedactedFields`, drive
/// [`REQUESTS`] blocked requests, and answer what the destination group then holds.
async fn delivered_records(emulator: &Emulator, case: &str) -> Delivered {
    let group = format!("{WAF_LOGS_PREFIX}{NAME}");
    let rule = format!("{NAME}-deny");
    let api = deployed_api(emulator, NAME).await;
    emulator
        .logs_ok("CreateLogGroup", json!({ "logGroupName": group }))
        .await;

    let created = emulator
        .waf_ok(
            "CreateWebACL",
            json!({
                "Name": NAME,
                "Scope": SCOPE,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility(NAME),
                "Rules": [blocking_rule(&rule)],
            }),
        )
        .await;
    let web_acl = created["Summary"]["ARN"]
        .as_str()
        .expect("Summary.ARN")
        .to_string();
    emulator
        .waf_ok(
            "AssociateWebACL",
            json!({ "WebACLArn": web_acl, "ResourceArn": stage_arn(&api, NAME) }),
        )
        .await;
    emulator
        .waf_ok(
            "PutLoggingConfiguration",
            json!({
                "LoggingConfiguration": {
                    "ResourceArn": web_acl,
                    "LogDestinationConfigs": [group_arn(&group)],
                    "RedactedFields": [{ "SingleHeader": { "Name": CREDENTIAL_HEADER } }],
                }
            }),
        )
        .await;

    for request in 1..=REQUESTS {
        let (status, text) = emulator.invoke(&api, NAME).await;
        assert_eq!(
            status, BLOCKED_STATUS,
            "case {case:?}: request {request} must be answered {BLOCKED_STATUS}, got {status} \
             with {text:?}"
        );
    }
    let events = emulator.events_settled(case, &group, REQUESTS).await;
    assert_eq!(
        events.len(),
        REQUESTS,
        "case {case:?}: {REQUESTS} blocked request(s) must leave {REQUESTS} event(s) in \
         {group:?}, got {events:?}"
    );
    Delivered {
        group,
        web_acl,
        rule,
        events,
    }
}

/// One reader row: how the scan over the destination group is narrowed, and how many of the
/// delivered records that narrowing must report.
struct ReaderRow {
    case: &'static str,
    /// The members the row adds to the `FilterLogEvents` request beside the group's name.
    narrowing: Value,
    expected: usize,
}

/// The rows, built against what the group actually holds: the stream the records landed in and
/// the instants they were delivered at are read back from the group rather than recomputed here,
/// so a row narrows by what a client of the group would narrow by.
fn reader_rows(delivered: &Delivered) -> Vec<ReaderRow> {
    let stream = delivered.events[0]["logStreamName"]
        .as_str()
        .expect("a delivered event names the stream it landed in")
        .to_string();
    // A prefix of the stream's own name, which is what a reader that knows only part of it — the
    // Region, say — narrows by.
    let prefix = stream[..stream.len() / 2].to_string();
    let stamps: Vec<i64> = delivered
        .events
        .iter()
        .map(|event| event["timestamp"].as_i64().unwrap_or_default())
        .collect();
    let first = stamps.iter().copied().min().unwrap_or_default();
    let last = stamps.iter().copied().max().unwrap_or_default();
    let all = REQUESTS;
    vec![
        ReaderRow {
            case: "no narrowing at all, the scan a delivery poll makes",
            narrowing: json!({}),
            expected: all,
        },
        ReaderRow {
            case: "the delivering stream named in full",
            narrowing: json!({ "logStreamNames": [stream] }),
            expected: all,
        },
        ReaderRow {
            case: "a prefix the delivering stream's name begins with",
            narrowing: json!({ "logStreamNamePrefix": prefix }),
            expected: all,
        },
        ReaderRow {
            case: "a stream-name prefix no stream of the group carries",
            narrowing: json!({ "logStreamNamePrefix": "no-stream-of-this-group" }),
            expected: 0,
        },
        ReaderRow {
            case: "a range starting at the instant the first record was delivered at",
            narrowing: json!({ "startTime": first }),
            expected: all,
        },
        ReaderRow {
            case: "a range starting one millisecond past the last record",
            narrowing: json!({ "startTime": last + 1 }),
            expected: 0,
        },
        ReaderRow {
            case: "a range ending one millisecond past the last record",
            narrowing: json!({ "startTime": first, "endTime": last + 1 }),
            expected: all,
        },
        ReaderRow {
            case: "a JSON pattern on the action WAF applied",
            narrowing: json!({ "filterPattern": "{ $.action = \"BLOCK\" }" }),
            expected: all,
        },
        ReaderRow {
            case: "a JSON pattern on the rule that terminated",
            narrowing: json!({
                "filterPattern": format!("{{ $.terminatingRuleId = \"{}\" }}", delivered.rule)
            }),
            expected: all,
        },
        ReaderRow {
            case: "an unstructured term naming the redaction marker",
            narrowing: json!({ "filterPattern": REDACTED }),
            expected: all,
        },
        ReaderRow {
            case: "a quoted term naming the value of the header no redaction names",
            narrowing: json!({ "filterPattern": format!("\"{MARKER_VALUE}\"") }),
            expected: all,
        },
        ReaderRow {
            case: "a quoted term naming the credential the configuration redacts",
            narrowing: json!({ "filterPattern": format!("\"{CREDENTIAL_VALUE}\"") }),
            expected: 0,
        },
        ReaderRow {
            case: "a JSON pattern on an action this web ACL never applied",
            narrowing: json!({ "filterPattern": "{ $.action = \"ALLOW\" }" }),
            expected: 0,
        },
    ]
}

/// Every narrowing a client of the destination group can scan it with reports the delivered
/// records, and each reported record is the blocked request's own.
#[tokio::test]
async fn every_narrowed_scan_of_the_destination_group_finds_the_delivered_block_records() {
    let emulator = Emulator::new();
    let delivered = delivered_records(&emulator, "narrowed scans").await;

    for row in reader_rows(&delivered) {
        let events = emulator.filtered(&delivered.group, &row.narrowing).await;
        assert_eq!(
            events.len(),
            row.expected,
            "case {:?}: expected {} of the {REQUESTS} delivered record(s) under {}, got {events:?}",
            row.case,
            row.expected,
            row.narrowing
        );
        for event in &events {
            assert_is_the_blocked_record(
                row.case,
                &record_of(event),
                &delivered.rule,
                &delivered.web_acl,
            );
        }
    }
}

/// The delivered records page the way a client walks a scan — one page at a time, resuming from
/// the token the last page answered — and read back through the stream's own event reader, which
/// `DescribeLogStreams` names.
#[tokio::test]
async fn the_delivered_records_page_and_read_back_through_the_stream_that_holds_them() {
    let emulator = Emulator::new();
    let delivered = delivered_records(&emulator, "paged and streamed reads").await;

    // Paged: a page carries at most the limit, and the walk over the pages reports every record.
    let case = "one record per page, resumed by its token";
    let mut paged: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut body = json!({ "logGroupName": delivered.group, "limit": 1 });
        if let Some(token) = &token {
            body["nextToken"] = json!(token);
        }
        let answered = emulator.logs_ok("FilterLogEvents", body).await;
        pages += 1;
        assert!(
            pages <= REQUESTS + 2,
            "case {case:?}: a walk over {REQUESTS} record(s) one page at a time must finish, \
             read {pages} page(s) and {} record(s)",
            paged.len()
        );
        let events = answered["events"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            events.len() <= 1,
            "case {case:?}: a page must carry no more events than the limit, got {events:?}"
        );
        paged.extend(events);
        match answered["nextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(
        paged.len(),
        REQUESTS,
        "case {case:?}: the walk must report every delivered record, got {} over {pages} page(s)",
        paged.len()
    );
    for event in &paged {
        assert_is_the_blocked_record(case, &record_of(event), &delivered.rule, &delivered.web_acl);
    }

    // Streamed: the group reports the stream the records landed in, and that stream's own reader
    // answers the same records.
    let case = "the stream's own event reader";
    let streams = emulator
        .logs_ok(
            "DescribeLogStreams",
            json!({ "logGroupName": delivered.group }),
        )
        .await;
    let named: Vec<String> = streams["logStreams"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|stream| stream["logStreamName"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        named.len(),
        1,
        "case {case:?}: the one delivering web ACL must have left one stream in the group, got \
         {named:?}"
    );
    let events = emulator
        .logs_ok(
            "GetLogEvents",
            json!({
                "logGroupName": delivered.group,
                "logStreamName": named[0],
                "startFromHead": true,
            }),
        )
        .await;
    let events = events["events"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        events.len(),
        REQUESTS,
        "case {case:?}: the stream {:?} must hold every delivered record, got {events:?}",
        named[0]
    );
    for event in &events {
        assert_is_the_blocked_record(case, &record_of(event), &delivered.rule, &delivered.web_acl);
    }
}
