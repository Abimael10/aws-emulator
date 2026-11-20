// SPDX-License-Identifier: CC0-1.0

//! Which of a REST api's stages an access-log record reaches, when every stage of that api
//! carries `accessLogSettings` of its OWN, on the assembled emulator.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > A stage's access-log record is written into the log group that STAGE's own
//! > `accessLogSettings` `destinationArn` names and nowhere else, so of the sibling stages of
//! > one api — each pointed at a group the account holds — only the stage a request was served
//! > on receives a record for it, rendered with that stage's own name where the format reads
//! > `$context.stage`; a stage that served no request has nothing created in its destination
//! > at all, neither an event nor a log stream, and that destination goes on holding nothing
//! > until that stage itself serves a request.
//!
//! The second half is an INDEPENDENT observable, and it is what this suite adds: a client of an
//! untrafficked stage's destination reads it through `DescribeLogStreams` and has to be told the
//! group holds no stream — a group whose stream was opened and left empty would read as an
//! api that delivers to every sibling and loses the records, and a cross-stage leak would be
//! indistinguishable from the correct empty result.
//!
//! Only a cross-service suite can pin this: the push role and its trust policy are AWS IAM's,
//! the account setting, the api, its stages and their logging configuration are API Gateway's,
//! and whether a group holds an event or a stream at all is CloudWatch Logs'. Every assertion
//! reads the outcome back through CloudWatch Logs' own readers of the destination, so a record
//! that never reached that store cannot pass, and every call is made exactly as a client makes
//! it — one request, answered.
//!
//! What the sibling suite `apigateway_stage_access_log_destination_delivery` pins is the
//! delivery itself (one readable event per request, every reader of the group agreeing) and the
//! destination CloudWatch Logs holds NO group under; the api's own crate pins the rendering and
//! the per-stage destination over the services it mounts itself (`stage_access_logging`). What
//! this one pins, and all it is here for, is the isolation between destinations that ALL exist,
//! on the assembled front door: which group each record lands in when every sibling stage names
//! one, and the second, independent observable neither of those reads — that a stage which
//! served no request has no log STREAM opened in its destination at all.
//!
//! The rows differ along what the rule generalises over: HOW MANY stages the api carries (two
//! and three), WHICH of them serves traffic (the first, the last, the middle, and two of three
//! at once), how many requests each serves, the FORMAT the records are rendered by (the JSON
//! object of the recorded case and a keyed line), the published ARN FORM each destination is
//! named by, and the SHAPE of each group's name (a path and a flat name).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every call is signed for, every invoke host carries and every ARN names.
const REGION: &str = "us-east-1";

/// The api's one resource, the verb its method takes and the status that method answers.
const PATH_PART: &str = "echo";
const RESOURCE_PATH: &str = "/echo";
const VERB: &str = "GET";
const STATUS: &str = "200";

/// The role the account writes its logs under, and the inline policy embedded on it.
const ROLE_NAME: &str = "apigw-per-stage-access-log-push";
const POLICY_NAME: &str = "apigw-per-stage-access-log-push";

/// A trust policy letting API Gateway assume that role, which is what makes it a role the
/// account may log under (`Account$cloudwatchRoleArn`).
const APIGATEWAY_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"apigateway.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The header API Gateway reports the id of the request it processed in, which a record's
/// `$context.requestId` has to equal.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// What an expected record writes where this request's own id and the stage that served it
/// belong: the id is minted per request, so the row carries placeholders and the assertion
/// substitutes the id the answer reported and the stage the request was sent to.
const REQUEST_ID: &str = "{requestId}";
const STAGE: &str = "{stage}";

/// The error a reader of a log group CloudWatch Logs does not hold answers with.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// How long a delivered record is given to be readable in its destination, and how long to
/// wait between looks. Generous, and the suite waits for the RECORDS rather than for any
/// duration: it fails only where they never arrive at all.
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

    /// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
    /// routes the request. The signature itself is never checked here.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host, Signature=0000"
        )
    }

    /// Dispatch one request as it arrives on the wire, answering its status, the one header
    /// value this suite reads and its body verbatim.
    async fn dispatch(
        &self,
        method: &str,
        host: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Bytes,
    ) -> (u16, Option<String>, String) {
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
        let request_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let text = String::from_utf8_lossy(&response.into_body()).into_owned();
        (status, request_id, text)
    }

    /// One API Gateway control-plane call, answering its status and parsed body.
    async fn api(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value, String) {
        let payload = body.map(|value| value.to_string()).unwrap_or_default();
        let (status, _, text) = self
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
        let parsed = serde_json::from_str(&text).unwrap_or(Value::Null);
        (status, parsed, text)
    }

    /// One API Gateway control-plane call that has to be answered with `expected`.
    async fn api_ok(&self, method: &str, path: &str, body: Option<Value>, expected: u16) -> Value {
        let (status, parsed, text) = self.api(method, path, body).await;
        assert_eq!(
            status, expected,
            "setup {method} {path}: expected HTTP {expected}, got {status} with {text:?}"
        );
        parsed
    }

    /// One CloudWatch Logs call (`awsJson1_1`), answering its status and parsed body.
    async fn logs(&self, operation: &str, body: Value) -> (u16, Value) {
        let (status, _, text) = self
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
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// One CloudWatch Logs call that has to succeed.
    async fn logs_ok(&self, operation: &str, body: Value) -> Value {
        let (status, parsed) = self.logs(operation, body).await;
        assert_eq!(
            status, 200,
            "setup {operation}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// One IAM `awsQuery` call that has to succeed, answering its XML body.
    async fn iam_ok(&self, what: &str, body: String) -> String {
        let (status, _, text) = self
            .dispatch(
                "POST",
                "iam.amazonaws.com",
                "/",
                &[
                    ("content-type", "application/x-www-form-urlencoded"),
                    ("authorization", &Self::authorization("iam")),
                ],
                Bytes::from(body),
            )
            .await;
        assert_eq!(
            status, 200,
            "setup {what}: expected HTTP 200 from IAM, got {status} with {text:?}"
        );
        text
    }

    /// One unsigned request to a deployed stage's own invoke URL, as a client of a method
    /// whose `authorizationType` is `NONE` makes it.
    async fn invoke(&self, api: &str, stage: &str) -> (u16, Option<String>, String) {
        self.dispatch(
            VERB,
            &format!("{api}.execute-api.{REGION}.amazonaws.com"),
            &format!("/{stage}{RESOURCE_PATH}"),
            &[("content-type", "application/json")],
            Bytes::new(),
        )
        .await
    }
}

/// The two published forms the ARN of ONE log group is written in, both of which name that
/// group as an access-log destination: the group itself, and the group together with
/// everything under it (the form `DescribeLogGroups` reports a group's `arn` in).
#[derive(Clone, Copy, Debug)]
enum GroupArnForm {
    Group,
    GroupAndEverythingUnderIt,
}

/// The ARN of the log group `name` in `form`, as an `accessLogSettings` `destinationArn`
/// names one.
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

/// The IAM role API Gateway writes the account's logs as, carrying the vended-logs document
/// the recorded flow embeds on it. Answers the role's ARN, as IAM minted it.
///
/// Every destination in this suite is a group the CLIENT creates, so the document allows the
/// vended-logs actions outright: what this suite reads is which group a record lands in, not
/// what a policy admits.
async fn push_role(emulator: &Emulator) -> String {
    let created = emulator
        .iam_ok(
            "CreateRole",
            format!(
                "Action=CreateRole&Version=2010-05-08&RoleName={role}\
                 &AssumeRolePolicyDocument={trust}",
                role = form_encode(ROLE_NAME),
                trust = form_encode(APIGATEWAY_TRUST)
            ),
        )
        .await;
    let role_arn = element(&created, "Arn");
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": [
                    "logs:CreateLogGroup",
                    "logs:CreateLogStream",
                    "logs:DescribeLogGroups",
                    "logs:DescribeLogStreams",
                    "logs:PutLogEvents",
                    "logs:GetLogEvents",
                    "logs:FilterLogEvents",
                ],
                "Resource": "*",
            },
        ],
    })
    .to_string();
    emulator
        .iam_ok(
            "PutRolePolicy",
            format!(
                "Action=PutRolePolicy&Version=2010-05-08&RoleName={role}&PolicyName={policy}\
                 &PolicyDocument={document}",
                role = form_encode(ROLE_NAME),
                policy = form_encode(POLICY_NAME),
                document = form_encode(&document)
            ),
        )
        .await;
    role_arn
}

/// Point the ACCOUNT at `role_arn`, which is what gives API Gateway something to write the
/// account's logs as, and read it back through `GetAccount`.
async fn account_logs_under(emulator: &Emulator, role_arn: &str) {
    let reported = emulator
        .api_ok(
            "PATCH",
            "/account",
            Some(json!({
                "patchOperations": [
                    { "op": "replace", "path": "/cloudwatchRoleArn", "value": role_arn }
                ]
            })),
            200,
        )
        .await;
    assert_eq!(
        reported["cloudwatchRoleArn"].as_str(),
        Some(role_arn),
        "setup UpdateAccount: expected the account to report the push role {role_arn:?}, got \
         {reported}"
    );
    let read = emulator.api_ok("GET", "/account", None, 200).await;
    assert_eq!(
        read["cloudwatchRoleArn"].as_str(),
        Some(role_arn),
        "setup GetAccount: expected the stored push role {role_arn:?}, got {read}"
    );
}

/// An api with one `MOCK` method on [`RESOURCE_PATH`] — "for testing without actually invoking
/// the backend" (`IntegrationType`), so what is delivered is the stage's own doing and no
/// backend's — deployed to `stages[0]` by the deployment itself and, from that same deployment,
/// to each stage after it. Answers the api id.
async fn deployed_api(emulator: &Emulator, name: &str, stages: &[&str]) -> String {
    let created = emulator
        .api_ok("POST", "/restapis", Some(json!({ "name": name })), 201)
        .await;
    let api = created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateRestApi: no id in {created}"))
        .to_string();
    let root = created["rootResourceId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateRestApi: no rootResourceId in {created}"))
        .to_string();
    let resource = emulator
        .api_ok(
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": PATH_PART })),
            201,
        )
        .await;
    let resource = resource["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateResource: no id in {resource}"))
        .to_string();
    let method = format!("/restapis/{api}/resources/{resource}/methods/{VERB}");
    emulator
        .api_ok(
            "PUT",
            &method,
            Some(json!({ "authorizationType": "NONE" })),
            201,
        )
        .await;
    emulator
        .api_ok(
            "PUT",
            &format!("{method}/integration"),
            Some(json!({
                "type": "MOCK",
                "requestTemplates": { "application/json": r#"{"statusCode": 200}"# },
            })),
            201,
        )
        .await;
    emulator
        .api_ok(
            "PUT",
            &format!("{method}/responses/{STATUS}"),
            Some(json!({})),
            201,
        )
        .await;
    emulator
        .api_ok(
            "PUT",
            &format!("{method}/integration/responses/{STATUS}"),
            Some(json!({
                "responseTemplates": { "application/json": r#"{"ok":true}"# },
            })),
            201,
        )
        .await;
    let (first, rest) = stages
        .split_first()
        .unwrap_or_else(|| panic!("setup {name}: an api is deployed to at least one stage"));
    let deployment = emulator
        .api_ok(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": first })),
            201,
        )
        .await;
    let deployment_id = deployment["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateDeployment: no id in {deployment}"))
        .to_string();
    for stage in rest {
        emulator
            .api_ok(
                "POST",
                &format!("/restapis/{api}/stages"),
                Some(json!({ "stageName": stage, "deploymentId": deployment_id })),
                201,
            )
            .await;
    }
    api
}

/// Point `stage`'s access logging at `destination`, rendering by `format`, and assert the stage
/// reports back what it was given.
async fn point_at(
    emulator: &Emulator,
    api: &str,
    stage: &str,
    destination: &str,
    format: &str,
    case: &str,
) {
    let reported = emulator
        .api_ok(
            "PATCH",
            &format!("/restapis/{api}/stages/{stage}"),
            Some(json!({
                "patchOperations": [
                    {
                        "op": "replace",
                        "path": "/accessLogSettings/destinationArn",
                        "value": destination,
                    },
                    { "op": "replace", "path": "/accessLogSettings/format", "value": format },
                ],
            })),
            200,
        )
        .await;
    assert_eq!(
        reported["accessLogSettings"]["destinationArn"].as_str(),
        Some(destination),
        "case {case:?}: expected stage {stage:?} to report the destination {destination:?}, got \
         {reported}"
    );
    assert_eq!(
        reported["accessLogSettings"]["format"].as_str(),
        Some(format),
        "case {case:?}: expected stage {stage:?} to report the configured format, got {reported}"
    );
}

/// One request to `stage`'s own invoke URL that has to be served, answering the id API Gateway
/// processed it under.
async fn served(emulator: &Emulator, api: &str, stage: &str, case: &str) -> String {
    let (status, request_id, text) = emulator.invoke(api, stage).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the deployed stage {stage:?} to serve its traffic, got {status} \
         with {text:?}"
    );
    request_id
        .unwrap_or_else(|| panic!("case {case:?}: stage {stage:?} named no request id in {text:?}"))
}

/// The `(message, logStreamName)` of every event `FilterLogEvents` reports for `group`, or
/// `None` where CloudWatch Logs holds no group of that name at all.
async fn filtered(emulator: &Emulator, group: &str) -> Option<Vec<(String, String)>> {
    let (status, body) = emulator
        .logs("FilterLogEvents", json!({ "logGroupName": group }))
        .await;
    if status != 200 {
        assert!(
            body["__type"]
                .as_str()
                .unwrap_or_default()
                .contains(RESOURCE_NOT_FOUND),
            "FilterLogEvents({group:?}): expected either a page of events or \
             {RESOURCE_NOT_FOUND}, got {status} with {body}"
        );
        return None;
    }
    Some(
        body["events"]
            .as_array()
            .map(|events| {
                events
                    .iter()
                    .map(|event| {
                        (
                            event["message"].as_str().unwrap_or_default().to_string(),
                            event["logStreamName"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// The events of `group` once at least `expected` of them are readable there, looking again
/// until they are or [`DELIVERY_DEADLINE`] passes: the suite waits for the RECORDS, never for a
/// duration.
async fn events_settled(
    emulator: &Emulator,
    case: &str,
    group: &str,
    expected: usize,
) -> Vec<(String, String)> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let events = filtered(emulator, group).await.unwrap_or_default();
        if events.len() >= expected {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected at least {expected} access-log event(s) in {group:?}, got \
             {events:?}"
        );
        tokio::time::sleep(DELIVERY_POLL).await;
    }
}

/// The names `DescribeLogStreams` reports for `group`, or `None` where CloudWatch Logs holds no
/// group of that name at all — which only the modelled "no such group" refusal answers, so a
/// reader that failed for any other reason fails the suite rather than reading as an absent
/// destination.
async fn streams_of(emulator: &Emulator, group: &str) -> Option<Vec<String>> {
    let (status, body) = emulator
        .logs("DescribeLogStreams", json!({ "logGroupName": group }))
        .await;
    if status != 200 {
        assert!(
            body["__type"]
                .as_str()
                .unwrap_or_default()
                .contains(RESOURCE_NOT_FOUND),
            "DescribeLogStreams({group:?}): expected either the group's streams or \
             {RESOURCE_NOT_FOUND}, got {status} with {body}"
        );
        return None;
    }
    Some(
        body["logStreams"]
            .as_array()
            .map(|streams| {
                streams
                    .iter()
                    .map(|stream| {
                        stream["logStreamName"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// The names `DescribeLogGroups` reports under `prefix`, which is the question a client puts to
/// CloudWatch Logs to establish whether a group is there at all.
async fn groups_named(emulator: &Emulator, prefix: &str) -> Vec<String> {
    let body = emulator
        .logs_ok("DescribeLogGroups", json!({ "logGroupNamePrefix": prefix }))
        .await;
    body["logGroups"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .map(|group| group["logGroupName"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// `records` in a fixed order, for the comparisons about WHICH records a destination holds
/// rather than about the order one reader reports them in.
fn sorted(records: &[String]) -> Vec<String> {
    let mut ordered = records.to_vec();
    ordered.sort();
    ordered
}

/// The format of the recorded case: a JSON object naming the request and the stage that served
/// it, so a record says which stage wrote it and a record in the wrong destination cannot pass.
const JSON_FORMAT: &str = r#"{"requestId":"$context.requestId","stage":"$context.stage"}"#;
const JSON_RECORD: &str = r#"{"requestId":"{requestId}","stage":"{stage}"}"#;

/// A keyed line, the other shape a client writes a format in.
const KEYED_FORMAT: &str =
    "stage=$context.stage id=$context.requestId status=$context.status path=$context.resourcePath";
const KEYED_RECORD: &str = "stage={stage} id={requestId} status=200 path=/echo";

/// One stage of a row: the stage name, the destination it alone is pointed at, the published
/// form that destination's ARN is named by, and how many requests are sent to its invoke URL
/// (zero for a stage that serves none).
struct StageRow {
    stage: &'static str,
    group: &'static str,
    arn_form: GroupArnForm,
    requests: usize,
}

/// One isolation row: the api its stages are deployed on, the format every stage of it renders
/// by, the record that format renders for one request, and the stages themselves.
struct Row {
    case: &'static str,
    api: &'static str,
    format: &'static str,
    record: &'static str,
    stages: &'static [StageRow],
}

/// The isolation table: two- and three-stage apis, traffic on the first stage, the last, the
/// middle and on two of three at once, one to three requests each, both formats, both published
/// ARN forms, and path-shaped and flat group names.
const ROWS: &[Row] = &[
    Row {
        case: "two stages, the first serves every request",
        api: "per-stage-access-log-first",
        format: JSON_FORMAT,
        record: JSON_RECORD,
        stages: &[
            StageRow {
                stage: "stagea",
                group: "/aws/apigateway/access/stagea",
                arn_form: GroupArnForm::Group,
                requests: 3,
            },
            StageRow {
                stage: "stageb",
                group: "/aws/apigateway/access/stageb",
                arn_form: GroupArnForm::Group,
                requests: 0,
            },
        ],
    },
    Row {
        case: "two stages, only the last serves, flat group names and a keyed line",
        api: "per-stage-access-log-last",
        format: KEYED_FORMAT,
        record: KEYED_RECORD,
        stages: &[
            StageRow {
                stage: "dev",
                group: "apigw-access-dev-flat",
                arn_form: GroupArnForm::GroupAndEverythingUnderIt,
                requests: 0,
            },
            StageRow {
                stage: "prod",
                group: "apigw-access-prod-flat",
                arn_form: GroupArnForm::Group,
                requests: 1,
            },
        ],
    },
    Row {
        case: "three stages, only the middle one serves",
        api: "per-stage-access-log-middle",
        format: JSON_FORMAT,
        record: JSON_RECORD,
        stages: &[
            StageRow {
                stage: "alpha",
                group: "/aws/apigateway/access/alpha",
                arn_form: GroupArnForm::Group,
                requests: 0,
            },
            StageRow {
                stage: "beta",
                group: "apigw-access-beta",
                arn_form: GroupArnForm::GroupAndEverythingUnderIt,
                requests: 2,
            },
            StageRow {
                stage: "gamma",
                group: "/aws/apigateway/access/gamma",
                arn_form: GroupArnForm::Group,
                requests: 0,
            },
        ],
    },
    Row {
        case: "three stages, two deliver side by side and the third serves nothing",
        api: "per-stage-access-log-two-of-three",
        format: KEYED_FORMAT,
        record: KEYED_RECORD,
        stages: &[
            StageRow {
                stage: "blue",
                group: "/aws/apigateway/access/blue",
                arn_form: GroupArnForm::Group,
                requests: 1,
            },
            StageRow {
                stage: "green",
                group: "apigw-access-green",
                arn_form: GroupArnForm::Group,
                requests: 2,
            },
            StageRow {
                stage: "silver",
                group: "/aws/apigateway/access/silver",
                arn_form: GroupArnForm::GroupAndEverythingUnderIt,
                requests: 0,
            },
        ],
    },
];

/// The records every stage of `row` has to end up holding, after the traffic the row describes
/// has been served INTERLEAVED across its stages — so a record of any one stage could in
/// principle have reached any destination. Keyed by the stage's place in the row.
async fn traffic_of(emulator: &Emulator, api: &str, row: &Row) -> Vec<Vec<String>> {
    let rounds = row
        .stages
        .iter()
        .map(|stage| stage.requests)
        .max()
        .unwrap_or_default();
    let mut expected: Vec<Vec<String>> = row.stages.iter().map(|_| Vec::new()).collect();
    for round in 0..rounds {
        for (at, stage) in row.stages.iter().enumerate() {
            if round < stage.requests {
                let request_id = served(emulator, api, stage.stage, row.case).await;
                expected[at].push(
                    row.record
                        .replace(REQUEST_ID, &request_id)
                        .replace(STAGE, stage.stage),
                );
            }
        }
    }
    expected
}

/// Assert `group` holds exactly `expected`, each event naming the stream it was written into
/// and those streams being the ones the group itself reports.
async fn holds_exactly(
    emulator: &Emulator,
    case: &str,
    stage: &str,
    group: &str,
    expected: &[String],
) {
    let events = events_settled(emulator, case, group, expected.len()).await;
    let messages: Vec<String> = events.iter().map(|(message, _)| message.clone()).collect();
    assert_eq!(
        sorted(&messages),
        sorted(expected),
        "case {case:?}: expected {group:?} to hold exactly the records of the requests stage \
         {stage:?} answered, got {messages:?}"
    );
    assert!(
        messages.iter().all(|message| !message.contains("$context.")),
        "case {case:?}: expected every reference of stage {stage:?}'s format to be rendered, got \
         {messages:?}"
    );
    let named: BTreeSet<String> = events.iter().map(|(_, stream)| stream.clone()).collect();
    assert!(
        !named.is_empty() && !named.iter().any(String::is_empty),
        "case {case:?}: expected every event in {group:?} to name its stream, got {events:?}"
    );
    let reported = streams_of(emulator, group)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected CloudWatch Logs to hold {group:?}"));
    assert_eq!(
        reported.iter().cloned().collect::<BTreeSet<String>>(),
        named,
        "case {case:?}: expected the streams of {group:?} to be the ones its events name, got \
         {reported:?}"
    );
}

/// Assert `group` — a destination the account created and pointed `stage` at — holds nothing at
/// all: no event, no stream, and the group still there.
async fn holds_nothing(emulator: &Emulator, case: &str, stage: &str, group: &str) {
    let events = filtered(emulator, group).await.unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected CloudWatch Logs to still hold {group:?}, the destination of \
             the untrafficked stage {stage:?}"
        )
    });
    assert!(
        events.is_empty(),
        "case {case:?}: expected no access-log event in {group:?}, the destination of stage \
         {stage:?}, which served no request, got {events:?}"
    );
    let streams = streams_of(emulator, group).await.unwrap_or_else(|| {
        panic!("case {case:?}: expected CloudWatch Logs to still hold {group:?}")
    });
    assert!(
        streams.is_empty(),
        "case {case:?}: expected {group:?} to have no log stream opened in it while stage \
         {stage:?} served no request, got {streams:?}"
    );
    assert_eq!(
        groups_named(emulator, group).await,
        vec![group.to_string()],
        "case {case:?}: expected the destination {group:?} the account created to still be listed"
    );
}

/// Assert no record naming ANOTHER stage of `row` is in `stage`'s own destination: WHICH records
/// a destination holds is pinned by request id elsewhere, and this says the same about the stage
/// name each record renders, which is the value a client reads a record's own stage off.
async fn no_record_of_another_stage(emulator: &Emulator, row: &Row, stage: &StageRow) {
    let case = row.case;
    let messages: Vec<String> = filtered(emulator, stage.group)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(message, _)| message)
        .collect();
    for other in row.stages {
        if other.stage == stage.stage {
            continue;
        }
        assert!(
            !messages.iter().any(|message| message.contains(other.stage)),
            "case {case:?}: expected no record of stage {:?} in {:?}, the destination of stage \
             {:?}, got {messages:?}",
            other.stage,
            stage.group,
            stage.stage
        );
    }
}

/// RULE: every record a stage writes lands in the destination that stage alone names, so each
/// trafficked stage's group holds exactly its own records (its own name rendered for
/// `$context.stage`) and a sibling stage that served nothing has neither an event nor a stream
/// in its own group.
#[tokio::test]
async fn each_stage_delivers_only_its_own_records_into_its_own_destination() {
    let emulator = Emulator::new();
    let role_arn = push_role(&emulator).await;
    account_logs_under(&emulator, &role_arn).await;

    for row in ROWS {
        let case = row.case;
        let stages: Vec<&str> = row.stages.iter().map(|stage| stage.stage).collect();
        let api = deployed_api(&emulator, row.api, &stages).await;
        // Every destination is a log group of the ACCOUNT's, created before any stage is
        // pointed at it — including the ones no traffic is ever sent to, which is what makes
        // "nothing was created there" a reading of delivery rather than of an absent group.
        for stage in row.stages {
            emulator
                .logs_ok("CreateLogGroup", json!({ "logGroupName": stage.group }))
                .await;
            point_at(
                &emulator,
                &api,
                stage.stage,
                &group_arn(stage.group, stage.arn_form),
                row.format,
                case,
            )
            .await;
        }

        let expected = traffic_of(&emulator, &api, row).await;

        // The stages that SERVED are read first, each settling on the condition that its own
        // records are readable: by the time an untrafficked destination is read, every delivery
        // this row's traffic makes has been made, so an empty destination is a statement about
        // isolation rather than about one read arriving early.
        for (at, stage) in row.stages.iter().enumerate() {
            if stage.requests > 0 {
                holds_exactly(&emulator, case, stage.stage, stage.group, &expected[at]).await;
                no_record_of_another_stage(&emulator, row, stage).await;
            }
        }
        for stage in row.stages {
            if stage.requests == 0 {
                holds_nothing(&emulator, case, stage.stage, stage.group).await;
                no_record_of_another_stage(&emulator, row, stage).await;
            }
        }
    }
}

/// The stages of the api the second rule below is read on, each pointed at its own destination.
const SETTLED_STAGES: &[(&str, &str)] = &[
    ("stagea", "/aws/apigateway/access/opened-by-stagea"),
    ("stageb", "/aws/apigateway/access/opened-by-stageb"),
];

/// RULE, the other direction: a destination stays empty only while ITS stage serves nothing —
/// the first request that stage answers opens its stream and writes its one record there, and
/// the sibling destination is left exactly as it was.
#[tokio::test]
async fn an_untrafficked_destination_opens_its_stream_when_its_own_stage_serves() {
    let case = "a destination opened by its own stage's first request";
    let emulator = Emulator::new();
    let role_arn = push_role(&emulator).await;
    account_logs_under(&emulator, &role_arn).await;
    let stages: Vec<&str> = SETTLED_STAGES.iter().map(|(stage, _)| *stage).collect();
    let api = deployed_api(&emulator, "per-stage-access-log-opened", &stages).await;
    for (stage, group) in SETTLED_STAGES {
        emulator
            .logs_ok("CreateLogGroup", json!({ "logGroupName": group }))
            .await;
        point_at(
            &emulator,
            &api,
            stage,
            &group_arn(group, GroupArnForm::Group),
            JSON_FORMAT,
            case,
        )
        .await;
    }
    let (first_stage, first_group) = SETTLED_STAGES[0];
    let (second_stage, second_group) = SETTLED_STAGES[1];

    // Only the first stage serves: its own destination holds its records, and the second
    // stage's holds nothing and has no stream.
    let mut first_records: Vec<String> = Vec::new();
    for _ in 0..2 {
        let request_id = served(&emulator, &api, first_stage, case).await;
        first_records.push(
            JSON_RECORD
                .replace(REQUEST_ID, &request_id)
                .replace(STAGE, first_stage),
        );
    }
    holds_exactly(&emulator, case, first_stage, first_group, &first_records).await;
    holds_nothing(&emulator, case, second_stage, second_group).await;

    // Then the second stage serves one request of its own, and that is the only record its
    // destination comes to hold — while the first stage's destination is untouched.
    let request_id = served(&emulator, &api, second_stage, case).await;
    let second_records = vec![
        JSON_RECORD
            .replace(REQUEST_ID, &request_id)
            .replace(STAGE, second_stage),
    ];
    holds_exactly(&emulator, case, second_stage, second_group, &second_records).await;
    holds_exactly(&emulator, case, first_stage, first_group, &first_records).await;
}
