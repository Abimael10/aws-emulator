// SPDX-License-Identifier: CC0-1.0

//! What a WAFv2 `Captcha` or `Challenge` action ANSWERS a request with at a deployed REST
//! API stage, end to end over the wire.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > A request a web ACL's token-based action INTERCEPTS — a `Captcha` or a `Challenge`
//! > rule that matches a request holding no valid token — is answered BY WAF out of that
//! > action's own documented envelope: the status the action serves its interstitial
//! > under AND the response header `x-amzn-waf-action` carrying that action's fixed
//! > token. `CaptchaAction` documents the pair ("the header `x-amzn-waf-action` with a
//! > value of `captcha`" and "the HTTP status code `405 Method Not Allowed`") and
//! > `ChallengeAction` documents its own (`challenge`, `202 Request Accepted`). No other
//! > decision carries that header — not an allowed request, not a block, not a counted
//! > match, and not a request the rule that produced it no longer exists for. WAF's own
//! > record of the request reports the same interception through `GetSampledRequests`:
//! > the action it applied, the response code it sent, and the token inspection's own
//! > `ResponseCode` and `FailureReason`.
//!
//! Only a cross-service suite can pin that: the api and its stage are API Gateway's, the
//! web ACL and the decision are WAFv2's, and the answer a client reads is the one the
//! stage's data plane renders from that decision — both mounted on ONE engine, as
//! `core/router/src/lib.rs` mounts them. Every assertion reads what a client reads: the
//! status and headers of an unsigned GET to the published invoke host, and the sample
//! `GetSampledRequests` draws for the rule's own dimension.
//!
//! The rows span what the rule generalises over: which of the two intercepting actions
//! decides, which request component its statement inspects, whether the request presents
//! a WAF token at all, whether the intercepting rule is the first to run or follows a
//! non-terminating one, and the three decisions that are NOT interceptions (a block, a
//! counted match the origin then serves, and a request no rule matches).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region the invoke host, both control planes and every ARN name.
const REGION: &str = "us-east-1";

/// The `Scope` an API Gateway stage's web ACL lives in: a stage is a REGIONAL resource.
const SCOPE: &str = "REGIONAL";

/// The stage every api below is deployed to.
const STAGE: &str = "prod";

/// What the stage's `MOCK` integration answers, so the body a request gets back says
/// whether it reached the origin.
const ORIGIN_BODY: &str = r#"{"served":"origin"}"#;
const ORIGIN_MARK: &str = "origin";

/// The header WAF stamps on a response it generates itself for an intercepted request,
/// and the two values it carries — `CaptchaAction` and `ChallengeAction` document both.
const ACTION_HEADER: &str = "x-amzn-waf-action";
const CAPTCHA_TOKEN: &str = "captcha";
const CHALLENGE_TOKEN: &str = "challenge";

/// The statuses the two actions serve their interstitials under: "the HTTP status code
/// `405 Method Not Allowed`" (`CaptchaAction`) and `202 Request Accepted`
/// (`ChallengeAction`).
const CAPTCHA_STATUS: u16 = 405;
const CHALLENGE_STATUS: u16 = 202;

/// The status a `Block` with no custom response is refused under at an API Gateway
/// stage, through the api's own `WAF_FILTERED` gateway response.
const BLOCK_STATUS: u16 = 403;

/// The members of a sample that report a token inspection
/// (`SampledHTTPRequest$CaptchaResponse`, `SampledHTTPRequest$ChallengeResponse`).
const CAPTCHA_RESPONSE: &str = "CaptchaResponse";
const CHALLENGE_RESPONSE: &str = "ChallengeResponse";

/// The `FailureReason` values the inspection reports, from the modelled enum
/// (`smithy_query(query="shape",shape="FailureReason")` → `TOKEN_MISSING`,
/// `TOKEN_EXPIRED`, `TOKEN_INVALID`, `TOKEN_DOMAIN_MISMATCH`): a request presenting no
/// token has none to evaluate, and one presenting a token no WAF issued presents an
/// invalid one.
const TOKEN_MISSING: &str = "TOKEN_MISSING";
const TOKEN_INVALID: &str = "TOKEN_INVALID";

/// The header a client presents a WAF token in, and the token one row presents: WAF's
/// own CAPTCHA and challenge scripts store the token they acquired in the
/// `aws-waf-token` cookie, and nothing here ever issued one, so a presented token is a
/// token no WAF issued.
const COOKIE_HEADER: &str = "cookie";
const TOKEN_PRESENTED: &str = "aws-waf-token=presented-by-the-client";

/// The header every request below carries a value of its own in, so the sample WAF
/// recorded for THAT request can be told from every other sample of the same dimension.
const MARKER_HEADER: &str = "x-waf-probe";

/// The header the `Captcha` rule that inspects a single header hunts for, and the value
/// it matches on.
const CAPTCHA_PROBE_HEADER: &str = "x-waf-captcha";
const CAPTCHA_PROBE_VALUE: &str = "solve-me";

/// The `VisibilityConfig$MetricName` of the WEB ACL's own dimension, which is the
/// dimension a request the `DefaultAction` decided is sampled under.
const WEB_ACL_METRIC: &str = "webAclDefault";

/// How long ONE request to the deployed stage is given to be answered. A request still
/// open after this was never going to be answered at all, so it is asserted rather than
/// waited out.
const ANSWER_DEADLINE: Duration = Duration::from_secs(60);

/// One emulator with API Gateway and WAFv2 mounted on a single shared engine.
struct Front {
    router: MultiServiceRouter,
}

/// One answered request, as a client reads it.
struct Answered {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Answered {
    /// The first value of the header `name`, or `None` when the answer carries none.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Every header name the answer carries, for an assertion that has to say what the
    /// answer held instead of the header it wanted.
    fn header_names(&self) -> Vec<String> {
        self.headers.iter().map(|(name, _)| name.clone()).collect()
    }
}

impl Front {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite exercises the services, not SigV4: the control-plane calls carry a
        // credential scope so the router can name the target service, and the invokes
        // carry no credential at all, as a client of a published invoke URL sends none.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_wafv2_awswaf_20190729::EmulatedService::new(Arc::clone(&engine)),
        );
        // The destination a logging configuration names is a CloudWatch Logs group, so the
        // record a web ACL delivers is read back through that service's own reader.
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// A SigV4 `Authorization` whose credential scope names `signing_name` in `REGION`,
    /// which is what routes the request. The signature itself is never checked here.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host, Signature=0000"
        )
    }

    /// Dispatch one request as it arrives on the wire.
    async fn dispatch(
        &self,
        method: &str,
        host: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Bytes,
    ) -> Answered {
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
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.to_string(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        let body = String::from_utf8_lossy(&response.into_body()).into_owned();
        Answered {
            status,
            headers,
            body,
        }
    }

    /// One CloudWatch Logs call, answering its status and parsed body.
    async fn logs(&self, operation: &str, body: Value) -> (u16, Value) {
        let answered = self
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
        (
            answered.status,
            serde_json::from_str(&answered.body).unwrap_or(Value::Null),
        )
    }

    /// The records delivered into the log group `group`, each parsed as the JSON line a
    /// WAF log record is.
    async fn delivered(&self, case: &str, group: &str) -> Vec<Value> {
        let (status, body) = self
            .logs("FilterLogEvents", json!({ "logGroupName": group }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the destination group {group:?} must hold the records the web \
             ACL delivered, got HTTP {status} with {body}"
        );
        body["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|event| {
                serde_json::from_str(event["message"].as_str().unwrap_or_default())
                    .unwrap_or(Value::Null)
            })
            .collect()
    }

    /// One API Gateway control-plane call that has to be accepted with `expected`.
    async fn api_ok(&self, method: &str, path: &str, body: Option<Value>, expected: u16) -> Value {
        let payload = body.map(|value| value.to_string()).unwrap_or_default();
        let answered = self
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
            answered.status, expected,
            "setup {method} {path}: expected HTTP {expected}, got {} with {:?}",
            answered.status, answered.body
        );
        serde_json::from_str(&answered.body).unwrap_or(Value::Null)
    }

    /// One WAFv2 call (`awsJson1_1`) that has to succeed.
    async fn waf_ok(&self, operation: &str, body: Value) -> Value {
        let answered = self
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
            answered.status, 200,
            "setup {operation}: expected HTTP 200, got {} with {:?}",
            answered.status, answered.body
        );
        serde_json::from_str(&answered.body).unwrap_or(Value::Null)
    }

    /// One unsigned GET to a deployed stage's invoke URL — the api's own host, no
    /// credential, as a client of a published invoke URL sends it.
    ///
    /// The request must be ANSWERED: a stage that leaves it open fails the case here
    /// rather than hanging the suite.
    async fn get(&self, case: &str, api: &str, path: &str, headers: &[(&str, &str)]) -> Answered {
        let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
        tokio::time::timeout(
            ANSWER_DEADLINE,
            self.dispatch("GET", &host, path, headers, Bytes::new()),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "case {case:?}: GET {path} must be answered by the stage within \
                 {ANSWER_DEADLINE:?}, got no HTTP response at all"
            )
        })
    }
}

/// What the token inspection of one intercepted request must report.
struct Inspection {
    /// The member of the sample that carries it.
    member: &'static str,
    /// Its `ResponseCode`: the status the inspection answered the request with.
    response_code: u16,
    /// Its `FailureReason`.
    failure_reason: &'static str,
}

/// One row: the request it drives, what the edge must answer it with, and what WAF's own
/// record of it must report.
struct Row {
    case: &'static str,
    /// The path under the stage the row drives.
    path: &'static str,
    /// The value this row's request carries in [`MARKER_HEADER`].
    marker: &'static str,
    /// The headers it carries beside its marker.
    headers: &'static [(&'static str, &'static str)],
    /// The dimension whose sample reports this request: the deciding rule's own
    /// `MetricName`, or the web ACL's for a request its `DefaultAction` decided.
    metric: &'static str,
    /// The status the edge must answer with.
    status: u16,
    /// The `x-amzn-waf-action` value the answer must carry, or `None` when it must carry
    /// no such header at all.
    action_header: Option<&'static str>,
    /// The `Action` the sample must report.
    sampled_action: &'static str,
    /// The token inspection the sample must report, or `None` when no action inspected a
    /// token and the sample must report neither inspection.
    inspection: Option<Inspection>,
    /// Whether the request must reach the origin.
    origin: bool,
    /// A dimension that COUNTED this request without deciding it, whose own sample must
    /// report it under `COUNT`.
    counted_by: Option<&'static str>,
}

/// The dimensions the rows' rules carry, each named here so a sample can be asked for by
/// the dimension rather than by the rule.
const PATH_PUZZLE_METRIC: &str = "pathPuzzleRule";
const HEADER_PUZZLE_METRIC: &str = "headerPuzzleRule";
const SILENT_METRIC: &str = "silentRule";
const LATE_PUZZLE_METRIC: &str = "latePuzzleRule";
const COUNTED_METRIC: &str = "countedRule";
const BLOCKED_METRIC: &str = "blockedRule";

/// The table.
const ROWS: [Row; 8] = [
    Row {
        case: "a Captcha rule on the URI path, the request presenting no token",
        path: "/puzzle",
        marker: "captcha-path",
        headers: &[],
        metric: PATH_PUZZLE_METRIC,
        status: CAPTCHA_STATUS,
        action_header: Some(CAPTCHA_TOKEN),
        sampled_action: "CAPTCHA",
        inspection: Some(Inspection {
            member: CAPTCHA_RESPONSE,
            response_code: CAPTCHA_STATUS,
            failure_reason: TOKEN_MISSING,
        }),
        origin: false,
        counted_by: None,
    },
    Row {
        case: "the same Captcha rule, the request presenting a token no WAF issued",
        path: "/puzzle",
        marker: "captcha-path-with-a-token",
        headers: &[(COOKIE_HEADER, TOKEN_PRESENTED)],
        metric: PATH_PUZZLE_METRIC,
        status: CAPTCHA_STATUS,
        action_header: Some(CAPTCHA_TOKEN),
        sampled_action: "CAPTCHA",
        inspection: Some(Inspection {
            member: CAPTCHA_RESPONSE,
            response_code: CAPTCHA_STATUS,
            failure_reason: TOKEN_INVALID,
        }),
        origin: false,
        counted_by: None,
    },
    Row {
        case: "a Captcha rule on a single header rather than on the path",
        path: "/plain",
        marker: "captcha-header",
        headers: &[(CAPTCHA_PROBE_HEADER, CAPTCHA_PROBE_VALUE)],
        metric: HEADER_PUZZLE_METRIC,
        status: CAPTCHA_STATUS,
        action_header: Some(CAPTCHA_TOKEN),
        sampled_action: "CAPTCHA",
        inspection: Some(Inspection {
            member: CAPTCHA_RESPONSE,
            response_code: CAPTCHA_STATUS,
            failure_reason: TOKEN_MISSING,
        }),
        origin: false,
        counted_by: None,
    },
    Row {
        case: "a Challenge rule, the request presenting no token",
        path: "/silent",
        marker: "challenge-path",
        headers: &[],
        metric: SILENT_METRIC,
        status: CHALLENGE_STATUS,
        action_header: Some(CHALLENGE_TOKEN),
        sampled_action: "CHALLENGE",
        inspection: Some(Inspection {
            member: CHALLENGE_RESPONSE,
            response_code: CHALLENGE_STATUS,
            failure_reason: TOKEN_MISSING,
        }),
        origin: false,
        counted_by: None,
    },
    Row {
        case: "the same Challenge rule, the request presenting a token no WAF issued",
        path: "/silent",
        marker: "challenge-path-with-a-token",
        headers: &[(COOKIE_HEADER, TOKEN_PRESENTED)],
        metric: SILENT_METRIC,
        status: CHALLENGE_STATUS,
        action_header: Some(CHALLENGE_TOKEN),
        sampled_action: "CHALLENGE",
        inspection: Some(Inspection {
            member: CHALLENGE_RESPONSE,
            response_code: CHALLENGE_STATUS,
            failure_reason: TOKEN_INVALID,
        }),
        origin: false,
        counted_by: None,
    },
    Row {
        case: "a Captcha rule that runs after a Count rule matched the same request",
        path: "/counted-puzzle",
        marker: "captcha-after-a-count",
        headers: &[],
        metric: LATE_PUZZLE_METRIC,
        status: CAPTCHA_STATUS,
        action_header: Some(CAPTCHA_TOKEN),
        sampled_action: "CAPTCHA",
        inspection: Some(Inspection {
            member: CAPTCHA_RESPONSE,
            response_code: CAPTCHA_STATUS,
            failure_reason: TOKEN_MISSING,
        }),
        origin: false,
        counted_by: Some(COUNTED_METRIC),
    },
    Row {
        case: "a Block rule, which WAF serves no envelope of its own for",
        path: "/blocked",
        marker: "blocked",
        headers: &[],
        metric: BLOCKED_METRIC,
        status: BLOCK_STATUS,
        action_header: None,
        sampled_action: "BLOCK",
        inspection: None,
        origin: false,
        counted_by: None,
    },
    Row {
        case: "a request no rule matches, which the DefaultAction allows",
        path: "/elsewhere",
        marker: "allowed",
        headers: &[],
        metric: WEB_ACL_METRIC,
        status: 200,
        action_header: None,
        sampled_action: "ALLOW",
        inspection: None,
        origin: true,
        counted_by: None,
    },
];

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

/// Wall-clock now as epoch seconds, the clock a sampling window is measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// A `Blob` member travels base64-encoded in an awsJson body, which is what
/// `ByteMatchStatement$SearchString` is.
fn base64_of(value: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
}

/// A `VisibilityConfig` naming `metric`, storing samples so `GetSampledRequests` has a
/// population to draw from.
fn visibility(metric: &str) -> Value {
    json!({
        "SampledRequestsEnabled": true,
        "CloudWatchMetricsEnabled": true,
        "MetricName": metric,
    })
}

/// A `ByteMatchStatement` over `field`, searching for `search` under `constraint`.
fn byte_match(field: Value, search: &str, constraint: &str) -> Value {
    json!({
        "ByteMatchStatement": {
            "SearchString": base64_of(search),
            "FieldToMatch": field,
            "TextTransformations": [{ "Priority": 0, "Type": "NONE" }],
            "PositionalConstraint": constraint,
        }
    })
}

/// One rule: a name that is also its dimension, a priority, a statement and an action.
fn rule(metric: &str, priority: i64, statement: Value, action: Value) -> Value {
    json!({
        "Name": metric,
        "Priority": priority,
        "Statement": statement,
        "Action": action,
        "VisibilityConfig": visibility(metric),
    })
}

/// A rule whose statement matches a request whose URI path — `/{stage}/{resource-path}`,
/// the path AWS's edge inspects — ends in `path`.
fn on_path(metric: &str, priority: i64, path: &str, action: Value) -> Value {
    rule(
        metric,
        priority,
        byte_match(json!({ "UriPath": {} }), path, "ENDS_WITH"),
        action,
    )
}

/// Every rule the table's rows are decided by, in the priority order they run in.
fn rules() -> Value {
    json!([
        on_path(PATH_PUZZLE_METRIC, 0, "/puzzle", json!({ "Captcha": {} })),
        rule(
            HEADER_PUZZLE_METRIC,
            1,
            byte_match(
                json!({ "SingleHeader": { "Name": CAPTCHA_PROBE_HEADER } }),
                CAPTCHA_PROBE_VALUE,
                "EXACTLY",
            ),
            json!({ "Captcha": {} }),
        ),
        on_path(SILENT_METRIC, 2, "/silent", json!({ "Challenge": {} })),
        // The Count rule runs BEFORE the Captcha rule over the same request, so the
        // interception is made by a rule that is not the first to match.
        on_path(COUNTED_METRIC, 3, "/counted-puzzle", json!({ "Count": {} })),
        on_path(
            LATE_PUZZLE_METRIC,
            4,
            "/counted-puzzle",
            json!({ "Captcha": {} }),
        ),
        on_path(BLOCKED_METRIC, 5, "/blocked", json!({ "Block": {} })),
    ])
}

/// An api with a `{proxy+}` catch-all on `ANY`, whose `MOCK` integration answers `200`
/// with [`ORIGIN_BODY`], deployed to [`STAGE`]. Answers the api's id.
async fn deployed_api(front: &Front, name: &str) -> String {
    let created = front
        .api_ok("POST", "/restapis", Some(json!({ "name": name })), 201)
        .await;
    let api = created["id"].as_str().expect("the api id").to_string();
    let root = created["rootResourceId"]
        .as_str()
        .expect("the root resource id")
        .to_string();
    let proxy = front
        .api_ok(
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": "{proxy+}" })),
            201,
        )
        .await;
    let proxy = proxy["id"].as_str().expect("the resource id").to_string();
    let method = format!("/restapis/{api}/resources/{proxy}/methods/ANY");
    front
        .api_ok(
            "PUT",
            &method,
            Some(json!({ "authorizationType": "NONE" })),
            201,
        )
        .await;
    front
        .api_ok(
            "PUT",
            &format!("{method}/integration"),
            Some(json!({ "type": "MOCK" })),
            201,
        )
        .await;
    front
        .api_ok("PUT", &format!("{method}/responses/200"), Some(json!({})), 201)
        .await;
    front
        .api_ok(
            "PUT",
            &format!("{method}/integration/responses/200"),
            Some(json!({ "responseTemplates": { "application/json": ORIGIN_BODY } })),
            201,
        )
        .await;
    front
        .api_ok(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": STAGE })),
            201,
        )
        .await;
    api
}

/// One created web ACL, by the identifiers a later update needs.
struct WebAcl {
    id: String,
    name: String,
    arn: String,
    lock_token: String,
}

/// Create a web ACL named `name` whose `DefaultAction` allows, carrying `rules`.
async fn create_web_acl(front: &Front, name: &str, rules: Value) -> WebAcl {
    let created = front
        .waf_ok(
            "CreateWebACL",
            json!({
                "Name": name,
                "Scope": SCOPE,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility(WEB_ACL_METRIC),
                "Rules": rules,
            }),
        )
        .await;
    let summary = &created["Summary"];
    WebAcl {
        id: summary["Id"].as_str().expect("Summary.Id").to_string(),
        name: name.to_string(),
        arn: summary["ARN"].as_str().expect("Summary.ARN").to_string(),
        lock_token: summary["LockToken"]
            .as_str()
            .expect("Summary.LockToken")
            .to_string(),
    }
}

/// Put `web_acl` in front of `api`'s stage.
async fn associate(front: &Front, web_acl: &WebAcl, api: &str) {
    front
        .waf_ok(
            "AssociateWebACL",
            json!({ "WebACLArn": web_acl.arn, "ResourceArn": stage_arn(api, STAGE) }),
        )
        .await;
}

/// The request `row` drives, answered by the stage.
async fn drive(front: &Front, api: &str, row: &Row) -> Answered {
    let mut headers: Vec<(&str, &str)> = vec![(MARKER_HEADER, row.marker)];
    headers.extend_from_slice(row.headers);
    front
        .get(
            row.case,
            api,
            &format!("/{STAGE}{}", row.path),
            &headers,
        )
        .await
}

/// The sample WAF recorded for the request carrying `marker` under the dimension
/// `metric`, out of `GetSampledRequests`.
///
/// The marker is what makes the lookup unambiguous: two rows may be decided by the same
/// rule, so the dimension alone does not name one request's record.
async fn sample_for(front: &Front, web_acl: &WebAcl, case: &str, metric: &str, marker: &str) -> Value {
    let at = now_secs();
    let answered = front
        .waf_ok(
            "GetSampledRequests",
            json!({
                "WebAclArn": web_acl.arn,
                "RuleMetricName": metric,
                "Scope": SCOPE,
                "MaxItems": 500,
                "TimeWindow": { "StartTime": at - 900.0, "EndTime": at + 60.0 },
            }),
        )
        .await;
    let samples = answered["SampledRequests"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    samples
        .into_iter()
        .find(|sample| {
            sample["Request"]["Headers"]
                .as_array()
                .map(|headers| {
                    headers.iter().any(|header| {
                        header["Name"]
                            .as_str()
                            .is_some_and(|name| name.eq_ignore_ascii_case(MARKER_HEADER))
                            && header["Value"].as_str() == Some(marker)
                    })
                })
                .unwrap_or_default()
        })
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the dimension {metric} must hold a sample of the request \
                 marked {marker:?}, got {answered}"
            )
        })
}

/// Assert the answer `row` must get, and the record WAF must keep of it.
async fn assert_row(front: &Front, api: &str, web_acl: &WebAcl, row: &Row) {
    let answered = drive(front, api, row).await;
    assert_eq!(
        answered.status, row.status,
        "case {:?}: expected HTTP {}, got {} with {:?}",
        row.case, row.status, answered.status, answered.body
    );
    assert_eq!(
        answered.header(ACTION_HEADER),
        row.action_header,
        "case {:?}: expected {ACTION_HEADER}: {:?}, got {:?} among {:?}",
        row.case,
        row.action_header,
        answered.header(ACTION_HEADER),
        answered.header_names()
    );
    assert_eq!(
        answered.body.contains(ORIGIN_MARK),
        row.origin,
        "case {:?}: expected the origin to {}serve this request, got {:?}",
        row.case,
        if row.origin { "" } else { "not " },
        answered.body
    );

    let sample = sample_for(front, web_acl, row.case, row.metric, row.marker).await;
    assert_eq!(
        sample["Action"].as_str(),
        Some(row.sampled_action),
        "case {:?}: expected the sample to report Action {:?}, got {sample}",
        row.case,
        row.sampled_action
    );
    // The response code WAF ITSELF sent: the envelope an interception is answered out of,
    // and nothing for a decision the protected resource's own service worded.
    let sent = sample["ResponseCodeSent"].as_u64();
    let expected_sent = row.inspection.as_ref().map(|it| u64::from(it.response_code));
    assert_eq!(
        sent, expected_sent,
        "case {:?}: expected ResponseCodeSent {expected_sent:?}, got {sample}",
        row.case
    );
    for member in [CAPTCHA_RESPONSE, CHALLENGE_RESPONSE] {
        let expected = row
            .inspection
            .as_ref()
            .filter(|inspection| inspection.member == member)
            .map(|inspection| {
                json!({
                    "ResponseCode": inspection.response_code,
                    "FailureReason": inspection.failure_reason,
                })
            });
        match expected {
            Some(expected) => {
                assert_eq!(
                    sample[member]["ResponseCode"].as_u64(),
                    expected["ResponseCode"].as_u64(),
                    "case {:?}: expected {member}.ResponseCode {}, got {sample}",
                    row.case,
                    expected["ResponseCode"]
                );
                assert_eq!(
                    sample[member]["FailureReason"].as_str(),
                    expected["FailureReason"].as_str(),
                    "case {:?}: expected {member}.FailureReason {}, got {sample}",
                    row.case,
                    expected["FailureReason"]
                );
            }
            None => assert!(
                sample[member].is_null(),
                "case {:?}: a decision that inspected no {member} token must report none, \
                 got {sample}",
                row.case
            ),
        }
    }

    // A rule that only COUNTED the request records it under its own dimension, under
    // `COUNT`, which is what says the interception was made by the rule that terminated
    // the evaluation rather than by the one before it.
    if let Some(counted_by) = row.counted_by {
        let counted = sample_for(front, web_acl, row.case, counted_by, row.marker).await;
        assert_eq!(
            counted["Action"].as_str(),
            Some("COUNT"),
            "case {:?}: expected the dimension {counted_by} to report Action COUNT, got \
             {counted}",
            row.case
        );
    }
}

#[tokio::test]
async fn an_intercepted_request_is_answered_out_of_the_actions_own_envelope() {
    let front = Front::new();
    let api = deployed_api(&front, "intercepted-action-api").await;
    let web_acl = create_web_acl(&front, "intercepted-action-acl", rules()).await;
    associate(&front, &web_acl, &api).await;

    for row in &ROWS {
        assert_row(&front, &api, &web_acl, row).await;
    }
}

/// RULE: the header is part of the INTERCEPTION, not of the stage — a stage with no web
/// ACL in front of it carries none, and neither does the same request once the rule that
/// intercepted it is gone.
#[tokio::test]
async fn the_action_header_is_carried_only_while_a_rule_intercepts_the_request() {
    let front = Front::new();
    let api = deployed_api(&front, "intercepted-action-binding-api").await;

    // Before any web ACL exists: the origin serves the request and carries no such
    // header.
    let answered = front
        .get(
            "no web ACL in front of the stage",
            &api,
            &format!("/{STAGE}/puzzle"),
            &[(MARKER_HEADER, "before")],
        )
        .await;
    assert_eq!(
        (answered.status, answered.header(ACTION_HEADER)),
        (200, None),
        "case no web ACL in front of the stage: expected the origin's 200 and no \
         {ACTION_HEADER}, got {} with {:?} among {:?}",
        answered.status,
        answered.header(ACTION_HEADER),
        answered.header_names()
    );

    let web_acl = create_web_acl(
        &front,
        "intercepted-action-binding-acl",
        json!([on_path(
            PATH_PUZZLE_METRIC,
            0,
            "/puzzle",
            json!({ "Captcha": {} })
        )]),
    )
    .await;
    associate(&front, &web_acl, &api).await;

    let answered = front
        .get(
            "the Captcha rule in force",
            &api,
            &format!("/{STAGE}/puzzle"),
            &[(MARKER_HEADER, "intercepted")],
        )
        .await;
    assert_eq!(
        (answered.status, answered.header(ACTION_HEADER)),
        (CAPTCHA_STATUS, Some(CAPTCHA_TOKEN)),
        "case the Captcha rule in force: expected HTTP {CAPTCHA_STATUS} and \
         {ACTION_HEADER}: {CAPTCHA_TOKEN}, got {} with {:?} among {:?}",
        answered.status,
        answered.header(ACTION_HEADER),
        answered.header_names()
    );

    // The rule taken out of the web ACL: the same request is served by the origin again,
    // with no trace of the interception on the answer.
    front
        .waf_ok(
            "UpdateWebACL",
            json!({
                "Name": web_acl.name,
                "Scope": SCOPE,
                "Id": web_acl.id,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility(WEB_ACL_METRIC),
                "Rules": [],
                "LockToken": web_acl.lock_token,
            }),
        )
        .await;
    let answered = front
        .get(
            "the Captcha rule removed",
            &api,
            &format!("/{STAGE}/puzzle"),
            &[(MARKER_HEADER, "restored")],
        )
        .await;
    assert_eq!(
        (answered.status, answered.header(ACTION_HEADER)),
        (200, None),
        "case the Captcha rule removed: expected the origin's 200 and no {ACTION_HEADER}, \
         got {} with {:?} among {:?}",
        answered.status,
        answered.header(ACTION_HEADER),
        answered.header_names()
    );
}

/// RULE: the record a web ACL's `LoggingConfiguration` delivers reports what WAF itself
/// answered the request with — `responseCodeSent` is "the response code that was sent for
/// the request", so an interception reports the status of the envelope WAF generated,
/// while a refusal the protected resource's own service worded reports none.
#[tokio::test]
async fn the_log_record_of_an_intercepted_request_reports_the_status_waf_sent() {
    let front = Front::new();
    let api = deployed_api(&front, "intercepted-action-logged-api").await;
    let web_acl = create_web_acl(&front, "intercepted-action-logged-acl", rules()).await;
    associate(&front, &web_acl, &api).await;
    // A destination a `LoggingConfiguration` may name: a log group whose name carries the
    // prefix WAF requires of one, created before the put names it — WAF resolves the
    // destination it is pointed at before it stores the configuration.
    let group = "aws-waf-logs-intercepted-action";
    let (created, created_body) = front
        .logs("CreateLogGroup", json!({ "logGroupName": group }))
        .await;
    assert_eq!(
        created, 200,
        "setup CreateLogGroup({group}): expected HTTP 200, got {created} with {created_body}"
    );
    front
        .waf_ok(
            "PutLoggingConfiguration",
            json!({
                "LoggingConfiguration": {
                    "ResourceArn": web_acl.arn,
                    "LogDestinationConfigs": [group_arn(group)],
                }
            }),
        )
        .await;

    // The two interceptions, and the block whose answer the stage itself words: the rows
    // differ in which action decided and so in whether WAF sent a status at all.
    let logged: [(&str, &str, &str, &str, Option<u64>); 3] = [
        (
            "a Captcha interception",
            "/puzzle",
            "logged-captcha",
            "CAPTCHA",
            Some(u64::from(CAPTCHA_STATUS)),
        ),
        (
            "a Challenge interception",
            "/silent",
            "logged-challenge",
            "CHALLENGE",
            Some(u64::from(CHALLENGE_STATUS)),
        ),
        (
            "a Block, whose answer the stage itself words",
            "/blocked",
            "logged-block",
            "BLOCK",
            None,
        ),
    ];
    for (case, path, marker, _, _) in logged {
        front
            .get(case, &api, &format!("/{STAGE}{path}"), &[(MARKER_HEADER, marker)])
            .await;
    }

    let records = front.delivered("the delivered records", group).await;
    for (case, _, marker, action, sent) in logged {
        let record = records
            .iter()
            .find(|record| {
                record["httpRequest"]["headers"]
                    .as_array()
                    .map(|headers| {
                        headers.iter().any(|header| {
                            header["name"]
                                .as_str()
                                .is_some_and(|name| name.eq_ignore_ascii_case(MARKER_HEADER))
                                && header["value"].as_str() == Some(marker)
                        })
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the web ACL must deliver a record of the request marked \
                     {marker:?}, got {records:?}"
                )
            });
        assert_eq!(
            record["action"].as_str(),
            Some(action),
            "case {case:?}: expected the record to report action {action:?}, got {record}"
        );
        assert_eq!(
            record["responseCodeSent"].as_u64(),
            sent,
            "case {case:?}: expected responseCodeSent {sent:?}, got {record}"
        );
    }
}
