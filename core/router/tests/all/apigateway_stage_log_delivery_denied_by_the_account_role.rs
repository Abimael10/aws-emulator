// SPDX-License-Identifier: CC0-1.0

//! What a deployed REST stage LOGS when the account's CloudWatch role is not permitted the
//! CloudWatch Logs call the delivery would make, on the assembled emulator.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > Every CloudWatch Logs call API Gateway makes for a stage — creating the execution log
//! > group it publishes, opening the stream and putting the events of either log — is made as
//! > the ACCOUNT's CloudWatch role (`Account$cloudwatchRoleArn`, "the ARN of an Amazon
//! > CloudWatch role for the current Account"), so a policy in force on that role which
//! > explicitly DENIES the call on the log group it addresses stops it: the group is not
//! > created, the records are not delivered, and nothing of that log appears. A sibling stage
//! > or destination the Deny does not cover keeps receiving exactly its own records, and the
//! > requests themselves are answered either way — `200` with an `x-amzn-RequestId` header —
//! > because a log delivery is made after the stage has already answered.
//!
//! Which RESOURCE each call is judged against is IAM's own publication for these actions
//! ("Actions defined by Amazon CloudWatch Logs", IAM Service Authorization Reference):
//! `logs:CreateLogGroup` addresses the log group, while `logs:CreateLogStream` and
//! `logs:PutLogEvents` address the log STREAM inside it
//! (`…:log-group:{name}:log-stream:{stream}`) — so a document scoped to the group together
//! with everything under it covers the writes into it, and one naming the group alone covers
//! its creation.
//!
//! Only a cross-service suite can pin this: the role and the policy embedded on it are AWS
//! IAM's, the account setting, the api, its stages and their logging configuration are API
//! Gateway's, and whether a group, a stream or an event exists at all is CloudWatch Logs'.
//! All three are mounted on ONE engine, as `core/router/src/lib.rs` mounts them, and every
//! assertion reads the outcome back through CloudWatch Logs' own `FilterLogEvents` and
//! `DescribeLogGroups`, so a record or a group that never reached that store cannot pass.
//!
//! Each row waits for the SIBLING's records first — the delivery the Deny does not cover —
//! and only then reads the denied destination, so "nothing was delivered" is read after a
//! delivery pass has demonstrably run rather than before one had a chance to.
//!
//! The rows differ along what the rule generalises over: WHICH of the actions a delivery
//! performs is denied, which published form of the log group's ARN the document is scoped by,
//! the shape of the group's name, and how many requests are served under the configuration.

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

/// The two stages every row deploys the api to: one inside the Deny, one outside it.
const DENIED_STAGE: &str = "denied";
const ALLOWED_STAGE: &str = "allowed";

/// The role the account writes its logs under, and the inline policy embedded on it. One
/// policy NAME, so a row's put replaces the document rather than adding a second one.
const ROLE_NAME: &str = "apigw-log-delivery-boundary";
const POLICY_NAME: &str = "apigw-log-delivery-boundary";

/// A trust policy letting API Gateway assume that role, which is what makes it a role the
/// account may log under (`Account$cloudwatchRoleArn`).
const APIGATEWAY_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"apigateway.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The header API Gateway reports the id of the request it processed in, which every answer
/// below has to carry whatever its stage could log.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The error a reader of a log group CloudWatch Logs does not hold answers with.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The access-log format every row renders its records by: the request's own id and the stage
/// that answered it, so a record delivered into the wrong destination cannot pass.
const FORMAT: &str = "$context.requestId $context.stage $context.status";

/// The execution-log line every traced request leaves, as API Gateway words it: each line of
/// a trace is prefixed with the id of the request it belongs to, and this is the step that
/// names the request itself.
fn execution_line(request_id: &str) -> String {
    format!("({request_id}) Starting execution for request: {request_id}")
}

/// How long a delivery that IS permitted is given to be readable, and how long to wait between
/// looks. Generous, and every wait is for the RECORDS rather than for a duration.
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

    /// One API Gateway control-plane call that has to be answered with `expected`.
    async fn api_ok(&self, method: &str, path: &str, body: Option<Value>, expected: u16) -> Value {
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
        assert_eq!(
            status, expected,
            "setup {method} {path}: expected HTTP {expected}, got {status} with {text:?}"
        );
        serde_json::from_str(&text).unwrap_or(Value::Null)
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

/// Which published form of a log group's ARN a row's policy is scoped by: the group itself,
/// and the group together with everything under it (the form `DescribeLogGroups` reports a
/// group's `arn` in, and the one that covers the streams inside it).
#[derive(Clone, Copy, Debug)]
enum Scope {
    Group,
    GroupAndEverythingUnderIt,
    Both,
}

/// The resource list a policy scoped by `scope` names for the log group `group`.
fn scoped_resources(group: &str, scope: Scope) -> Vec<String> {
    let arn = format!(
        "arn:aws:logs:{REGION}:{account}:log-group:{group}",
        account = engine::emulator_account_id()
    );
    let under = format!("{arn}:*");
    match scope {
        Scope::Group => vec![arn],
        Scope::GroupAndEverythingUnderIt => vec![under],
        Scope::Both => vec![arn, under],
    }
}

/// The ARN of the log group `group`, as an `accessLogSettings` `destinationArn` names one.
fn destination_arn(group: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{account}:log-group:{group}",
        account = engine::emulator_account_id()
    )
}

/// The execution log group API Gateway publishes for the stage `stage` of the api `api`.
fn execution_group(api: &str, stage: &str) -> String {
    format!("API-Gateway-Execution-Logs_{api}/{stage}")
}

/// Create the push role with the trust policy API Gateway needs, and point the ACCOUNT at it.
/// Answers the role ARN IAM minted.
async fn installed_push_role(emulator: &Emulator) -> String {
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
        Some(role_arn.as_str()),
        "setup UpdateAccount: expected the account to report the push role {role_arn:?}, got \
         {reported}"
    );
    role_arn
}

/// The document a client scopes the push role down with: a blanket `Allow` of the vended-logs
/// actions, and — where `denied` names any — an explicit `Deny` of those actions on the log
/// group `group` as `scope` names it.
fn policy_document(denied: Option<(&[&str], &str, Scope)>) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": [
            "logs:CreateLogGroup",
            "logs:CreateLogStream",
            "logs:PutLogEvents",
            "logs:DescribeLogGroups",
            "logs:DescribeLogStreams",
            "logs:GetLogEvents",
            "logs:FilterLogEvents",
        ],
        "Resource": "*",
    })];
    if let Some((actions, group, scope)) = denied {
        statements.push(json!({
            "Effect": "Deny",
            "Action": actions,
            "Resource": scoped_resources(group, scope),
        }));
    }
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// Embed `document` on the push role as its inline policy. One policy NAME, so a row's put
/// replaces the document rather than adding a second one.
async fn embedded(emulator: &Emulator, document: &str) {
    emulator
        .iam_ok(
            "PutRolePolicy",
            format!(
                "Action=PutRolePolicy&Version=2010-05-08&RoleName={role}&PolicyName={policy}\
                 &PolicyDocument={document}",
                role = form_encode(ROLE_NAME),
                policy = form_encode(POLICY_NAME),
                document = form_encode(document)
            ),
        )
        .await;
}

/// Embed on the push role the document that denies `denied` on `group` (see
/// [`policy_document`]).
async fn scoped_down(emulator: &Emulator, denied: &[&str], group: &str, scope: Scope) {
    embedded(emulator, &policy_document(Some((denied, group, scope)))).await;
}

/// Put the same document on the role as its PERMISSIONS BOUNDARY instead — "the ARN of the
/// managed policy that is used to set the permissions boundary"
/// (`AttachedPermissionsBoundary$PermissionsBoundaryArn`), so the `Deny` is stated by the
/// ceiling in force on the role rather than by a policy embedded in it.
async fn bounded_by(emulator: &Emulator, name: &str, denied: &[&str], group: &str, scope: Scope) {
    let created = emulator
        .iam_ok(
            "CreatePolicy",
            format!(
                "Action=CreatePolicy&Version=2010-05-08&PolicyName={name}&PolicyDocument={document}",
                name = form_encode(name),
                document = form_encode(&policy_document(Some((denied, group, scope))))
            ),
        )
        .await;
    let policy_arn = element(&created, "Arn");
    emulator
        .iam_ok(
            "PutRolePermissionsBoundary",
            format!(
                "Action=PutRolePermissionsBoundary&Version=2010-05-08&RoleName={role}\
                 &PermissionsBoundary={boundary}",
                role = form_encode(ROLE_NAME),
                boundary = form_encode(&policy_arn)
            ),
        )
        .await;
}

/// An api with one `MOCK` method on [`RESOURCE_PATH`] — "for testing without actually
/// invoking the backend" (`IntegrationType`), so what is logged is the stage's own doing and
/// no backend's — deployed to [`DENIED_STAGE`] and, from that same deployment, to
/// [`ALLOWED_STAGE`]. Answers the api id.
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
        .api_ok("PUT", &format!("{method}/responses/{STATUS}"), Some(json!({})), 201)
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
            Some(json!({ "stageName": DENIED_STAGE })),
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
            Some(json!({ "stageName": ALLOWED_STAGE, "deploymentId": deployment_id })),
            201,
        )
        .await;
    api
}

/// Turn `stage`'s EXECUTION logging on at `INFO` — "specifies the logging level for this
/// method" (`MethodSetting$loggingLevel`) — for every method of the stage.
async fn logs_executions(emulator: &Emulator, api: &str, stage: &str) {
    emulator
        .api_ok(
            "PATCH",
            &format!("/restapis/{api}/stages/{stage}"),
            Some(json!({
                "patchOperations": [
                    { "op": "replace", "path": "/*/*/logging/loglevel", "value": "INFO" },
                ],
            })),
            200,
        )
        .await;
}

/// Point `stage`'s access logging at the log group `group`, rendering by [`FORMAT`].
async fn access_logs_to(emulator: &Emulator, api: &str, stage: &str, group: &str) {
    emulator
        .api_ok(
            "PATCH",
            &format!("/restapis/{api}/stages/{stage}"),
            Some(json!({
                "patchOperations": [
                    {
                        "op": "replace",
                        "path": "/accessLogSettings/destinationArn",
                        "value": destination_arn(group),
                    },
                    { "op": "replace", "path": "/accessLogSettings/format", "value": FORMAT },
                ],
            })),
            200,
        )
        .await;
}

/// Create the log group `group` through CloudWatch Logs' own `CreateLogGroup`, as the account
/// creates a destination it means a stage to write into.
async fn create_group(emulator: &Emulator, group: &str) {
    emulator
        .logs_ok("CreateLogGroup", json!({ "logGroupName": group }))
        .await;
}

/// One request to `stage` that has to be served, answering the id API Gateway processed it
/// under: a log delivery happens after the answer, so what the role may log never changes it.
async fn served(emulator: &Emulator, api: &str, stage: &str, case: &str) -> String {
    let (status, request_id, text) = emulator.invoke(api, stage).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected stage {stage:?} to serve its traffic whatever its account \
         role may log, got {status} with {text:?}"
    );
    request_id.unwrap_or_else(|| {
        panic!("case {case:?}: stage {stage:?} named no request id in {text:?}")
    })
}

/// The messages `FilterLogEvents` reports for `group`, or `None` where CloudWatch Logs holds
/// no group of that name at all.
async fn messages(emulator: &Emulator, group: &str) -> Option<Vec<String>> {
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
async fn settled(emulator: &Emulator, case: &str, group: &str, expected: usize) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let found = messages(emulator, group).await.unwrap_or_default();
        if found.len() >= expected {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected at least {expected} event(s) in {group:?}, got {found:?}"
        );
        tokio::time::sleep(DELIVERY_POLL).await;
    }
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
                .map(|group| {
                    group["logGroupName"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `records` in a fixed order, for a comparison about WHICH records were delivered rather
/// than about the order one reader reports them in: one record per request is the rule here.
fn sorted(records: &[String]) -> Vec<String> {
    let mut ordered = records.to_vec();
    ordered.sort();
    ordered
}

/// One row: which actions the role is denied, which form of the group's ARN the Deny is
/// scoped by, and how many requests each stage serves under the configuration.
struct Row {
    case: &'static str,
    denied: &'static [&'static str],
    scope: Scope,
    requests: usize,
}

/// The rows for a DELIVERY the role may not make: each of the two actions a delivery performs
/// and both of them together, scoped by the published ARN form that covers the streams inside
/// the group, with one and with several requests served under the configuration.
const DELIVERY_ROWS: &[Row] = &[
    Row {
        case: "both writes a delivery makes, denied on the group and everything under it",
        denied: &["logs:CreateLogStream", "logs:PutLogEvents"],
        scope: Scope::Both,
        requests: 1,
    },
    Row {
        case: "the event write alone, denied on everything under the group",
        denied: &["logs:PutLogEvents"],
        scope: Scope::GroupAndEverythingUnderIt,
        requests: 2,
    },
    Row {
        case: "the stream open alone, denied on everything under the group",
        denied: &["logs:CreateLogStream"],
        scope: Scope::GroupAndEverythingUnderIt,
        requests: 1,
    },
    Row {
        case: "every logs action, denied on the group and everything under it",
        denied: &["logs:*"],
        scope: Scope::Both,
        requests: 2,
    },
];

/// RULE: a stage whose EXECUTION log group the account role may not write into leaves that
/// group empty, while a sibling stage of the same api whose group the Deny does not cover
/// receives the trace of each request it answered — and both stages serve their traffic.
#[tokio::test]
async fn a_denied_execution_log_group_stays_empty_while_its_sibling_stage_is_traced() {
    let emulator = Emulator::new();
    installed_push_role(&emulator).await;
    for (index, row) in DELIVERY_ROWS.iter().enumerate() {
        let case = row.case;
        let api = deployed_api(&emulator, &format!("execution-{index}")).await;
        let denied_group = execution_group(&api, DENIED_STAGE);
        let allowed_group = execution_group(&api, ALLOWED_STAGE);
        scoped_down(&emulator, row.denied, &denied_group, row.scope).await;
        logs_executions(&emulator, &api, DENIED_STAGE).await;
        logs_executions(&emulator, &api, ALLOWED_STAGE).await;

        // Interleaved traffic, so a trace of either stage could in principle reach either
        // group.
        let mut allowed_ids: Vec<String> = Vec::new();
        for _ in 0..row.requests {
            served(&emulator, &api, DENIED_STAGE, case).await;
            allowed_ids.push(served(&emulator, &api, ALLOWED_STAGE, case).await);
        }

        // The sibling stage's own group: the trace of each request it answered, naming that
        // request's id. Waited for first, so the denied group below is read after a delivery
        // pass has run.
        let traced = settled(&emulator, case, &allowed_group, row.requests).await;
        for request_id in &allowed_ids {
            let expected = execution_line(request_id);
            assert!(
                traced.iter().any(|line| line == &expected),
                "case {case:?}: expected {expected:?} in the sibling stage's group \
                 {allowed_group:?}, got {traced:?}"
            );
        }

        // The denied group: the account role may not write into it, so it holds nothing at
        // all — whatever the stage answered.
        let found = messages(&emulator, &denied_group).await.unwrap_or_default();
        assert!(
            found.is_empty(),
            "case {case:?}: expected no execution log event in {denied_group:?} after {} \
             request(s), got {found:?}",
            row.requests
        );
    }
}

/// RULE, the access log: a stage pointed at an access-log destination the account role may not
/// write into delivers nothing there, while a sibling stage pointed at a destination the Deny
/// does not cover receives exactly one record per request it answered.
#[tokio::test]
async fn a_denied_access_log_destination_takes_nothing_while_its_sibling_keeps_delivering() {
    let emulator = Emulator::new();
    installed_push_role(&emulator).await;
    // The destination group NAME shape varies with the row: a path-shaped name and a flat one
    // name the same kind of destination.
    let names: &[(&str, &str)] = &[
        ("/apigw/boundary/denied-0", "/apigw/boundary/allowed-0"),
        ("apigw-boundary-denied-1", "apigw-boundary-allowed-1"),
        ("/apigw/boundary/denied-2", "apigw-boundary-allowed-2"),
        ("apigw-boundary-denied-3", "/apigw/boundary/allowed-3"),
    ];
    for (index, (row, (denied_group, allowed_group))) in
        DELIVERY_ROWS.iter().zip(names).enumerate()
    {
        let case = row.case;
        let api = deployed_api(&emulator, &format!("access-{index}")).await;
        // Both destinations are groups the ACCOUNT creates: what the row varies is what the
        // role may do with them, not whether they are there.
        create_group(&emulator, denied_group).await;
        create_group(&emulator, allowed_group).await;
        scoped_down(&emulator, row.denied, denied_group, row.scope).await;
        access_logs_to(&emulator, &api, DENIED_STAGE, denied_group).await;
        access_logs_to(&emulator, &api, ALLOWED_STAGE, allowed_group).await;

        let mut allowed_records: Vec<String> = Vec::new();
        for _ in 0..row.requests {
            served(&emulator, &api, DENIED_STAGE, case).await;
            let request_id = served(&emulator, &api, ALLOWED_STAGE, case).await;
            allowed_records.push(format!("{request_id} {ALLOWED_STAGE} 200"));
        }

        let delivered = settled(&emulator, case, allowed_group, row.requests).await;
        assert_eq!(
            sorted(&delivered),
            sorted(&allowed_records),
            "case {case:?}: expected one record per request the sibling stage answered in \
             {allowed_group:?}, got {delivered:?}"
        );

        let found = messages(&emulator, denied_group).await.unwrap_or_default();
        assert!(
            found.is_empty(),
            "case {case:?}: expected no access-log record in {denied_group:?} after {} \
             request(s), got {found:?}",
            row.requests
        );
    }
}

/// One creation row: the form of the group's ARN the `Deny` of the create is scoped by.
struct CreationRow {
    case: &'static str,
    denied: &'static [&'static str],
    scope: Scope,
}

/// The rows for a CREATE the role may not make: the action named on its own and through a
/// wildcard, scoped by the group's own ARN, by everything under it, and by both.
const CREATION_ROWS: &[CreationRow] = &[
    CreationRow {
        case: "the create denied on the group and everything under it",
        denied: &["logs:CreateLogGroup"],
        scope: Scope::Both,
    },
    CreationRow {
        case: "the create denied on the group's own ARN",
        denied: &["logs:CreateLogGroup"],
        scope: Scope::Group,
    },
    CreationRow {
        case: "every logs action denied on the group's own ARN",
        denied: &["logs:*"],
        scope: Scope::Group,
    },
];

/// RULE, the group API Gateway publishes ITSELF: a stage whose execution logging is turned on
/// while the account role may not create that stage's log group publishes no group at all —
/// `DescribeLogGroups` goes on listing none of that name, and the stage keeps serving — while
/// the sibling stage's group is published by the very call that turns its logging on.
#[tokio::test]
async fn a_role_denied_the_create_publishes_no_execution_log_group() {
    let emulator = Emulator::new();
    installed_push_role(&emulator).await;
    for (index, row) in CREATION_ROWS.iter().enumerate() {
        let case = row.case;
        let api = deployed_api(&emulator, &format!("creation-{index}")).await;
        let denied_group = execution_group(&api, DENIED_STAGE);
        let allowed_group = execution_group(&api, ALLOWED_STAGE);
        scoped_down(&emulator, row.denied, &denied_group, row.scope).await;
        logs_executions(&emulator, &api, DENIED_STAGE).await;
        logs_executions(&emulator, &api, ALLOWED_STAGE).await;

        // The sibling stage's group is published by the configuration itself, before any
        // request: that is the group API Gateway creates under its own name.
        let listed = groups_named(&emulator, &allowed_group).await;
        assert_eq!(
            listed,
            vec![allowed_group.clone()],
            "case {case:?}: expected the sibling stage's group {allowed_group:?} to be \
             published by the call that turned its logging on, got {listed:?}"
        );
        let listed = groups_named(&emulator, &denied_group).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected no group named {denied_group:?} to be created, got \
             {listed:?}"
        );

        // Serving traffic creates nothing either: the create is the same call, made as the
        // same role.
        served(&emulator, &api, DENIED_STAGE, case).await;
        let request_id = served(&emulator, &api, ALLOWED_STAGE, case).await;
        let traced = settled(&emulator, case, &allowed_group, 1).await;
        assert!(
            traced.iter().any(|line| line == &execution_line(&request_id)),
            "case {case:?}: expected the sibling stage's trace of {request_id:?} in \
             {allowed_group:?}, got {traced:?}"
        );
        let listed = groups_named(&emulator, &denied_group).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected the group {denied_group:?} to stay absent after the \
             stage served a request, got {listed:?}"
        );
        assert_eq!(
            messages(&emulator, &denied_group).await,
            None,
            "case {case:?}: expected {denied_group:?} to hold no log group to read at all"
        );
    }
}

/// RULE, the ceiling the role is capped by: a `Deny` stated by the role's PERMISSIONS BOUNDARY
/// is an explicit deny like any other, so the delivery it names is withheld even though the
/// role's own embedded policy allows every write — while the sibling stage outside it is
/// traced.
#[tokio::test]
async fn a_deny_stated_by_the_roles_permissions_boundary_withholds_the_delivery() {
    let case = "the deny stated by the role's permissions boundary";
    let emulator = Emulator::new();
    installed_push_role(&emulator).await;
    let api = deployed_api(&emulator, "boundary").await;
    let denied_group = execution_group(&api, DENIED_STAGE);
    let allowed_group = execution_group(&api, ALLOWED_STAGE);
    // The role's OWN document allows every write everywhere; only the ceiling says no.
    embedded(&emulator, &policy_document(None)).await;
    bounded_by(
        &emulator,
        "apigw-log-delivery-ceiling",
        &["logs:CreateLogStream", "logs:PutLogEvents"],
        &denied_group,
        Scope::Both,
    )
    .await;
    logs_executions(&emulator, &api, DENIED_STAGE).await;
    logs_executions(&emulator, &api, ALLOWED_STAGE).await;

    served(&emulator, &api, DENIED_STAGE, case).await;
    let request_id = served(&emulator, &api, ALLOWED_STAGE, case).await;
    let traced = settled(&emulator, case, &allowed_group, 1).await;
    assert!(
        traced.iter().any(|line| line == &execution_line(&request_id)),
        "case {case:?}: expected the sibling stage's trace of {request_id:?} in \
         {allowed_group:?}, got {traced:?}"
    );
    let found = messages(&emulator, &denied_group).await.unwrap_or_default();
    assert!(
        found.is_empty(),
        "case {case:?}: expected no execution log event in {denied_group:?}, got {found:?}"
    );
}
