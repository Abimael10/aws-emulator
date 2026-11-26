// SPDX-License-Identifier: CC0-1.0

//! What a WAFv2 logging configuration in front of a deployed REST stage DECIDES about the
//! destination it names, on the assembled emulator.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > A `PutLoggingConfiguration` is judged BEFORE anything is persisted: a destination
//! > whose own NAME does not begin with `aws-waf-logs-` is refused with
//! > `WAFInvalidParameterException` (HTTP 400), so a web ACL whose put was refused carries
//! > no logging configuration at all (`GetLoggingConfiguration` answers
//! > `WAFNonexistentItemException`, HTTP 400) and the destination that put named — a log
//! > group the client created itself included — takes no stream and no record, while the
//! > web ACL goes on deciding requests at the stage. A destination whose name DOES carry
//! > the prefix is accepted, round-trips `LogDestinationConfigs` and `RedactedFields`, and
//! > from then on each request the web ACL inspects leaves one record in THAT destination
//! > naming the action WAF applied, the rule that applied it and the path asked for, with
//! > every component `RedactedFields` names shown as `REDACTED` and every other one shown
//! > as the client sent it.
//!
//! Only a cross-service suite can pin this, and it is pinned here because all three
//! services have to be the ones the emulator ASSEMBLES: the api and its stage are API
//! Gateway's, the web ACL and its logging configuration are WAFv2's, and the group a record
//! lands in is CloudWatch Logs', reached through `aws_emulator_router::build_router`. Every
//! delivery assertion reads the outcome back through CloudWatch Logs' own readers
//! (`DescribeLogStreams` and `FilterLogEvents`), so a record that did not reach that store
//! cannot pass, and every refusal is read as a client reads it — the status and body of the
//! call it made.
//!
//! The refusal rows differ along what the rule generalises over: the KIND of destination
//! (a CloudWatch Logs log group by either published form of its ARN, an Amazon S3 bucket, a
//! Firehose delivery stream, and a value that is no ARN at all) and the WAY its name fails
//! the prefix (absent, present but not at the start, present in another case). The delivery
//! rows differ along the form the accepted destination's ARN is written in, WHICH component
//! the configuration redacts, and what the web ACL DECIDED (a terminating block, and the
//! default action letting the request through to the origin).

use std::collections::BTreeSet;
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

/// The `terminatingRuleId` a record names for a request no rule terminated, which the web
/// ACL's `DefaultAction` decided.
const DEFAULT_ACTION: &str = "Default_Action";

/// The `ParameterExceptionField` WAF reports a destination it will not accept against
/// (`smithy_query(query="shape",shape="ParameterExceptionField")` lists `LOG_DESTINATION`
/// among its values).
const FIELD_LOG_DESTINATION: &str = "LOG_DESTINATION";

/// The `ErrorReason` WAF carries for an ARN it will not accept, and the sentence a
/// `WAFInvalidParameterException` composes from its own three members, as the recorded WAF
/// responses behind this service's refusal vocabulary show them (the `Field`, `Parameter`
/// and `Reason` members each carry their own doc: see the exception's constructor in the
/// wafv2 crate's `state.rs`). Spelled out here rather than read off the service, so this
/// suite pins the wording and not merely its reuse.
const ARN_NOT_VALID: &str = "The ARN isn't valid. A valid ARN begins with arn: and includes \
     other information separated by colons or slashes.";

/// The resource path every case drives, and the query string it carries.
const MARKER_PATH: &str = "marker";
const QUERY: &str = "probe=1";

/// The two headers every request carries. Which of them a configuration redacts is per row:
/// a header is NAMED in `RedactedFields`, not fixed, so either can be the redacted one and
/// the other must then be shown as the client sent it.
const CREDENTIAL_HEADER: &str = "authorization";
const CREDENTIAL_VALUE: &str = "Bearer a-caller-token-91ac";
const MARKER_HEADER: &str = "x-waf-keep";
const MARKER_VALUE: &str = "kept-verbatim";

/// How long a delivered record is given to be readable in its destination group, and how
/// long to wait between looks. Generous: the suite waits for the RECORD, never for a
/// duration, and fails only if one never arrives at all.
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

    /// Create the log group `name` as the client of a logging destination does, before any
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
    /// credential, and both headers every case sends.
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

    /// The log streams `DescribeLogStreams` reports for `group`, or `None` when the account
    /// holds no group of that name at all.
    async fn streams(&self, group: &str) -> Option<Vec<String>> {
        let (status, body) = self
            .logs("DescribeLogStreams", json!({ "logGroupName": group }))
            .await;
        if status != 200 {
            return None;
        }
        Some(
            body.get("logStreams")
                .and_then(Value::as_array)
                .map(|streams| {
                    streams
                        .iter()
                        .map(|stream| {
                            stream
                                .get("logStreamName")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string()
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )
    }

    /// The records `FilterLogEvents` reports for `group`, parsed as the JSON lines a WAF log
    /// record is, or `None` when the account holds no group of that name at all — which is
    /// what "nothing was delivered and nothing was provisioned" looks like to a client.
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

/// The two forms the ARN of ONE log group is read in, both of which name that group as a
/// destination: the group on its own, and the group together with everything under it (the
/// form `DescribeLogGroups` reports a group's `arn` in).
#[derive(Clone, Copy, Debug)]
enum GroupArnForm {
    Group,
    GroupAndEverythingUnderIt,
}

/// The ARN of the log group `name`, in `form`, as a `LogDestinationConfigs` entry names one.
fn group_arn(name: &str, form: GroupArnForm) -> String {
    let group = format!(
        "arn:aws:logs:{REGION}:{account}:log-group:{name}",
        account = engine::emulator_account_id()
    );
    match form {
        GroupArnForm::Group => group,
        GroupArnForm::GroupAndEverythingUnderIt => format!("{group}:*"),
    }
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

/// A rule named `name` that applies `action` to a request whose URI path contains the marker
/// path every case drives.
fn rule_on_the_marker(name: &str, action: Value) -> Value {
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
        "Action": action,
        "VisibilityConfig": visibility(name),
    })
}

/// A `SingleHeader` redaction naming `header`.
fn single_header(header: &str) -> Value {
    json!({ "SingleHeader": { "Name": header } })
}

/// The `PutLoggingConfiguration` request binding `destination` in front of the web ACL
/// `arn`, redacting `header`.
fn put_request(arn: &str, destination: &str, header: &str) -> Value {
    json!({
        "LoggingConfiguration": {
            "ResourceArn": arn,
            "LogDestinationConfigs": [destination],
            "RedactedFields": [single_header(header)],
        }
    })
}

/// The api every case is driven against: one `GET` method on the marker path, on a `MOCK`
/// integration that answers `200`, so the status a request gets says whether it reached the
/// origin.
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

/// Create a web ACL carrying `rules` and `default_action`, put it in front of the stage, and
/// answer its ARN.
async fn web_acl_in_front(
    emulator: &Emulator,
    name: &str,
    default_action: Value,
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
                "DefaultAction": default_action,
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

/// One destination a put may not name: how its name fails the prefix, and the log group it
/// named when it named one — a group the CLIENT created, which must stay empty.
struct RefusedDestination {
    case: &'static str,
    destination: String,
    /// The log group the destination addresses, when it addresses one at all.
    group: Option<&'static str>,
}

/// The suffix the refused destinations and the accepted one share, so the only difference
/// between the group that is refused and the group that is accepted is the prefix itself.
const SUFFIX: &str = "audit-trail";

/// The rows: a log group whose name lacks the prefix by either published form of its ARN, a
/// group carrying the prefix but not at the start of its name, a group carrying it in another
/// case, destinations of the two other kinds WAF delivers to, and a value that is no ARN at
/// all.
fn refused_destinations() -> Vec<RefusedDestination> {
    vec![
        RefusedDestination {
            case: "a log group whose name lacks the prefix, by the group's own ARN",
            destination: group_arn(SUFFIX, GroupArnForm::Group),
            group: Some(SUFFIX),
        },
        RefusedDestination {
            case: "the same group, by the ARN of the group and everything under it",
            destination: group_arn(SUFFIX, GroupArnForm::GroupAndEverythingUnderIt),
            group: Some(SUFFIX),
        },
        RefusedDestination {
            case: "a log group carrying the prefix, but not at the start of its name",
            destination: group_arn("team-aws-waf-logs-audit", GroupArnForm::Group),
            group: Some("team-aws-waf-logs-audit"),
        },
        RefusedDestination {
            case: "a log group carrying the prefix in another case",
            destination: group_arn("AWS-WAF-LOGS-audit", GroupArnForm::Group),
            group: Some("AWS-WAF-LOGS-audit"),
        },
        RefusedDestination {
            case: "an Amazon S3 bucket whose name lacks the prefix",
            destination: format!("arn:aws:s3:::{SUFFIX}-bucket"),
            group: None,
        },
        RefusedDestination {
            case: "a Firehose delivery stream whose name lacks the prefix",
            destination: format!(
                "arn:aws:firehose:{REGION}:{account}:deliverystream/{SUFFIX}-stream",
                account = engine::emulator_account_id()
            ),
            group: None,
        },
        RefusedDestination {
            // Long enough to satisfy the member's own length constraint
            // (`ResourceArn` is `@length(min: 20)`), so what refuses it is its NAME and not
            // its size.
            case: "a destination that is not an ARN at all",
            destination: format!("{SUFFIX}-destination"),
            group: None,
        },
    ]
}

/// A put naming a destination without the prefix is refused and stores nothing, so the web
/// ACL stays without a logging configuration, the group that destination named takes no
/// stream and no record, and the stage goes on enforcing — while the SAME name carrying the
/// prefix is accepted and starts taking records.
#[tokio::test]
async fn a_destination_without_the_prefix_is_refused_and_stores_nothing() {
    let emulator = Emulator::new();
    let stage = "gate";
    let api = deployed_api(&emulator, "waf-log-gate-api", &[stage]).await;
    let arn = web_acl_in_front(
        &emulator,
        "acl-log-gate",
        json!({ "Allow": {} }),
        json!([rule_on_the_marker("logblocker", json!({ "Block": {} }))]),
        &api,
        stage,
    )
    .await;

    // Both groups as the client of a logging destination creates them: the same suffix, one
    // of them carrying the prefix and one of them not.
    let accepted_group = format!("{WAF_LOGS_PREFIX}{SUFFIX}");
    let rows = refused_destinations();
    // Two rows name ONE group by its two ARN forms, so the names are deduplicated: a group
    // the client already created is not created twice.
    let named: BTreeSet<&str> = rows.iter().filter_map(|row| row.group).collect();
    for group in named {
        emulator.create_group(group).await;
    }
    emulator.create_group(&accepted_group).await;

    for refused in &rows {
        let (status, body) = emulator
            .waf(
                "PutLoggingConfiguration",
                put_request(&arn, &refused.destination, CREDENTIAL_HEADER),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400 for {:?}, got {status} with {body}",
            refused.case, refused.destination
        );
        assert_eq!(
            body["__type"],
            json!("WAFInvalidParameterException"),
            "case {:?}: expected WAFInvalidParameterException, got {body}",
            refused.case
        );
        assert_eq!(
            body["Field"],
            json!(FIELD_LOG_DESTINATION),
            "case {:?}: the refusal must name the field it is about, got {body}",
            refused.case
        );
        assert_eq!(
            body["Parameter"],
            json!(refused.destination),
            "case {:?}: the refusal must echo the destination it refused, got {body}",
            refused.case
        );
        assert_eq!(
            body["Reason"],
            json!(ARN_NOT_VALID),
            "case {:?}: expected the ARN reason, got {body}",
            refused.case
        );
        assert_eq!(
            body["message"],
            json!(format!(
                "Error reason: {ARN_NOT_VALID}, field: {FIELD_LOG_DESTINATION}, parameter: {}",
                refused.destination
            )),
            "case {:?}: the message is composed from Reason, Field and Parameter, got {body}",
            refused.case
        );

        // Nothing was stored, so the web ACL still has no logging configuration to read.
        let (status, body) = emulator
            .waf("GetLoggingConfiguration", json!({ "ResourceArn": arn }))
            .await;
        assert_eq!(
            status, 400,
            "case {:?}: a web ACL whose only put was refused holds no configuration, so the \
             read must answer HTTP 400, got {status} with {body}",
            refused.case
        );
        assert_eq!(
            body["__type"],
            json!("WAFNonexistentItemException"),
            "case {:?}: expected WAFNonexistentItemException, got {body}",
            refused.case
        );

        // The web ACL goes on deciding requests at the stage, which is what makes the silence
        // at the destination a logging outage rather than an inactive web ACL.
        let (status, text) = emulator.invoke(&api, stage).await;
        assert_eq!(
            status, 403,
            "case {:?}: the blocked request must still be answered 403 at the stage, got \
             {status} with {text:?}",
            refused.case
        );

        // And the group the refused destination named takes nothing at all: no stream, no
        // record.
        if let Some(named) = refused.group {
            let streams = emulator.streams(named).await.unwrap_or_else(|| {
                panic!(
                    "case {:?}: the client-created group {named:?} must still be there",
                    refused.case
                )
            });
            assert!(
                streams.is_empty(),
                "case {:?}: a refused destination takes no log stream, got {streams:?} in \
                 {named:?}",
                refused.case
            );
            let delivered = emulator.delivered(named).await.unwrap_or_default();
            assert!(
                delivered.is_empty(),
                "case {:?}: a refused destination takes no record, got {delivered:?} in \
                 {named:?}",
                refused.case
            );
        }
    }

    // The positive control: the SAME suffix carrying the prefix. It is accepted, it reads
    // back as it was put, and from now on the records arrive THERE.
    let destination = group_arn(&accepted_group, GroupArnForm::Group);
    let put = emulator
        .waf_ok(
            "PutLoggingConfiguration",
            put_request(&arn, &destination, CREDENTIAL_HEADER),
        )
        .await;
    let configuration = &put["LoggingConfiguration"];
    assert_eq!(
        configuration["LogDestinationConfigs"],
        json!([destination]),
        "the accepted destination must be answered as it was put, got {put}"
    );
    let read_back = emulator
        .waf_ok("GetLoggingConfiguration", json!({ "ResourceArn": arn }))
        .await;
    assert_eq!(
        read_back["LoggingConfiguration"]["LogDestinationConfigs"],
        json!([destination]),
        "the stored configuration must round-trip its destination, got {read_back}"
    );
    assert_eq!(
        read_back["LoggingConfiguration"]["RedactedFields"],
        json!([single_header(CREDENTIAL_HEADER)]),
        "the stored configuration must round-trip its redacted fields, got {read_back}"
    );

    let (status, text) = emulator.invoke(&api, stage).await;
    assert_eq!(
        status, 403,
        "the blocked request must be answered 403 at the stage, got {status} with {text:?}"
    );
    let case = "the accepted destination";
    let delivered = emulator.records_settled(case, &accepted_group, 1).await;
    for record in &delivered {
        assert_eq!(
            record["action"],
            json!("BLOCK"),
            "case {case:?}: the record must name the action WAF applied, got {record}"
        );
        assert_eq!(
            record["terminatingRuleId"],
            json!("logblocker"),
            "case {case:?}: the record must name the rule that applied it, got {record}"
        );
        let uri = record["httpRequest"]["uri"].as_str().unwrap_or_default();
        assert!(
            uri.contains(MARKER_PATH),
            "case {case:?}: the record must name the path the request asked for, got {uri:?} \
             in {record}"
        );
    }

    // EVERY group a refusal named is STILL empty, read at a point where delivery has
    // demonstrably happened — the record above landed in the accepted group — so a
    // destination AWS would not have accepted is not one records quietly go to late.
    for named in rows.iter().filter_map(|row| row.group) {
        let streams = emulator
            .streams(named)
            .await
            .unwrap_or_else(|| panic!("the client-created group {named:?} must still be there"));
        assert!(
            streams.is_empty(),
            "a destination that was refused takes no stream once logging is on either, got \
             {streams:?} in {named:?}"
        );
        let delivered = emulator.delivered(named).await.unwrap_or_default();
        assert!(
            delivered.is_empty(),
            "a destination that was refused takes no record once logging is on either, got \
             {delivered:?} in {named:?}"
        );
    }
}

/// One delivery row: the form the accepted destination's ARN is written in, which component
/// the configuration redacts, what the web ACL decides, and what that makes of the request
/// and of each record.
struct DeliveryRow {
    case: &'static str,
    /// The stage, the web ACL and the destination group this row drives, all named for it.
    name: &'static str,
    form: GroupArnForm,
    /// The header `RedactedFields` names; the other of the two sent must appear verbatim.
    redacted: &'static str,
    /// The header that is NOT redacted, and the value the client sent under it.
    shown: &'static str,
    shown_value: &'static str,
    /// The web ACL's `DefaultAction` and the rules it carries.
    default_action: Value,
    rules: Value,
    /// The status the stage answers each request with.
    status: u16,
    /// The `action` and `terminatingRuleId` each delivered record names.
    action: &'static str,
    terminating: &'static str,
}

/// The rows: a terminating block logged to a destination named by the group's own ARN with
/// the credential header redacted, the same block logged to a destination named by the group
/// and everything under it with the OTHER header redacted, and a request no rule matched that
/// the default action let through to the origin.
fn delivery_rows() -> Vec<DeliveryRow> {
    vec![
        DeliveryRow {
            case: "a terminating block, the group's own ARN, the credential header redacted",
            name: "blocked",
            form: GroupArnForm::Group,
            redacted: CREDENTIAL_HEADER,
            shown: MARKER_HEADER,
            shown_value: MARKER_VALUE,
            default_action: json!({ "Allow": {} }),
            rules: json!([rule_on_the_marker("deny-blocked", json!({ "Block": {} }))]),
            status: 403,
            action: "BLOCK",
            terminating: "deny-blocked",
        },
        DeliveryRow {
            case: "a terminating block, the group and everything under it, the marker header \
                   redacted",
            name: "wildcard",
            form: GroupArnForm::GroupAndEverythingUnderIt,
            redacted: MARKER_HEADER,
            shown: CREDENTIAL_HEADER,
            shown_value: CREDENTIAL_VALUE,
            default_action: json!({ "Allow": {} }),
            rules: json!([rule_on_the_marker("deny-wildcard", json!({ "Block": {} }))]),
            status: 403,
            action: "BLOCK",
            terminating: "deny-wildcard",
        },
        DeliveryRow {
            case: "a request no rule matched, which the default action let through to the \
                   origin",
            name: "allowed",
            form: GroupArnForm::Group,
            redacted: CREDENTIAL_HEADER,
            shown: MARKER_HEADER,
            shown_value: MARKER_VALUE,
            default_action: json!({ "Allow": {} }),
            rules: json!([]),
            status: 200,
            action: "ALLOW",
            terminating: DEFAULT_ACTION,
        },
    ]
}

/// Each request an accepted configuration's web ACL inspects leaves one more record in the
/// destination it named, naming what WAF did and showing exactly the component the
/// configuration redacts as `REDACTED`.
#[tokio::test]
async fn each_inspected_request_leaves_one_redacted_record_in_the_named_group() {
    let emulator = Emulator::new();
    let rows = delivery_rows();
    let stages: Vec<&str> = rows.iter().map(|row| row.name).collect();
    let api = deployed_api(&emulator, "waf-log-delivery-api", &stages).await;

    for row in &rows {
        let group = format!("{WAF_LOGS_PREFIX}{}", row.name);
        // The destination group as the client of a logging destination creates it: WAF
        // resolves the destination it is pointed at before it stores the configuration, so
        // the group is there before any put names it.
        emulator.create_group(&group).await;
        let arn = web_acl_in_front(
            &emulator,
            &format!("acl-{}", row.name),
            row.default_action.clone(),
            row.rules.clone(),
            &api,
            row.name,
        )
        .await;
        emulator
            .waf_ok(
                "PutLoggingConfiguration",
                put_request(&arn, &group_arn(&group, row.form), row.redacted),
            )
            .await;

        for inspected in 1..=2usize {
            let (status, text) = emulator.invoke(&api, row.name).await;
            assert_eq!(
                status, row.status,
                "case {:?}: request {inspected} must be answered {}, got {status} with \
                 {text:?}",
                row.case, row.status
            );
            let delivered = emulator.records_settled(row.case, &group, inspected).await;
            assert_eq!(
                delivered.len(),
                inspected,
                "case {:?}: {inspected} inspected request(s) must leave {inspected} record(s) \
                 in {group:?}, got {delivered:?}",
                row.case
            );
            for record in &delivered {
                assert_eq!(
                    record["action"],
                    json!(row.action),
                    "case {:?}: expected the action {:?}, got {record}",
                    row.case,
                    row.action
                );
                assert_eq!(
                    record["terminatingRuleId"],
                    json!(row.terminating),
                    "case {:?}: expected the terminating rule {:?}, got {record}",
                    row.case,
                    row.terminating
                );
                assert_eq!(
                    record["webaclId"],
                    json!(arn),
                    "case {:?}: the record must name the web ACL that decided, got {record}",
                    row.case
                );
                let uri = record["httpRequest"]["uri"].as_str().unwrap_or_default();
                assert!(
                    uri.contains(MARKER_PATH),
                    "case {:?}: the record must name the path asked for, got {uri:?} in \
                     {record}",
                    row.case
                );
                let redacted = logged_header(record, row.redacted);
                assert_eq!(
                    redacted, REDACTED,
                    "case {:?}: the redacted header must show {REDACTED:?}, got {redacted:?} \
                     in {record}",
                    row.case
                );
                let shown = logged_header(record, row.shown);
                assert_eq!(
                    shown, row.shown_value,
                    "case {:?}: a header no redaction names must show what the client sent, \
                     got {shown:?} in {record}",
                    row.case
                );
                if row.redacted == CREDENTIAL_HEADER {
                    let text = record.to_string();
                    assert!(
                        !text.contains(CREDENTIAL_VALUE),
                        "case {:?}: the redacted value must appear nowhere in the record, got \
                         {record}",
                        row.case
                    );
                }
            }
        }
    }
}
