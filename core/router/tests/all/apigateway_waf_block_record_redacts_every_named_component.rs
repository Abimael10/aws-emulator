// SPDX-License-Identifier: CC0-1.0

//! What a WAFv2 logging configuration in front of a deployed REST stage KEEPS OUT of the
//! record it delivers about a request its rules blocked, on the assembled emulator.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > A web ACL carrying a logging configuration whose destination is a CloudWatch Logs log
//! > group delivers, for EVERY request it decides at the resource it is associated with, one
//! > JSON record into that group — a request it blocked (answered `403` at the edge) as much
//! > as one it let through. In that record, each request component the configuration's
//! > `RedactedFields` names shows the redaction marker `REDACTED` instead of what the client
//! > sent, every component it does not name shows what the client sent, and redaction
//! > changes only the record: the action the web ACL applied, the rule that applied it and
//! > the status the client got are the same whether a component is redacted or not.
//!
//! Only a cross-service suite can pin this: the api and its stage are API Gateway's, the web
//! ACL and its logging configuration are WAFv2's, and the group a record lands in is
//! CloudWatch Logs', all reached through `aws_emulator_router::build_router`. Every delivery
//! assertion reads the record back through CloudWatch Logs' own reader (`FilterLogEvents`),
//! so a record that never reached that store cannot pass.
//!
//! The rows differ along what the rule generalises over: WHICH component the configuration
//! names — the four `FieldToMatch` settings `RedactedFields` admits for redaction
//! (`smithy_query(query="doc",shape="LoggingConfiguration",member="RedactedFields")` names
//! `UriPath`, `QueryString`, `SingleHeader` and `Method`) — how MANY it names (none, one,
//! two, all four), whether a `SingleHeader` entry is written in the case the header arrived
//! in or another, whether it names a header the request does not carry at all, and the method
//! the client sent. The recorded case — one header named in front of a blocked request — is
//! one row.
//!
//! The sibling suite [`crate::apigateway_wafv2_waf_log_destination_gate`] pins what the same
//! put decides about the DESTINATION it names; this one pins what the accepted
//! configuration's record shows.

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
/// `LoggingConfiguration$RedactedFields`: "the `HEADER` field in the logs will be
/// `REDACTED`".
const REDACTED: &str = "REDACTED";

/// `WAFNonexistentItemException`'s sentence, as recorded from WAF: no variable part, so the
/// whole message is the requirement.
const NONEXISTENT_ITEM: &str =
    "AWS WAF couldn’t perform the operation because your resource doesn’t exist.";

/// The resource path every case drives, and the query string every request carries.
const MARKER_PATH: &str = "marker";
const QUERY: &str = "probe=1";

/// The two headers every request carries: one standing for a component a caller wants out of
/// its logs, and one that must show up verbatim wherever the configuration does not name it.
const CREDENTIAL_HEADER: &str = "authorization";
const CREDENTIAL_VALUE: &str = "Bearer a-caller-token-91ac";
const MARKER_HEADER: &str = "x-waf-keep";
const MARKER_VALUE: &str = "kept-verbatim";

/// A header no request here sends, so a row naming it redacts nothing.
const UNSENT_HEADER: &str = "cookie";

/// The status the stage answers a request the web ACL blocked with.
const BLOCKED_STATUS: u16 = 403;

/// How long a delivered record is given to be readable in its destination group, and how long
/// to wait between looks. Generous: the suite waits for the RECORD, never for a duration, and
/// fails only if one never arrives at all.
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
        // The suite exercises the services, not SigV4: a control-plane call carries a
        // credential scope so the router can name the target service, and the invoke of the
        // published stage URL carries no credential at all, as a client of one sends none.
        router.set_request_authentication(false);
        Self { router }
    }

    /// Dispatch one request as it arrives on the wire, answering its status, parsed body and
    /// raw text.
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

    /// Create the log group `name`, as the client of a logging destination does before any
    /// put names it.
    async fn create_group(&self, name: &str) {
        let (status, body) = self
            .logs("CreateLogGroup", json!({ "logGroupName": name }))
            .await;
        assert_eq!(
            status, 200,
            "setup CreateLogGroup {name:?}: expected HTTP 200, got {status} with {body}"
        );
    }

    /// One unsigned request to a deployed stage's invoke URL: the api's own host, no
    /// credential, and the two headers every case sends.
    async fn invoke(&self, api: &str, stage: &str, method: &str) -> (u16, String) {
        let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
        let (status, _, text) = self
            .dispatch(
                method,
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

    /// The records `FilterLogEvents` reports for `group`, parsed as the JSON lines a WAF log
    /// record is, or `None` when the account holds no group of that name at all.
    async fn delivered(&self, group: &str) -> Option<Vec<Value>> {
        let (status, body) = self
            .logs("FilterLogEvents", json!({ "logGroupName": group }))
            .await;
        if status != 200 {
            return None;
        }
        Some(
            body.get("events")
                .and_then(Value::as_array)
                .map(|events| {
                    events
                        .iter()
                        .map(|event| {
                            let message = event
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            serde_json::from_str(message).unwrap_or(Value::Null)
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )
    }

    /// The records in `group` once at least `expected` of them are readable there, looking
    /// again until they are or [`DELIVERY_DEADLINE`] passes: delivery is asynchronous, so the
    /// suite waits for the RECORDS rather than for any duration.
    async fn records_settled(&self, case: &str, group: &str, expected: usize) -> Vec<Value> {
        let deadline = Instant::now() + DELIVERY_DEADLINE;
        loop {
            let delivered = self.delivered(group).await.unwrap_or_default();
            if delivered.len() >= expected {
                return delivered;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected at least {expected} record(s) in {group:?}, got \
                 {delivered:?}"
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

/// A rule named `name` that BLOCKS a request whose URI path contains the marker path every
/// case drives.
///
/// The criterion is written against the same component some rows redact, which is how each
/// row shows that redaction decides what a RECORD shows and not what WAF inspects.
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

/// A `SingleHeader` redaction naming `header`.
fn single_header(header: &str) -> Value {
    json!({ "SingleHeader": { "Name": header } })
}

/// The `PutLoggingConfiguration` request binding `destination` in front of the web ACL `arn`,
/// naming `redacted` — or naming nothing at all when the row leaves the member out.
fn put_request(arn: &str, destination: &str, redacted: Option<&Value>) -> Value {
    let mut configuration = json!({
        "ResourceArn": arn,
        "LogDestinationConfigs": [destination],
    });
    if let Some(redacted) = redacted {
        configuration["RedactedFields"] = redacted.clone();
    }
    json!({ "LoggingConfiguration": configuration })
}

/// The api every case is driven against: a `GET` and a `POST` method on the marker path, both
/// on a `MOCK` integration that answers `200`, so the status a request gets says whether it
/// reached the origin.
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
            Some(json!({ "pathPart": MARKER_PATH })),
            201,
        )
        .await;
    let resource = resource["id"]
        .as_str()
        .expect("the resource id")
        .to_string();
    for method in ["GET", "POST"] {
        emulator
            .api_ok(
                "PUT",
                &format!("/restapis/{api}/resources/{resource}/methods/{method}"),
                Some(json!({ "authorizationType": "NONE" })),
                201,
            )
            .await;
        emulator
            .api_ok(
                "PUT",
                &format!("/restapis/{api}/resources/{resource}/methods/{method}/integration"),
                Some(json!({ "type": "MOCK" })),
                201,
            )
            .await;
        emulator
            .api_ok(
                "PUT",
                &format!(
                    "/restapis/{api}/resources/{resource}/methods/{method}/integration/responses/200"
                ),
                Some(json!({
                    "responseTemplates": { "application/json": r#"{"at":"origin"}"# }
                })),
                201,
            )
            .await;
    }
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

/// Create a web ACL carrying `rules`, put it in front of the stage, and answer its ARN.
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

/// One redaction row: the components the configuration names, the request the client sends,
/// and what each component of the delivered record must then show.
struct RedactionRow {
    case: &'static str,
    /// The stage, the web ACL, its rule and the destination group this row drives, all named
    /// for it.
    name: &'static str,
    /// The method the client sends, which `Method` redaction is about.
    method: &'static str,
    /// The `RedactedFields` list, or `None` to leave the member out of the put entirely.
    redacted: Option<Value>,
    /// What the record's `httpMethod` must show.
    logged_method: &'static str,
    /// What the record's `uri` must show: the invoke path, or the marker.
    uri_shown: bool,
    /// What the record's `args` must show: the query string the client sent, or the marker.
    args_shown: bool,
    /// What each of the two headers the client sends must show.
    credential_shown: bool,
    marker_shown: bool,
}

/// The rows: the member left out, each component the member admits named on its own, a header
/// named in another case than it arrived in, a header the request does not carry, two headers
/// at once, and all four components at once.
fn redaction_rows() -> Vec<RedactionRow> {
    vec![
        RedactionRow {
            case: "no redacted fields named at all",
            name: "nothing",
            method: "GET",
            redacted: None,
            logged_method: "GET",
            uri_shown: true,
            args_shown: true,
            credential_shown: true,
            marker_shown: true,
        },
        RedactionRow {
            case: "the URI path alone, the very component the blocking rule inspects",
            name: "uri",
            method: "GET",
            redacted: Some(json!([{ "UriPath": {} }])),
            logged_method: "GET",
            uri_shown: false,
            args_shown: true,
            credential_shown: true,
            marker_shown: true,
        },
        RedactionRow {
            case: "the query string alone",
            name: "query",
            method: "GET",
            redacted: Some(json!([{ "QueryString": {} }])),
            logged_method: "GET",
            uri_shown: true,
            args_shown: false,
            credential_shown: true,
            marker_shown: true,
        },
        RedactionRow {
            case: "the HTTP method alone, on a POST request",
            name: "method",
            method: "POST",
            redacted: Some(json!([{ "Method": {} }])),
            logged_method: REDACTED,
            uri_shown: true,
            args_shown: true,
            credential_shown: true,
            marker_shown: true,
        },
        RedactionRow {
            case: "one header, named exactly as it arrived",
            name: "header",
            method: "GET",
            redacted: Some(json!([single_header(CREDENTIAL_HEADER)])),
            logged_method: "GET",
            uri_shown: true,
            args_shown: true,
            credential_shown: false,
            marker_shown: true,
        },
        RedactionRow {
            case: "one header, named in another case than it arrived in",
            name: "header-case",
            method: "POST",
            redacted: Some(json!([single_header("AuThOrIzAtIoN")])),
            logged_method: "POST",
            uri_shown: true,
            args_shown: true,
            credential_shown: false,
            marker_shown: true,
        },
        RedactionRow {
            case: "a header the request does not carry, which redacts nothing",
            name: "absent-header",
            method: "GET",
            redacted: Some(json!([single_header(UNSENT_HEADER)])),
            logged_method: "GET",
            uri_shown: true,
            args_shown: true,
            credential_shown: true,
            marker_shown: true,
        },
        RedactionRow {
            case: "both headers the request carries",
            name: "both-headers",
            method: "GET",
            redacted: Some(json!([
                single_header(CREDENTIAL_HEADER),
                single_header(MARKER_HEADER),
            ])),
            logged_method: "GET",
            uri_shown: true,
            args_shown: true,
            credential_shown: false,
            marker_shown: false,
        },
        RedactionRow {
            case: "every component the member admits for redaction, at once",
            name: "every-component",
            method: "POST",
            redacted: Some(json!([
                { "UriPath": {} },
                { "QueryString": {} },
                single_header(CREDENTIAL_HEADER),
                { "Method": {} },
            ])),
            logged_method: REDACTED,
            uri_shown: false,
            args_shown: false,
            credential_shown: false,
            marker_shown: true,
        },
    ]
}

/// A request a web ACL blocked at the stage leaves one record in the log group its logging
/// configuration names, showing every component that configuration redacts as `REDACTED` and
/// every other one as the client sent it — while the block itself is unchanged.
#[tokio::test]
async fn a_blocked_requests_record_redacts_exactly_the_components_the_configuration_names() {
    let emulator = Emulator::new();
    let rows = redaction_rows();
    let stages: Vec<&str> = rows.iter().map(|row| row.name).collect();
    let api = deployed_api(&emulator, "waf-redaction-api", &stages).await;

    for row in &rows {
        let group = format!("{WAF_LOGS_PREFIX}{}", row.name);
        emulator.create_group(&group).await;
        let rule = format!("deny-{}", row.name);
        let arn = web_acl_in_front(
            &emulator,
            &format!("acl-{}", row.name),
            json!([blocking_rule(&rule)]),
            &api,
            row.name,
        )
        .await;

        let destination = group_arn(&group);
        let put = emulator
            .waf_ok(
                "PutLoggingConfiguration",
                put_request(&arn, &destination, row.redacted.as_ref()),
            )
            .await;
        let stored = &put["LoggingConfiguration"]["RedactedFields"];
        let expected_stored = row.redacted.clone().unwrap_or(Value::Null);
        assert_eq!(
            *stored, expected_stored,
            "case {:?}: the put must answer the redaction list as given, got {put}",
            row.case
        );

        // The decision is the rule's, not the redaction's: whatever the configuration keeps
        // out of the record, the request is still refused at the edge.
        let (status, text) = emulator.invoke(&api, row.name, row.method).await;
        assert_eq!(
            status, BLOCKED_STATUS,
            "case {:?}: the blocked request must be answered {BLOCKED_STATUS}, got {status} \
             with {text:?}",
            row.case
        );
        assert!(
            !text.contains("origin"),
            "case {:?}: a blocked request must not reach the origin, got {text:?}",
            row.case
        );

        let delivered = emulator.records_settled(row.case, &group, 1).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {:?}: one inspected request must leave exactly one record in {group:?}, got \
             {delivered:?}",
            row.case
        );
        let record = &delivered[0];
        assert_eq!(
            record["action"],
            json!("BLOCK"),
            "case {:?}: the record must name the action WAF applied, got {record}",
            row.case
        );
        assert_eq!(
            record["terminatingRuleId"],
            json!(rule),
            "case {:?}: the record must name the rule that applied it, got {record}",
            row.case
        );
        assert_eq!(
            record["webaclId"],
            json!(arn),
            "case {:?}: the record must name the web ACL that decided, got {record}",
            row.case
        );

        assert_eq!(
            record["httpRequest"]["httpMethod"],
            json!(row.logged_method),
            "case {:?}: expected the method {:?}, got {record}",
            row.case,
            row.logged_method
        );
        // The path WAF inspected is the path within the api, `/{stage}/{resource-path}`.
        let invoke_path = format!("/{}/{MARKER_PATH}", row.name);
        let expected_uri = if row.uri_shown {
            invoke_path.as_str()
        } else {
            REDACTED
        };
        assert_eq!(
            record["httpRequest"]["uri"],
            json!(expected_uri),
            "case {:?}: expected the uri {expected_uri:?}, got {record}",
            row.case
        );
        let expected_args = if row.args_shown { QUERY } else { REDACTED };
        assert_eq!(
            record["httpRequest"]["args"],
            json!(expected_args),
            "case {:?}: expected the args {expected_args:?}, got {record}",
            row.case
        );

        for (header, sent, shown) in [
            (CREDENTIAL_HEADER, CREDENTIAL_VALUE, row.credential_shown),
            (MARKER_HEADER, MARKER_VALUE, row.marker_shown),
        ] {
            let expected = if shown { sent } else { REDACTED };
            let logged = logged_header(record, header);
            assert_eq!(
                logged, expected,
                "case {:?}: expected the header {header:?} to show {expected:?}, got \
                 {logged:?} in {record}",
                row.case
            );
        }

        // What a redaction keeps out of a record is kept out of the WHOLE record, not merely
        // out of the one member it is read back through.
        let text = record.to_string();
        for (value, shown) in [
            (CREDENTIAL_VALUE, row.credential_shown),
            (MARKER_VALUE, row.marker_shown),
            (QUERY, row.args_shown),
            (invoke_path.as_str(), row.uri_shown),
        ] {
            if !shown {
                assert!(
                    !text.contains(value),
                    "case {:?}: the redacted value {value:?} must appear nowhere in the \
                     record, got {record}",
                    row.case
                );
            }
        }
    }
}

/// A put naming a web ACL the account does not hold is refused with the modelled
/// `WAFNonexistentItemException`, and the destination it named takes nothing.
#[tokio::test]
async fn a_put_for_a_web_acl_that_does_not_exist_delivers_nothing() {
    let emulator = Emulator::new();
    let group = format!("{WAF_LOGS_PREFIX}unbound");
    emulator.create_group(&group).await;
    let destination = group_arn(&group);

    // An ARN of the form a create mints, naming a web ACL no create minted.
    let missing = format!(
        "arn:aws:wafv2:{REGION}:{account}:regional/webacl/absent-acl/\
         11111111-2222-3333-4444-555555555555",
        account = engine::emulator_account_id()
    );
    let (status, body) = emulator
        .waf(
            "PutLoggingConfiguration",
            put_request(&missing, &destination, Some(&json!([single_header(CREDENTIAL_HEADER)]))),
        )
        .await;
    assert_eq!(
        status, 400,
        "expected HTTP 400 for a web ACL no create minted, got {status} with {body}"
    );
    assert_eq!(
        body["__type"],
        json!("WAFNonexistentItemException"),
        "expected WAFNonexistentItemException, got {body}"
    );
    assert_eq!(
        body["Message"],
        json!(NONEXISTENT_ITEM),
        "expected the modelled not-found sentence, got {body}"
    );
    let delivered = emulator.delivered(&group).await.unwrap_or_default();
    assert!(
        delivered.is_empty(),
        "a destination whose put was refused takes no record, got {delivered:?}"
    );
}
