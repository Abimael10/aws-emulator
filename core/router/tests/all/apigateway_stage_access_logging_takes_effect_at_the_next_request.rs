// SPDX-License-Identifier: CC0-1.0

//! WHEN a REST stage's access logging starts, stops and moves: the configuration a request is
//! logged under is the one the stage carries at the instant that request is served.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > A stage's `accessLogSettings` is read PER REQUEST. Once the account names a CloudWatch
//! > role API Gateway can assume, a stage that carries no logging configuration serves its
//! > traffic and writes nothing — not even into a destination log group the account already
//! > holds — and the stage update that installs a `destinationArn` and a `format` takes effect
//! > from the NEXT request, with no further deployment of the api: every request served
//! > afterwards is exactly one event in that group, rendered by that format, and no request
//! > served before it is. A later change to the configuration decides the same way: repointed,
//! > the requests that follow land in the new destination and the old one receives no more;
//! > reformatted, the requests that follow are rendered by the new format beside the records
//! > already written; removed, the stage goes on serving and writes nothing more.
//!
//! That is the convergence a client waits on. A client that turns access logging on, serves
//! traffic and then reads the destination has to find its records there, and a client that
//! repoints or turns it off has to see the change take hold without redeploying the api —
//! stage settings are the stage's own record, not part of the deployment snapshot the stage
//! serves from (`CreateStage` carries no `accessLogSettings` member at all, so an update of
//! the stage is the only way a client configures this).
//!
//! Only a cross-service suite can pin it: the role is AWS IAM's, the account setting, the api
//! and its stages are API Gateway's, and whether an event exists at all is CloudWatch Logs' —
//! every service of the ASSEMBLED emulator (`aws_emulator_router::build_router`) on one
//! engine, the api addressed on its own published `execute-api` host, and every record read
//! back through CloudWatch Logs' own reader, so a record that never reached that store cannot
//! pass.
//!
//! What the sibling suites pin is WHERE a record lands and WHAT it says for a stage that was
//! already configured when the traffic arrived: `apigateway_stage_access_log_destination_delivery`
//! (the destination, its streams and its pages), `apigateway_proxy_stage_access_log_time_window`
//! (the instant each record carries) and the api crate's own `stage_access_log_record_rendering`
//! (the rendering). What this suite adds is the TRANSITION: the windows before and after each
//! change to the configuration, and which records each destination holds across them.
//!
//! The rows differ along what the rule generalises over: how many requests are served before
//! and after the change, the format records are rendered by, the published ARN form the
//! destination is named by, the shape of the group's name, and the change applied (installed,
//! repointed, reformatted, removed).

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

/// The stage the api's one deployment is first published to, which no row uses: every row's
/// own stage is created from that deployment, so each row starts from a stage that serves the
/// same methods and carries no logging configuration at all.
const SETUP_STAGE: &str = "staging";

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
    /// this suite reads and its body verbatim.
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

/// The IAM role API Gateway writes the account's logs as, carrying the vended-logs actions a
/// client embeds on it. Answers the role's ARN, as IAM minted it.
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
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "logs:CreateLogStream",
                "logs:DescribeLogGroups",
                "logs:DescribeLogStreams",
                "logs:PutLogEvents",
                "logs:GetLogEvents",
                "logs:FilterLogEvents",
            ],
            "Resource": "*",
        }],
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
/// account's logs as, and read it back.
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
}

/// An api with one `MOCK` method on [`RESOURCE_PATH`] — "for testing without actually invoking
/// the backend" (`IntegrationType`), so what is delivered is the stage's own doing and no
/// backend's — deployed once. Answers `(the api id, the deployment id)`.
async fn deployed_api(emulator: &Emulator, name: &str) -> (String, String) {
    let created = emulator
        .api_ok("POST", "/restapis", Some(json!({ "name": name })), 201)
        .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");
    let resource = emulator
        .api_ok(
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": PATH_PART })),
            201,
        )
        .await;
    let resource = member("CreateResource", &resource, "id");
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
            Some(json!({ "stageName": SETUP_STAGE })),
            201,
        )
        .await;
    let deployment_id = member("CreateDeployment", &deployment, "id");
    (api, deployment_id)
}

/// The member `name` of a JSON answer, as text.
fn member(what: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {what}: no {name:?} in {body}"))
        .to_string()
}

/// One further stage of `api`, published from the deployment the setup made, which starts out
/// carrying NO logging configuration: that is the state the rule's first window is about.
async fn stage_from(emulator: &Emulator, api: &str, deployment: &str, stage: &str, case: &str) {
    let created = emulator
        .api_ok(
            "POST",
            &format!("/restapis/{api}/stages"),
            Some(json!({ "stageName": stage, "deploymentId": deployment })),
            201,
        )
        .await;
    assert_eq!(
        created["accessLogSettings"],
        Value::Null,
        "case {case:?}: expected a new stage to carry no accessLogSettings, got {created}"
    );
}

/// Point `stage`'s access logging at `destination`, rendering by `format`, and read the stored
/// configuration back through `GetStage`.
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
    let read = emulator
        .api_ok("GET", &format!("/restapis/{api}/stages/{stage}"), None, 200)
        .await;
    assert_eq!(
        read["accessLogSettings"]["destinationArn"].as_str(),
        Some(destination),
        "case {case:?}: expected GetStage to read back the destination {destination:?}, got {read}"
    );
    assert_eq!(
        read["accessLogSettings"]["format"].as_str(),
        Some(format),
        "case {case:?}: expected GetStage to read back the configured format, got {read}"
    );
}

/// Take `stage`'s whole logging configuration away, and read back that it is gone.
async fn logging_removed(emulator: &Emulator, api: &str, stage: &str, case: &str) {
    let reported = emulator
        .api_ok(
            "PATCH",
            &format!("/restapis/{api}/stages/{stage}"),
            Some(json!({
                "patchOperations": [{ "op": "remove", "path": "/accessLogSettings" }],
            })),
            200,
        )
        .await;
    assert_eq!(
        reported["accessLogSettings"],
        Value::Null,
        "case {case:?}: expected stage {stage:?} to report no accessLogSettings, got {reported}"
    );
    let read = emulator
        .api_ok("GET", &format!("/restapis/{api}/stages/{stage}"), None, 200)
        .await;
    assert_eq!(
        read["accessLogSettings"],
        Value::Null,
        "case {case:?}: expected GetStage to read back no accessLogSettings, got {read}"
    );
}

/// One request to `stage` that has to be served, answering the id API Gateway processed it
/// under.
async fn served(emulator: &Emulator, api: &str, stage: &str, case: &str) -> String {
    let (status, request_id, text) = emulator.invoke(api, stage).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected stage {stage:?} to serve its traffic whatever its access-log \
         configuration, got {status} with {text:?}"
    );
    request_id
        .unwrap_or_else(|| panic!("case {case:?}: stage {stage:?} named no request id in {text:?}"))
}

/// The messages `FilterLogEvents` reports for `group`, or `None` where CloudWatch Logs holds
/// no group of that name at all.
async fn filtered(emulator: &Emulator, group: &str) -> Option<Vec<String>> {
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
                    .map(|event| event["message"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// The messages of `group` once at least `expected` of them are readable there, looking again
/// until they are or [`DELIVERY_DEADLINE`] passes: the suite waits for the RECORDS, never for
/// a duration.
async fn messages_settled(
    emulator: &Emulator,
    case: &str,
    group: &str,
    expected: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let messages = filtered(emulator, group).await.unwrap_or_default();
        if messages.len() >= expected {
            return messages;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected at least {expected} access-log record(s) in {group:?}, got \
             {messages:?}"
        );
        tokio::time::sleep(DELIVERY_POLL).await;
    }
}

/// The destination log group `name`, created by the ACCOUNT as a client creates one before
/// pointing a stage at it.
async fn create_log_group(emulator: &Emulator, name: &str) {
    emulator
        .logs_ok("CreateLogGroup", json!({ "logGroupName": name }))
        .await;
}

/// `records` in a fixed order, for the comparisons about WHICH records a destination holds
/// rather than about the order one reader reports records written in the same instant in.
fn sorted(records: &[String]) -> Vec<String> {
    let mut ordered = records.to_vec();
    ordered.sort();
    ordered
}

/// One enablement row: the stage, the destination and format its logging is turned on with,
/// the record that format renders for one request, and how many requests are served before
/// the stage update and after it.
struct OnRow {
    case: &'static str,
    stage: &'static str,
    group: &'static str,
    arn_form: GroupArnForm,
    format: &'static str,
    expected: &'static str,
    before: usize,
    after: usize,
}

/// The enablement table: three formats, both published ARN forms, a path-shaped and a flat
/// group name, and differing numbers of requests served in each window.
const ON_ROWS: &[OnRow] = &[
    OnRow {
        case: "a keyed line, one request served before the stage update and one after",
        stage: "live",
        group: "/aws/apigateway/access/turned-on-keyed",
        arn_form: GroupArnForm::Group,
        format: "id=$context.requestId stage=$context.stage status=$context.status",
        expected: "id={requestId} stage=live status=200",
        before: 1,
        after: 1,
    },
    OnRow {
        case: "a json format into a flat group name, two requests before and three after",
        stage: "beta",
        group: "apigw-access-turned-on-flat",
        arn_form: GroupArnForm::GroupAndEverythingUnderIt,
        format: concat!(
            r#"{"requestId":"$context.requestId","httpMethod":"$context.httpMethod","#,
            r#""resourcePath":"$context.resourcePath","stage":"$context.stage","#,
            r#""status":"$context.status"}"#
        ),
        expected: concat!(
            r#"{"requestId":"{requestId}","httpMethod":"GET","#,
            r#""resourcePath":"/ping","stage":"beta","#,
            r#""status":"200"}"#
        ),
        before: 2,
        after: 3,
    },
    OnRow {
        case: "a single-variable line, three requests before and one after",
        stage: "v2",
        group: "/aws/apigateway/access/turned-on-single",
        arn_form: GroupArnForm::Group,
        format: "$context.requestId",
        expected: "{requestId}",
        before: 3,
        after: 1,
    },
];

/// RULE: a stage carrying no logging configuration writes nothing into a destination the
/// account already holds, and the stage update that installs one takes effect from the next
/// request — one record per request served afterwards, and none for a request served before.
#[tokio::test]
async fn turning_access_logging_on_starts_delivery_at_the_next_request() {
    let emulator = Emulator::new();
    let role_arn = push_role(&emulator).await;
    account_logs_under(&emulator, &role_arn).await;
    let (api, deployment) = deployed_api(&emulator, "access-log-enablement").await;

    for row in ON_ROWS {
        let case = row.case;
        stage_from(&emulator, &api, &deployment, row.stage, case).await;
        // The destination exists BEFORE the first request: what the first window shows is a
        // stage that writes nothing, not a group that is missing.
        create_log_group(&emulator, row.group).await;

        let mut before_ids: Vec<String> = Vec::new();
        for _ in 0..row.before {
            before_ids.push(served(&emulator, &api, row.stage, case).await);
        }
        let unconfigured = filtered(&emulator, row.group)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: no group {:?} at all", row.group));
        assert!(
            unconfigured.is_empty(),
            "case {case:?}: expected an unconfigured stage to leave {:?} empty after {} \
             request(s), got {unconfigured:?}",
            row.group,
            row.before
        );

        let destination = group_arn(row.group, row.arn_form);
        point_at(&emulator, &api, row.stage, &destination, row.format, case).await;

        let mut expected: Vec<String> = Vec::new();
        for _ in 0..row.after {
            let request_id = served(&emulator, &api, row.stage, case).await;
            expected.push(row.expected.replace(REQUEST_ID, &request_id));
        }
        let messages = messages_settled(&emulator, case, row.group, row.after).await;
        assert_eq!(
            sorted(&messages),
            sorted(&expected),
            "case {case:?}: expected one record per request served after the stage update in \
             {:?}, got {messages:?}",
            row.group
        );
        for id in &before_ids {
            assert!(
                !messages.iter().any(|message| message.contains(id)),
                "case {case:?}: expected no record of the request {id:?} served before the stage \
                 update, got {messages:?}"
            );
        }
    }
}

/// What a row does to the stage's logging configuration once its first requests were served.
enum Change {
    /// Repointed at another group, rendering by the same format.
    Destination { group: &'static str },
    /// A new format, into the same destination: the records that follow are rendered by it.
    Format {
        format: &'static str,
        expected: &'static str,
    },
    /// Turned off altogether.
    Off,
}

/// One change row: the stage, the destination and format it is first configured with, the
/// change applied afterwards, and how many requests are served in each window.
struct ChangeRow {
    case: &'static str,
    stage: &'static str,
    group: &'static str,
    arn_form: GroupArnForm,
    format: &'static str,
    expected: &'static str,
    before: usize,
    after: usize,
    change: Change,
}

/// The change table: each change a client makes to a configured stage, over differing numbers
/// of requests in each window, both published ARN forms and both shapes of a group name.
const CHANGE_ROWS: &[ChangeRow] = &[
    ChangeRow {
        case: "repointed at a second destination, two requests before and two after",
        stage: "repointed",
        group: "/aws/apigateway/access/first-destination",
        arn_form: GroupArnForm::Group,
        format: "id=$context.requestId stage=$context.stage status=$context.status",
        expected: "id={requestId} stage=repointed status=200",
        before: 2,
        after: 2,
        change: Change::Destination {
            group: "/aws/apigateway/access/second-destination",
        },
    },
    ChangeRow {
        case: "a new format into the same destination, one request before and three after",
        stage: "reformatted",
        group: "apigw-access-reformatted",
        arn_form: GroupArnForm::GroupAndEverythingUnderIt,
        format: "id=$context.requestId status=$context.status",
        expected: "id={requestId} status=200",
        before: 1,
        after: 3,
        change: Change::Format {
            format: concat!(
                r#"{"requestId":"$context.requestId","resourcePath":"$context.resourcePath",""#,
                r#"stage":"$context.stage"}"#
            ),
            expected: concat!(
                r#"{"requestId":"{requestId}","resourcePath":"/ping",""#,
                r#"stage":"reformatted"}"#
            ),
        },
    },
    ChangeRow {
        case: "turned off again, three requests before and two after",
        stage: "turned-off",
        group: "/aws/apigateway/access/turned-off",
        arn_form: GroupArnForm::Group,
        format: "$context.requestId $context.status",
        expected: "{requestId} 200",
        before: 3,
        after: 2,
        change: Change::Off,
    },
];

/// RULE, the other direction: a change to a configured stage decides the requests that FOLLOW
/// it and nothing already written — repointed, the next records land in the new destination and
/// the old one receives no more; reformatted, they are rendered by the new format beside the
/// records already there; removed, the stage goes on serving and writes nothing more.
#[tokio::test]
async fn a_change_to_the_configuration_decides_the_requests_that_follow_it() {
    let emulator = Emulator::new();
    let role_arn = push_role(&emulator).await;
    account_logs_under(&emulator, &role_arn).await;
    let (api, deployment) = deployed_api(&emulator, "access-log-reconfiguration").await;

    for row in CHANGE_ROWS {
        let case = row.case;
        stage_from(&emulator, &api, &deployment, row.stage, case).await;
        create_log_group(&emulator, row.group).await;
        if let Change::Destination { group } = row.change {
            create_log_group(&emulator, group).await;
        }
        let destination = group_arn(row.group, row.arn_form);
        point_at(&emulator, &api, row.stage, &destination, row.format, case).await;

        let mut first: Vec<String> = Vec::new();
        for _ in 0..row.before {
            let request_id = served(&emulator, &api, row.stage, case).await;
            first.push(row.expected.replace(REQUEST_ID, &request_id));
        }
        let written = messages_settled(&emulator, case, row.group, row.before).await;
        assert_eq!(
            sorted(&written),
            sorted(&first),
            "case {case:?}: expected one record per request served under the first \
             configuration in {:?}, got {written:?}",
            row.group
        );

        match row.change {
            Change::Destination { group } => {
                let moved = group_arn(group, GroupArnForm::Group);
                point_at(&emulator, &api, row.stage, &moved, row.format, case).await;
                let mut expected: Vec<String> = Vec::new();
                for _ in 0..row.after {
                    let request_id = served(&emulator, &api, row.stage, case).await;
                    expected.push(row.expected.replace(REQUEST_ID, &request_id));
                }
                let later = messages_settled(&emulator, case, group, row.after).await;
                assert_eq!(
                    sorted(&later),
                    sorted(&expected),
                    "case {case:?}: expected the records of the requests served after the \
                     repointing in {group:?}, got {later:?}"
                );
                let old = filtered(&emulator, row.group).await.unwrap_or_default();
                assert_eq!(
                    sorted(&old),
                    sorted(&first),
                    "case {case:?}: expected the former destination {:?} to receive no further \
                     record, got {old:?}",
                    row.group
                );
            }
            Change::Format { format, expected } => {
                point_at(&emulator, &api, row.stage, &destination, format, case).await;
                let mut all = first.clone();
                for _ in 0..row.after {
                    let request_id = served(&emulator, &api, row.stage, case).await;
                    all.push(expected.replace(REQUEST_ID, &request_id));
                }
                let later =
                    messages_settled(&emulator, case, row.group, row.before + row.after).await;
                assert_eq!(
                    sorted(&later),
                    sorted(&all),
                    "case {case:?}: expected the requests served after the reformatting to be \
                     rendered by the new format beside the records already in {:?}, got \
                     {later:?}",
                    row.group
                );
            }
            Change::Off => {
                logging_removed(&emulator, &api, row.stage, case).await;
                for _ in 0..row.after {
                    served(&emulator, &api, row.stage, case).await;
                }
                let after = filtered(&emulator, row.group).await.unwrap_or_default();
                assert_eq!(
                    sorted(&after),
                    sorted(&first),
                    "case {case:?}: expected no further record in {:?} once the configuration \
                     was removed, got {after:?}",
                    row.group
                );
            }
        }
    }
}
