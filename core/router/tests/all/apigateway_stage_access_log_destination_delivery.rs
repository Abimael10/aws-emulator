// SPDX-License-Identifier: CC0-1.0

//! What a deployed REST stage DELIVERS into the CloudWatch Logs log group its
//! `accessLogSettings` name, on the assembled emulator.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > Once the ACCOUNT names a CloudWatch role API Gateway can assume
//! > (`UpdateAccount` `/cloudwatchRoleArn`, accepted or refused by the call itself rather
//! > than left pending), every request a stage carrying `accessLogSettings` answers is
//! > written as exactly ONE log event — that stage's `$context` format rendered against
//! > that request — into the log group its `destinationArn` names, in a stream of that
//! > group, so the records are readable back through each of CloudWatch Logs' own readers
//! > of that group and agree: `FilterLogEvents` reports one event per request, every event
//! > names the stream it was written into, `DescribeLogStreams` reports exactly those
//! > streams, and `GetLogEvents` on one of them reports the same messages in the same
//! > order. A destination is a log group of the account's, so a stage pointed at a name
//! > CloudWatch Logs holds no group under delivers nothing and brings no group into
//! > existence, while a sibling stage of the same api pointed at a group that IS there goes
//! > on receiving exactly its own records.
//!
//! Only a cross-service suite can pin this: the role and its trust policy are AWS IAM's, the
//! account setting, the api, its stages and their logging configuration are API Gateway's,
//! and whether a group, a stream or an event exists at all is CloudWatch Logs'. Every
//! delivery assertion reads the outcome back through CloudWatch Logs' own readers, so a
//! record that never reached that store cannot pass, and every call is made exactly as a
//! client makes it — one request, answered.
//!
//! The api's own crate pins the rule over the services it mounts ITSELF, one pair at a time:
//! the rendering and the one-record-per-request delivery
//! (`stage_access_logging`, `stage_access_log_record_rendering`), a destination CloudWatch
//! Logs holds no group under (`stage_access_log_absent_destination`,
//! `stage_access_log_denied_destination`) and the account's role resolved inside the request
//! (`account_role_answered_within_the_request`). What this suite adds, and all it is here
//! for, is the chain as the emulator is ASSEMBLED — `aws_emulator_router::build_router`, the
//! front door a client really dials, every service of it mounted on one engine, the api
//! addressed by its own published `execute-api` host — plus the two readings a client of a
//! destination makes that a per-pair suite does not: the STREAM a record is written into
//! (`DescribeLogStreams`, and each event's own `logStreamName`) and that stream's own page
//! (`GetLogEvents`), which is what makes "the destination was materialised" more than a
//! filter answering a page.
//!
//! What the push role's own inline POLICY allows or denies is NOT under test here, and no
//! assertion below turns on it: a vended-logs write is made as the account's role, and which
//! writes that role's policies would admit is not something this emulator decides. The role
//! is created carrying the allow-plus-deny document the recorded flow installs, so that the
//! setup is the client's, and what the rows then read is the one thing that does decide the
//! outcome — whether CloudWatch Logs holds a group of that name at all.
//!
//! The delivery rows differ along what the rule generalises over: the FORMAT a stage renders
//! its records by (the two-variable line, a JSON object and a keyed line), the published ARN
//! FORM the destination is named by (the group, and the group with everything under it), the
//! SHAPE of the group's name (a path and a flat name), and HOW MANY requests are served
//! under the configuration. The absent-destination rows differ along the ARN form, the shape
//! of the name and the request count as well.

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
const PATH_PART: &str = "ping";
const RESOURCE_PATH: &str = "/ping";
const VERB: &str = "GET";
const STATUS: &str = "200";

/// The two stages the api is deployed to, created the two ways a client creates one: the
/// first by the deployment itself (`CreateDeployment` `stageName`), the second from that
/// deployment afterwards (`CreateStage` `deploymentId`).
const GOOD_STAGE: &str = "good";
const BAD_STAGE: &str = "bad";

/// The role the account writes its logs under, and the inline policy embedded on it.
const ROLE_NAME: &str = "apigw-access-log-push";
const POLICY_NAME: &str = "apigw-access-log-push";

/// A trust policy letting API Gateway assume that role, which is what makes it a role the
/// account may log under (`Account$cloudwatchRoleArn`).
const APIGATEWAY_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"apigateway.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The header API Gateway reports the id of the request it processed in, which a record's
/// `$context.requestId` has to equal.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// What a row's expected record writes where this request's own id belongs: the id is minted
/// per request, so the row carries the placeholder and the assertion substitutes the id the
/// answer reported.
const REQUEST_ID: &str = "{requestId}";

/// The error a reader of a log group CloudWatch Logs does not hold answers with, which is
/// what "the destination was never materialised" looks like to a client reading it.
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

    /// Dispatch one request as it arrives on the wire, answering its status, its headers'
    /// one value this suite reads and its body verbatim.
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

/// The IAM role API Gateway writes the account's logs as, carrying the document the recorded
/// flow embeds on it: the vended-logs actions on every resource, and an explicit `Deny` of
/// `logs:CreateLogGroup` on each destination the client means never to be created. Answers
/// the role's ARN, as IAM minted it.
async fn push_role(emulator: &Emulator, denied: &[String]) -> String {
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
            {
                "Effect": "Deny",
                "Action": "logs:CreateLogGroup",
                "Resource": denied,
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
/// account's logs as.
///
/// Asserted on the ONE call that makes it: the patch is accepted or refused by the request
/// itself, so a client never has to come back and ask again — and what it reports is what
/// `GetAccount` then reads back.
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

/// An api with one `MOCK` method on [`RESOURCE_PATH`] — "for testing without actually
/// invoking the backend" (`IntegrationType`), so what is delivered is the stage's own doing
/// and no backend's — deployed to [`GOOD_STAGE`] and, from that same deployment, to
/// [`BAD_STAGE`]. Answers the api id.
async fn deployed_api(emulator: &Emulator, name: &str) -> String {
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
                "responseTemplates": { "application/json": r#"{"pong":true}"# },
            })),
            201,
        )
        .await;
    let deployment = emulator
        .api_ok(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": GOOD_STAGE })),
            201,
        )
        .await;
    let deployment_id = deployment["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateDeployment: no id in {deployment}"))
        .to_string();
    emulator
        .api_ok(
            "POST",
            &format!("/restapis/{api}/stages"),
            Some(json!({ "stageName": BAD_STAGE, "deploymentId": deployment_id })),
            201,
        )
        .await;
    api
}

/// Point `stage`'s access logging at `destination`, rendering by `format`.
///
/// Accepted whatever CloudWatch Logs holds — the configuration is API Gateway's own record —
/// and the stage reports back the destination and format it was given.
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

/// One request to `stage` that has to be served, answering the id API Gateway processed it
/// under.
async fn served(emulator: &Emulator, api: &str, stage: &str, case: &str) -> String {
    let (status, request_id, text) = emulator.invoke(api, stage).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected stage {stage:?} to serve its traffic whatever its access-log \
         destination, got {status} with {text:?}"
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
/// until they are or [`DELIVERY_DEADLINE`] passes: the suite waits for the RECORDS, never for
/// a duration.
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

/// The names `DescribeLogStreams` reports for `group`, or `None` where CloudWatch Logs holds
/// no group of that name at all — which only the modelled "no such group" refusal answers,
/// so a reader that failed for any other reason fails the suite rather than reading as an
/// absent destination.
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

/// The messages `GetLogEvents` reports for one stream of `group`, read from its head so the
/// page is in the order the events were written.
async fn stream_messages(emulator: &Emulator, group: &str, stream: &str) -> Vec<String> {
    let body = emulator
        .logs_ok(
            "GetLogEvents",
            json!({
                "logGroupName": group,
                "logStreamName": stream,
                "startFromHead": true,
            }),
        )
        .await;
    body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .map(|event| event["message"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The names `DescribeLogGroups` reports under `prefix`, which is the question a client puts
/// to CloudWatch Logs to establish whether a group is there at all.
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

/// `records` in a fixed order, for the comparisons about WHICH records were delivered rather
/// than about the order one reader reports them in: one record per request is the rule, and
/// the order the stage wrote them in is read off the stream they were written into.
fn sorted(records: &[String]) -> Vec<String> {
    let mut ordered = records.to_vec();
    ordered.sort();
    ordered
}

/// One delivery row: the destination the account creates, the form its ARN is named by, the
/// format the stage renders by, the record that format renders for one request, and how many
/// requests are served under the configuration.
struct Row {
    case: &'static str,
    group: &'static str,
    arn_form: GroupArnForm,
    format: &'static str,
    expected: &'static str,
    requests: usize,
}

/// The delivery table: three formats, both published ARN forms, a path-shaped and a flat
/// group name, and one, two and three requests served under the configuration.
const ROWS: &[Row] = &[
    Row {
        case: "the request id and the status, one request",
        group: "/aws/apigateway/access/id-and-status",
        arn_form: GroupArnForm::Group,
        format: "$context.requestId $context.status",
        expected: "{requestId} 200",
        requests: 1,
    },
    Row {
        case: "a json format named by the group and everything under it, two requests",
        group: "/aws/apigateway/access/json",
        arn_form: GroupArnForm::GroupAndEverythingUnderIt,
        format: concat!(
            r#"{"requestId":"$context.requestId","httpMethod":"$context.httpMethod","#,
            r#""resourcePath":"$context.resourcePath","stage":"$context.stage","#,
            r#""status":"$context.status"}"#
        ),
        expected: concat!(
            r#"{"requestId":"{requestId}","httpMethod":"GET","#,
            r#""resourcePath":"/ping","stage":"good","#,
            r#""status":"200"}"#
        ),
        requests: 2,
    },
    Row {
        case: "a keyed line into a destination whose name carries no path, three requests",
        group: "apigw-access-flat",
        arn_form: GroupArnForm::Group,
        format: "stage=$context.stage path=$context.resourcePath status=$context.status \
                 id=$context.requestId",
        expected: "stage=good path=/ping status=200 id={requestId}",
        requests: 3,
    },
];

/// RULE: each request a stage pointed at a log group the account holds answers is one event
/// in that group, rendered by the stage's own format, and every CloudWatch Logs reader of
/// that group agrees about it.
#[tokio::test]
async fn each_request_a_configured_stage_answers_is_one_readable_event_in_its_destination() {
    let emulator = Emulator::new();
    let role_arn = push_role(&emulator, &[]).await;
    account_logs_under(&emulator, &role_arn).await;
    let api = deployed_api(&emulator, "access-log-destination-delivery").await;

    for row in ROWS {
        let case = row.case;
        // The destination is a log group of the ACCOUNT's, created before the stage is
        // pointed at it, exactly as the client creates one.
        emulator
            .logs_ok("CreateLogGroup", json!({ "logGroupName": row.group }))
            .await;
        point_at(
            &emulator,
            &api,
            GOOD_STAGE,
            &group_arn(row.group, row.arn_form),
            row.format,
            case,
        )
        .await;

        let mut expected: Vec<String> = Vec::new();
        for _ in 0..row.requests {
            let request_id = served(&emulator, &api, GOOD_STAGE, case).await;
            expected.push(row.expected.replace(REQUEST_ID, &request_id));
        }

        // One event per request, rendered by THIS stage's format. Compared as a multiset,
        // because the rule is one record per request and not the order a group's own filter
        // reports records written in the same instant in; the ORDER the stage wrote them in
        // is pinned on the stream below, which is where a record's place is decided.
        let events = events_settled(&emulator, case, row.group, row.requests).await;
        let messages: Vec<String> = events.iter().map(|(message, _)| message.clone()).collect();
        assert_eq!(
            sorted(&messages),
            sorted(&expected),
            "case {case:?}: expected one record per request in {:?}, got {messages:?}",
            row.group
        );
        assert!(
            messages.iter().all(|message| !message.contains("$context.")),
            "case {case:?}: expected every reference of the format to be rendered, got \
             {messages:?}"
        );

        // Every event names the stream it was written into, and those are exactly the
        // streams the group reports: a record readable through the group's filter and a
        // stream the group does not hold cannot both be true.
        let named: BTreeSet<String> = events.iter().map(|(_, stream)| stream.clone()).collect();
        assert!(
            !named.iter().any(String::is_empty),
            "case {case:?}: expected every event to name its stream, got {events:?}"
        );
        let reported = streams_of(&emulator, row.group).await.unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected CloudWatch Logs to still hold the destination {:?}",
                row.group
            )
        });
        assert_eq!(
            reported.iter().cloned().collect::<BTreeSet<String>>(),
            named,
            "case {case:?}: expected the streams of {:?} to be the ones its events name, got \
             {reported:?}",
            row.group
        );

        // And the streams read back the same records: a stream carries the records written
        // into it, so the records of all the group's streams together are exactly the ones
        // the group's filter reports — and where the group carries ONE stream, that stream
        // carries them in the order they were written. Stated this way rather than as
        // "every stream carries all of them", so a delivery spread over more than one
        // stream is read as the rule allows rather than failing for the wrong reason.
        let mut in_streams: Vec<String> = Vec::new();
        for stream in &named {
            let in_stream = stream_messages(&emulator, row.group, stream).await;
            if named.len() == 1 {
                assert_eq!(
                    in_stream, expected,
                    "case {case:?}: expected the one stream {stream:?} of {:?} to carry the \
                     records in the order they were written, got {in_stream:?}",
                    row.group
                );
            }
            in_streams.extend(in_stream);
        }
        assert_eq!(
            sorted(&in_streams),
            sorted(&expected),
            "case {case:?}: expected the streams of {:?} together to carry exactly the records \
             the group reports, got {in_streams:?}",
            row.group
        );
    }
}

/// One absent-destination row: the name the stage is pointed at and never created, the ARN
/// form it is named by, and how many requests each stage serves under the configuration.
struct AbsentRow {
    case: &'static str,
    absent_group: &'static str,
    arn_form: GroupArnForm,
    held_group: &'static str,
    requests: usize,
}

/// The absent-destination table: both published ARN forms, a path-shaped and a flat name, and
/// one and two requests served under the configuration.
const ABSENT_ROWS: &[AbsentRow] = &[
    AbsentRow {
        case: "a destination the account never created",
        absent_group: "/aws/apigateway/access/absent",
        arn_form: GroupArnForm::Group,
        held_group: "/aws/apigateway/access/held",
        requests: 1,
    },
    AbsentRow {
        case: "an absent destination named by the group and everything under it",
        absent_group: "/aws/apigateway/access/absent-everything-under",
        arn_form: GroupArnForm::GroupAndEverythingUnderIt,
        held_group: "/aws/apigateway/access/held-everything-under",
        requests: 2,
    },
    AbsentRow {
        case: "an absent destination whose name carries no path",
        absent_group: "apigw-access-absent-flat",
        arn_form: GroupArnForm::Group,
        held_group: "apigw-access-held-flat",
        requests: 2,
    },
];

/// The format the absent-destination rows render by: the rule under test there is about the
/// DESTINATION, so a line naming the request and the stage that answered it is enough — it
/// makes a record say which stage wrote it, so a record delivered into the wrong destination
/// cannot pass.
const SIBLING_FORMAT: &str = "$context.requestId $context.stage $context.status";

/// RULE, the other half: a stage pointed at a name CloudWatch Logs holds no group under
/// delivers nothing and brings no group into existence, while its sibling stage pointed at a
/// group the account created goes on receiving exactly its own records — and both stages
/// serve their traffic.
#[tokio::test]
async fn an_absent_destination_is_never_materialised_while_its_sibling_keeps_delivering() {
    let emulator = Emulator::new();
    let denied: Vec<String> = ABSENT_ROWS
        .iter()
        .map(|row| group_arn(row.absent_group, row.arn_form))
        .collect();
    let role_arn = push_role(&emulator, &denied).await;
    account_logs_under(&emulator, &role_arn).await;
    let api = deployed_api(&emulator, "access-log-absent-destination").await;

    for row in ABSENT_ROWS {
        let case = row.case;
        // Only the sibling's destination is created.
        emulator
            .logs_ok("CreateLogGroup", json!({ "logGroupName": row.held_group }))
            .await;
        point_at(
            &emulator,
            &api,
            BAD_STAGE,
            &group_arn(row.absent_group, row.arn_form),
            SIBLING_FORMAT,
            case,
        )
        .await;
        point_at(
            &emulator,
            &api,
            GOOD_STAGE,
            &group_arn(row.held_group, GroupArnForm::Group),
            SIBLING_FORMAT,
            case,
        )
        .await;

        // The two stages serve interleaved traffic, so a record of either could in principle
        // reach either destination.
        let mut expected: Vec<String> = Vec::new();
        for _ in 0..row.requests {
            served(&emulator, &api, BAD_STAGE, case).await;
            let request_id = served(&emulator, &api, GOOD_STAGE, case).await;
            expected.push(format!("{request_id} {GOOD_STAGE} 200"));
        }

        // The absent destination: no group, so nothing to read, nothing listed and no stream.
        assert_eq!(
            filtered(&emulator, row.absent_group).await,
            None,
            "case {case:?}: expected CloudWatch Logs to hold no group {:?} after {} request(s) \
             to stage {BAD_STAGE:?}",
            row.absent_group,
            row.requests
        );
        assert_eq!(
            streams_of(&emulator, row.absent_group).await,
            None,
            "case {case:?}: expected no stream in a group {:?} that was never created",
            row.absent_group
        );
        let listed = groups_named(&emulator, row.absent_group).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected DescribeLogGroups to list no group named {:?}, got \
             {listed:?}",
            row.absent_group
        );

        // The sibling destination: exactly the records of the requests THAT stage answered.
        let events = events_settled(&emulator, case, row.held_group, row.requests).await;
        let messages: Vec<String> = events.iter().map(|(message, _)| message.clone()).collect();
        assert_eq!(
            sorted(&messages),
            sorted(&expected),
            "case {case:?}: expected one record per request stage {GOOD_STAGE:?} answered in \
             {:?}, got {messages:?}",
            row.held_group
        );
        assert_eq!(
            groups_named(&emulator, row.held_group).await,
            vec![row.held_group.to_string()],
            "case {case:?}: expected DescribeLogGroups to list the destination the account \
             created"
        );
    }
}
