// SPDX-License-Identifier: CC0-1.0

//! What a WAFv2 web ACL in front of a deployed API Gateway REST stage makes of the
//! request's PATH, on the assembled emulator — and what a `RegexPatternSet` a live rule
//! references makes of its own delete.
//!
//! RULE: **the path a protected resource's own service hands the web-ACL inspection hook
//! is the request's path WITHIN the api — `/{stage}/{resource-path}`, a URI path, so it
//! begins with `/` — and it is the same text whichever addressing the client reached this
//! emulator by, so a rule whose criterion is written against the path decides every one of
//! those requests the same way.** `FieldToMatch$UriPath` is "the part of the URL that
//! identifies a resource, for example, /images/daily-ad.jpg", and the emulator's own
//! addressing prefix (the api id a client that resolves no `execute-api` host carries in
//! the path instead of in the host) is how the request reached this process, not part of
//! the path the stage serves.
//!
//! The second rule, over the same wiring: **while ANY stored rule tree still names a
//! `RegexPatternSet`, `DeleteRegexPatternSet` is refused with `WAFAssociatedItemException`
//! and has NO effect — the same `RegularExpressionList`, the same `LockToken`, the same
//! decision at the edge — and the identical delete succeeds once nothing names it.**
//!
//! Only a cross-service suite can pin either: the api, its stage and the response the
//! client gets are API Gateway's, the pattern set, the web ACL and the refusal are WAFv2's,
//! and both are reached through `aws_emulator_router::build_router` on one engine — so
//! every decision asserted here is one a client observes at an address this emulator
//! serves.
//!
//! The rows differ along what the rules generalise over: the ADDRESSING the client reached
//! the stage by (the api's own endpoint host, the api id carried in the path, and the two
//! spellings of the reserved data-plane prefix), the kind of criterion written against the
//! path (a referenced regex pattern set, a rule's own regular expression, a byte match),
//! where that criterion is anchored (both ends, the start, the whole path, a substring),
//! the request component it names (the path, the query string), and how many expressions
//! the referenced set holds. Each row drives a request the criterion selects and a sibling
//! it does not, so no row can pass by refusing — or serving — everything.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region the invoke host, the ARNs and both control planes name.
const REGION: &str = "us-west-2";

/// The `Scope` an API Gateway stage's web ACL lives in: a stage is a REGIONAL resource.
const SCOPE: &str = "REGIONAL";

/// The stage every request below is sent to.
const STAGE: &str = "prod";

/// The status API Gateway answers a request the web ACL blocked with.
const BLOCKED_STATUS: u16 = 403;

/// The marker the `MOCK` integration of every resource echoes, so the body of an answer
/// says whether the request reached the origin at all.
const ORIGIN_MARKER: &str = "at-the-origin";

/// A host that names NO service to the emulator's front door: the emulator's own endpoint,
/// which is what a client that resolves none of AWS's hosts dials, carrying in the PATH
/// what it cannot carry in the host.
const EMULATOR_HOST: &str = "localhost:9297";

/// The reserved path segment that names a data plane on the emulator's own endpoint, and
/// the `execute-api` label an api's endpoint host carries — the two parts of the prefixed
/// path-style addressing
/// (`service_com_amazonaws_apigateway_backplanecontrolservice::service::invoke` documents
/// both).
const EMULATOR_PATH_NAMESPACE: &str = "_aws";
const EXECUTE_API_LABEL: &str = "execute-api";

/// `WAFAssociatedItemException`'s sentence, as WAF words it for this error class: fixed
/// text, with no request-derived value.
const ASSOCIATED_ITEM: &str = "AWS WAF couldn’t perform the operation because your \
     resource is being used by another resource or it’s associated with another \
     resource.";

/// `WAFNonexistentItemException`'s sentence, likewise fixed.
const NONEXISTENT_ITEM: &str =
    "AWS WAF couldn’t perform the operation because your resource doesn’t exist.";

/// How long a decision the control plane has just been told about is given to settle at the
/// edge, and how long to wait between looks. Generous: the suite waits for the DECISION,
/// never for a duration, and fails only if one never arrives at all.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);
const SETTLE_POLL: Duration = Duration::from_millis(20);

/// How many times in a row the stage must answer a probe the same way for its decision to
/// count as settled: a decision that drifts between looks is as unusable to a client as one
/// that never arrives.
const STABLE_OBSERVATIONS: usize = 3;

/// The resource path parts every api below carries, each with its own `GET` method on a
/// `MOCK` integration.
const PATH_PARTS: &[&str] = &["rx-4821", "ok-4821", "Admin"];

/// One way a client ADDRESSES a deployed stage of this emulator.
#[derive(Clone, Copy, Debug)]
enum Addressing {
    /// The api's own endpoint host, which is the address AWS publishes.
    EndpointHost,
    /// The emulator's own endpoint, with the api id as the first path segment.
    ApiIdInThePath,
    /// The emulator's own endpoint, with the reserved data-plane prefix and the
    /// `execute-api` label before the api id.
    ReservedPrefixLabel,
    /// The same prefix, with the api's whole endpoint HOST carried across in the path.
    ReservedPrefixHost,
}

impl Addressing {
    /// The host and the request target this addressing sends `invoke_path` (the path within
    /// the api, `/{stage}/{resource-path}`) to.
    fn target(self, api: &str, invoke_path: &str) -> (String, String) {
        let endpoint_host = format!("{api}.execute-api.{REGION}.amazonaws.com");
        match self {
            Self::EndpointHost => (endpoint_host, invoke_path.to_string()),
            Self::ApiIdInThePath => (
                EMULATOR_HOST.to_string(),
                format!("/{api}{invoke_path}"),
            ),
            Self::ReservedPrefixLabel => (
                EMULATOR_HOST.to_string(),
                format!("/{EMULATOR_PATH_NAMESPACE}/{EXECUTE_API_LABEL}/{api}{invoke_path}"),
            ),
            Self::ReservedPrefixHost => (
                EMULATOR_HOST.to_string(),
                format!("/{EMULATOR_PATH_NAMESPACE}/{endpoint_host}{invoke_path}"),
            ),
        }
    }
}

/// Every addressing a row is driven on.
const ADDRESSINGS: &[Addressing] = &[
    Addressing::EndpointHost,
    Addressing::ApiIdInThePath,
    Addressing::ReservedPrefixLabel,
    Addressing::ReservedPrefixHost,
];

/// The emulator as it is ASSEMBLED, on one engine.
struct Emulator {
    router: MultiServiceRouter,
}

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
    body: String,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = aws_emulator_router::build_router(engine);
        // The suite exercises the services, not SigV4: a control-plane call carries a
        // credential scope so the router can name the target service, and the invoke of a
        // published stage address carries no credential at all, as a client of one sends
        // none.
        router.set_request_authentication(false);
        Self { router }
    }

    /// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
    /// routes a control-plane request. The signature itself is never checked here.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
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
        let body = String::from_utf8_lossy(&response.into_body()).into_owned();
        Answered { status, body }
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

    /// One WAFv2 call, answering its status and parsed body.
    async fn waf(&self, operation: &str, body: Value) -> (u16, Value) {
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
        let parsed = serde_json::from_str(&answered.body).unwrap_or(Value::Null);
        (answered.status, parsed)
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

    /// One unsigned request to the deployed stage, on `addressing`.
    async fn invoke(
        &self,
        api: &str,
        addressing: Addressing,
        path: &str,
        headers: &[(&str, &str)],
    ) -> Answered {
        let (host, target) = addressing.target(api, path);
        self.dispatch("GET", &host, &target, headers, Bytes::new())
            .await
    }
}

/// The error code an awsJson refusal reports, from the `__type` member the protocol carries
/// it in.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The message an awsJson refusal reports, whichever casing the member carries.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// The ARN `AssociateWebACL` binds a web ACL to a REST API stage by, the one form
/// `AssociateWebACLRequest$ResourceArn` documents for an API Gateway REST API stage.
fn stage_arn(api: &str) -> String {
    format!("arn:aws:apigateway:{REGION}::/restapis/{api}/stages/{STAGE}")
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

/// Build an api with one `GET` method on a `MOCK` integration for each path part in
/// [`PATH_PARTS`], deploy it to [`STAGE`], and answer the api's id.
async fn deployed_api(emulator: &Emulator, name: &str) -> String {
    let created = emulator
        .api_ok(
            "POST",
            "/restapis",
            Some(json!({
                "name": name,
                "endpointConfiguration": { "types": ["REGIONAL"] },
            })),
            201,
        )
        .await;
    let api = created["id"].as_str().expect("the api id").to_string();
    let root = created["rootResourceId"]
        .as_str()
        .expect("the root resource id")
        .to_string();
    for part in PATH_PARTS {
        let resource = emulator
            .api_ok(
                "POST",
                &format!("/restapis/{api}/resources/{root}"),
                Some(json!({ "pathPart": part })),
                201,
            )
            .await;
        let resource = resource["id"].as_str().expect("the resource id").to_string();
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
                Some(json!({
                    "type": "MOCK",
                    "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
                })),
                201,
            )
            .await;
        emulator
            .api_ok(
                "PUT",
                &format!("/restapis/{api}/resources/{resource}/methods/GET/responses/200"),
                Some(json!({ "responseModels": { "application/json": "Empty" } })),
                201,
            )
            .await;
        emulator
            .api_ok(
                "PUT",
                &format!(
                    "/restapis/{api}/resources/{resource}/methods/GET/integration/responses/200"
                ),
                Some(json!({
                    "responseTemplates": {
                        "application/json":
                            format!("{{\"at\":\"{ORIGIN_MARKER}\",\"path\":\"$context.resourcePath\"}}")
                    }
                })),
                201,
            )
            .await;
    }
    emulator
        .api_ok(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": STAGE })),
            201,
        )
        .await;
    api
}

/// One request a row drives: the path within the api it asks for, and the headers it
/// carries.
struct Probe {
    path: &'static str,
    headers: &'static [(&'static str, &'static str)],
}

/// How a row's criterion is written: against a `RegexPatternSet` the rule REFERENCES, or
/// inside the rule's own statement.
enum Criterion {
    /// A `RegexPatternSetReferenceStatement` over a set holding these expressions.
    ReferencedSet(&'static [&'static str]),
    /// A `RegexMatchStatement` carrying this expression itself.
    OwnRegex(&'static str),
    /// A `ByteMatchStatement` looking for this string under this `PositionalConstraint`.
    ByteMatch(&'static str, &'static str),
}

/// One row of the criterion table.
struct Row {
    case: &'static str,
    criterion: Criterion,
    /// The `FieldToMatch` the criterion names.
    field: fn() -> Value,
    /// The request the criterion selects, which the rule must block.
    matching: Probe,
    /// A sibling request it does not select, which the origin must serve.
    control: Probe,
}

/// The criterion table: every form of path criterion this emulator serves, plus a
/// component other than the path, so a row cannot pass merely because every criterion is
/// evaluated against the same text.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a referenced set whose expression is anchored at both ends of the path",
            criterion: Criterion::ReferencedSet(&["^/[^/]+/rx-[0-9]{4}$"]),
            field: || json!({ "UriPath": {} }),
            matching: Probe {
                path: "/prod/rx-4821",
                headers: &[],
            },
            control: Probe {
                path: "/prod/ok-4821",
                headers: &[],
            },
        },
        Row {
            case: "a referenced set whose expression is anchored at the start of the path",
            criterion: Criterion::ReferencedSet(&["^/prod/rx-"]),
            field: || json!({ "UriPath": {} }),
            matching: Probe {
                path: "/prod/rx-4821",
                headers: &[],
            },
            control: Probe {
                path: "/prod/ok-4821",
                headers: &[],
            },
        },
        Row {
            case: "a referenced set of several expressions, the last of which matches",
            criterion: Criterion::ReferencedSet(&[
                "^/never/matches$",
                "nothing-like-this",
                "/rx-[0-9]{4}$",
            ]),
            field: || json!({ "UriPath": {} }),
            matching: Probe {
                path: "/prod/rx-4821",
                headers: &[],
            },
            control: Probe {
                path: "/prod/ok-4821",
                headers: &[],
            },
        },
        Row {
            case: "the rule's own anchored regular expression over the path",
            criterion: Criterion::OwnRegex("^/[^/]+/Admin$"),
            field: || json!({ "UriPath": {} }),
            matching: Probe {
                path: "/prod/Admin",
                headers: &[],
            },
            control: Probe {
                path: "/prod/ok-4821",
                headers: &[],
            },
        },
        Row {
            case: "a byte match on the whole path",
            criterion: Criterion::ByteMatch("/prod/rx-4821", "EXACTLY"),
            field: || json!({ "UriPath": {} }),
            matching: Probe {
                path: "/prod/rx-4821",
                headers: &[],
            },
            control: Probe {
                path: "/prod/ok-4821",
                headers: &[],
            },
        },
        Row {
            case: "a byte match on the start of the path",
            criterion: Criterion::ByteMatch("/prod/Admin", "STARTS_WITH"),
            field: || json!({ "UriPath": {} }),
            matching: Probe {
                path: "/prod/Admin",
                headers: &[],
            },
            control: Probe {
                path: "/prod/rx-4821",
                headers: &[],
            },
        },
        Row {
            case: "a referenced set over the query string rather than the path",
            criterion: Criterion::ReferencedSet(&["(^|&)debug=1($|&)"]),
            field: || json!({ "QueryString": {} }),
            matching: Probe {
                path: "/prod/ok-4821?debug=1",
                headers: &[],
            },
            control: Probe {
                path: "/prod/ok-4821?debug=0",
                headers: &[],
            },
        },
    ]
}

/// The statement a row's criterion is, created with whatever resource it needs; answers the
/// statement and, for a referenced set, the `(name, id)` of that set.
async fn statement_of(
    emulator: &Emulator,
    row: &Row,
    index: usize,
) -> (Value, Option<(String, String)>) {
    match &row.criterion {
        Criterion::ReferencedSet(expressions) => {
            let name = format!("referenced-set-{index}");
            let (arn, id) = create_pattern_set(emulator, &name, expressions).await;
            (
                json!({
                    "RegexPatternSetReferenceStatement": {
                        "ARN": arn,
                        "FieldToMatch": (row.field)(),
                        "TextTransformations": [{ "Priority": 0, "Type": "NONE" }],
                    }
                }),
                Some((name, id)),
            )
        }
        Criterion::OwnRegex(expression) => (
            json!({
                "RegexMatchStatement": {
                    "RegexString": expression,
                    "FieldToMatch": (row.field)(),
                    "TextTransformations": [{ "Priority": 0, "Type": "NONE" }],
                }
            }),
            None,
        ),
        Criterion::ByteMatch(search, constraint) => (
            json!({
                "ByteMatchStatement": {
                    "SearchString": base64_of(search),
                    "FieldToMatch": (row.field)(),
                    "TextTransformations": [{ "Priority": 0, "Type": "NONE" }],
                    "PositionalConstraint": constraint,
                }
            }),
            None,
        ),
    }
}

/// Create a regex pattern set named `name` holding `expressions`, answering `(arn, id)`.
async fn create_pattern_set(
    emulator: &Emulator,
    name: &str,
    expressions: &[&str],
) -> (String, String) {
    let list: Vec<Value> = expressions
        .iter()
        .map(|expression| json!({ "RegexString": expression }))
        .collect();
    let created = emulator
        .waf_ok(
            "CreateRegexPatternSet",
            json!({ "Name": name, "Scope": SCOPE, "RegularExpressionList": list }),
        )
        .await;
    let summary = &created["Summary"];
    (
        summary["ARN"].as_str().expect("Summary.ARN").to_string(),
        summary["Id"].as_str().expect("Summary.Id").to_string(),
    )
}

/// Create a web ACL that allows by default and blocks on `statement`, answering
/// `(arn, id)`.
async fn create_web_acl(emulator: &Emulator, name: &str, statement: Value) -> (String, String) {
    let created = emulator
        .waf_ok(
            "CreateWebACL",
            json!({
                "Name": name,
                "Scope": SCOPE,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility("webacl"),
                "Rules": [{
                    "Name": "on-the-request",
                    "Priority": 0,
                    "Statement": statement,
                    "Action": { "Block": {} },
                    "VisibilityConfig": visibility("ontherequest"),
                }],
            }),
        )
        .await;
    let summary = &created["Summary"];
    (
        summary["ARN"].as_str().expect("Summary.ARN").to_string(),
        summary["Id"].as_str().expect("Summary.Id").to_string(),
    )
}

/// Ask for `probe` on `addressing` until the stage answers it with `expected`
/// [`STABLE_OBSERVATIONS`] times in a row, or [`SETTLE_DEADLINE`] passes: a binding the
/// control plane has just accepted is waited for by LOOKING again, never by sleeping a
/// fixed time.
async fn settled(
    emulator: &Emulator,
    api: &str,
    addressing: Addressing,
    case: &str,
    probe: &Probe,
    expected: u16,
) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut in_a_row = 0_usize;
    loop {
        let answered = emulator.invoke(api, addressing, probe.path, probe.headers).await;
        if answered.status == expected {
            in_a_row += 1;
            if in_a_row >= STABLE_OBSERVATIONS {
                return answered.body;
            }
            continue;
        }
        in_a_row = 0;
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stage to answer GET {} on {addressing:?} with HTTP \
             {expected} {STABLE_OBSERVATIONS} times in a row, got {} with {:?}",
            probe.path,
            answered.status,
            answered.body
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// RULE: a rule whose criterion is written against the request's path decides every
/// addressing of the same stage request the same way — and a criterion over another
/// component does too.
#[tokio::test]
async fn a_criterion_decides_the_same_way_on_every_addressing_of_the_stage() {
    let emulator = Emulator::new();
    let api = deployed_api(&emulator, "path-criterion-edge").await;
    let resource = stage_arn(&api);

    // Every addressing reaches the SAME deployed stage before any web ACL is in front of
    // it, so a later refusal is the web ACL's and not the addressing's.
    for addressing in ADDRESSINGS {
        for part in PATH_PARTS {
            let case = format!("{addressing:?} {part}");
            let answered = emulator
                .invoke(&api, *addressing, &format!("/{STAGE}/{part}"), &[])
                .await;
            assert_eq!(
                answered.status, 200,
                "setup {case:?}: expected the deployed stage to serve HTTP 200, got {} with \
                 {:?}",
                answered.status, answered.body
            );
            assert!(
                answered.body.contains(ORIGIN_MARKER),
                "setup {case:?}: expected the origin's own answer, got {:?}",
                answered.body
            );
        }
    }

    for (index, row) in rows().iter().enumerate() {
        let (statement, _set) = statement_of(&emulator, row, index).await;
        let (acl_arn, _acl_id) =
            create_web_acl(&emulator, &format!("criterion-acl-{index}"), statement).await;
        emulator
            .waf_ok(
                "AssociateWebACL",
                json!({ "WebACLArn": &acl_arn, "ResourceArn": &resource }),
            )
            .await;

        for addressing in ADDRESSINGS {
            let case = format!("{}, reached {addressing:?}", row.case);
            let blocked =
                settled(&emulator, &api, *addressing, &case, &row.matching, BLOCKED_STATUS).await;
            assert!(
                !blocked.contains(ORIGIN_MARKER),
                "case {case:?}: a blocked request never reaches the origin, got {blocked:?}"
            );
            let served = settled(&emulator, &api, *addressing, &case, &row.control, 200).await;
            assert!(
                served.contains(ORIGIN_MARKER),
                "case {case:?}: expected the origin's own answer for the sibling request, got \
                 {served:?}"
            );
        }

        emulator
            .waf_ok("DisassociateWebACL", json!({ "ResourceArn": &resource }))
            .await;
    }
}

/// RULE: while a live web ACL rule names the set, `DeleteRegexPatternSet` is refused with
/// `WAFAssociatedItemException` and changes nothing — then succeeds once the rule is gone.
///
/// Driven on every addressing of the stage, because what the refusal must leave untouched
/// includes the decision the referencing rule keeps making at the edge.
#[tokio::test]
async fn a_referenced_pattern_set_refuses_its_delete_until_the_rule_is_gone() {
    let emulator = Emulator::new();
    let api = deployed_api(&emulator, "referenced-set-edge").await;
    let resource = stage_arn(&api);

    let expressions: &[&str] = &["^/[^/]+/rx-[0-9]{4}$"];
    let (set_arn, set_id) = create_pattern_set(&emulator, "held-set", expressions).await;
    let (acl_arn, acl_id) = create_web_acl(
        &emulator,
        "holding-acl",
        json!({
            "RegexPatternSetReferenceStatement": {
                "ARN": &set_arn,
                "FieldToMatch": { "UriPath": {} },
                "TextTransformations": [{ "Priority": 0, "Type": "NONE" }],
            }
        }),
    )
    .await;
    emulator
        .waf_ok(
            "AssociateWebACL",
            json!({ "WebACLArn": &acl_arn, "ResourceArn": &resource }),
        )
        .await;

    let matching = Probe {
        path: "/prod/rx-4821",
        headers: &[],
    };
    let control = Probe {
        path: "/prod/ok-4821",
        headers: &[],
    };
    for addressing in ADDRESSINGS {
        settled(
            &emulator,
            &api,
            *addressing,
            "the referencing rule decides",
            &matching,
            BLOCKED_STATUS,
        )
        .await;
    }

    // The set as a client reads it before the refused delete.
    let read = emulator
        .waf_ok(
            "GetRegexPatternSet",
            json!({ "Name": "held-set", "Scope": SCOPE, "Id": &set_id }),
        )
        .await;
    let before = read["RegexPatternSet"]["RegularExpressionList"].clone();
    let lock = read["LockToken"].as_str().expect("LockToken").to_string();
    assert_eq!(
        before.as_array().map(Vec::len),
        Some(expressions.len()),
        "expected the stored list to hold every expression, got {before}"
    );

    let delete = json!({
        "Name": "held-set",
        "Scope": SCOPE,
        "Id": &set_id,
        "LockToken": &lock,
    });
    let (status, refusal) = emulator.waf("DeleteRegexPatternSet", delete.clone()).await;
    assert_eq!(
        status, 400,
        "expected HTTP 400 for a set a live rule references, got {status} with {refusal}"
    );
    assert_eq!(
        error_code(&refusal),
        "WAFAssociatedItemException",
        "expected the associated-item refusal, got {refusal}"
    );
    assert_eq!(
        error_message(&refusal),
        ASSOCIATED_ITEM,
        "expected the sentence WAF words this class with, got {refusal}"
    );

    // The refusal had NO effect: the same expressions, the same lock token, and the same
    // decision at every addressing of the edge.
    let reread = emulator
        .waf_ok(
            "GetRegexPatternSet",
            json!({ "Name": "held-set", "Scope": SCOPE, "Id": &set_id }),
        )
        .await;
    assert_eq!(
        reread["RegexPatternSet"]["RegularExpressionList"], before,
        "a refused delete leaves the list untouched, got {reread}"
    );
    assert_eq!(
        reread["LockToken"].as_str(),
        Some(lock.as_str()),
        "a refused delete does not rotate the lock token, got {reread}"
    );
    for addressing in ADDRESSINGS {
        let still = emulator
            .invoke(&api, *addressing, matching.path, matching.headers)
            .await;
        assert_eq!(
            still.status, BLOCKED_STATUS,
            "case {addressing:?}: the referencing rule still decides after the refused \
             delete, got {} with {:?}",
            still.status, still.body
        );
        let sibling = emulator
            .invoke(&api, *addressing, control.path, control.headers)
            .await;
        assert_eq!(
            sibling.status, 200,
            "case {addressing:?}: the sibling path is still served by the origin, got {} with \
             {:?}",
            sibling.status, sibling.body
        );
    }

    // Take the reference away, and the identical delete succeeds.
    let acl_lock = emulator
        .waf_ok(
            "GetWebACL",
            json!({ "Name": "holding-acl", "Scope": SCOPE, "Id": &acl_id }),
        )
        .await["LockToken"]
        .as_str()
        .expect("LockToken")
        .to_string();
    emulator
        .waf_ok(
            "UpdateWebACL",
            json!({
                "Name": "holding-acl",
                "Scope": SCOPE,
                "Id": &acl_id,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility("webacl"),
                "Rules": [],
                "LockToken": &acl_lock,
            }),
        )
        .await;
    for addressing in ADDRESSINGS {
        let reopened = settled(
            &emulator,
            &api,
            *addressing,
            "the rule is gone",
            &matching,
            200,
        )
        .await;
        assert!(
            reopened.contains(ORIGIN_MARKER),
            "case {addressing:?}: with the rule gone the request reaches the origin, got \
             {reopened:?}"
        );
    }
    emulator.waf_ok("DeleteRegexPatternSet", delete).await;
    let (gone_status, gone) = emulator
        .waf(
            "GetRegexPatternSet",
            json!({ "Name": "held-set", "Scope": SCOPE, "Id": &set_id }),
        )
        .await;
    assert_eq!(
        gone_status, 400,
        "expected HTTP 400 reading a deleted set, got {gone_status} with {gone}"
    );
    assert_eq!(
        error_code(&gone),
        "WAFNonexistentItemException",
        "expected the nonexistent-item refusal, got {gone}"
    );
    assert_eq!(
        error_message(&gone),
        NONEXISTENT_ITEM,
        "expected the sentence WAF words this class with, got {gone}"
    );
}
