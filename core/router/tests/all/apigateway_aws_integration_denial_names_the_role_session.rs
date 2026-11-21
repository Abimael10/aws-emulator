// SPDX-License-Identifier: CC0-1.0

//! What a client reads when an API Gateway `AWS` integration's call is one the role it names
//! may not make: the refusal the addressed service's own authorization layer answers, passed
//! through the stage's catch-all integration response.
//!
//! RULE: API Gateway ASSUMES the role `Integration$credentials` names and makes the call as a
//! SESSION of it, so the principal the refusal of such a call names is that session
//! (`arn:{partition}:sts::{account}:assumed-role/{role name}/{session name}`) and never the
//! role's own IAM ARN — a role is not an identity a request speaks for, and the session is what
//! tells an operator which assume was refused. The document the refusal travels in is the one
//! the layer that DECIDES authorization writes, ahead of the service's own code: the AWS common
//! error envelope, whose `__type` carries the BARE error code (`AccessDeniedException`, not a
//! shape id qualified by the service model's namespace, which is what an SDK resolves the error
//! code it classifies by) and whose sentence travels under the capitalized `Message` member.
//! The sentence itself is IAM's: `User: {principal} is not authorized to perform: {action} on
//! resource: {resource} because no identity-based policy allows the {action} action`, closed by
//! the clause that distinguishes IAM's default deny from an explicit one.
//!
//! Only a cross-service suite can pin it: the role, its path and its policies are IAM's, the
//! stream the call addresses and the records on it are the stream service's, and the api, the
//! stage and the integration are API Gateway's, so the rule is observable only where all three
//! are mounted on one engine.
//!
//! The rows vary what the rule generalises over: WHETHER the role may make the call at all (so
//! the refusal is decided from the policies in force rather than answered to every caller),
//! HOW IAM holds the role whose session is named (directly, under a path several levels deep),
//! WHY it may not make the call (no policy at all, a grant naming another stream, a grant
//! naming another action of the same stream), the ACTION the `uri` names (a single-record
//! write, a batch write) and HOW the integration's document addresses the stream (by name, by
//! ARN).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for, and the Region the integration ARNs name.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which is what the setup calls are signed with.
/// The signature itself is never checked: this router runs with request authentication off, as
/// every suite in this crate runs it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The stream every row's integration addresses.
const STREAM: &str = "integration-denial-orders";

/// Another stream, which the row whose role is granted on it alone addresses nothing of: it
/// exists only so that grant names a real resource of this account.
const OTHER_STREAM: &str = "integration-denial-elsewhere";

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// The service shape an `awsJson1_1` `X-Amz-Target` names an operation of the stream api under
/// (`smithy_query(query="service_info")` on that model).
const KINESIS_TARGET_PREFIX: &str = "Kinesis_20131202";

/// The media type that protocol carries.
const KINESIS_MEDIA_TYPE: &str = "application/x-amz-json-1.1";

/// A trust policy that lets API Gateway assume the role it is attached to.
const APIGATEWAY_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"apigateway.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The error code the refusal's `__type` carries: the bare code, which is what the layer
/// deciding authorization ahead of the service writes.
const ACCESS_DENIED_TYPE: &str = "AccessDeniedException";

/// The member the AWS common error envelope carries that refusal's sentence in.
const MESSAGE_MEMBER: &str = "Message";

/// The member one of these apis declares on its own modelled error shape, which this envelope
/// does NOT use — asserted absent so the two cannot both be written.
const MODELLED_MESSAGE_MEMBER: &str = "message";

/// The name of the session API Gateway assumes an integration's `credentials` role into, which
/// is the identity its calls are made as.
const INTEGRATION_SESSION_NAME: &str = "BackplaneAssumeRoleSession";

/// How long to wait between polls of a clock-owed transition, and the generous budget a
/// transition is given before the suite calls it a failure. Nothing here asserts on how long a
/// transition took: the condition is polled for until it holds.
const POLL: Duration = Duration::from_millis(50);

/// See [`POLL`].
const BUDGET: Duration = Duration::from_secs(120);

/// What one call answered.
struct Answer {
    status: u16,
    body: Bytes,
}

impl Answer {
    /// The body as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The body as JSON, `Null` when it is neither JSON nor present.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The three services this rule spans, over ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body,
    }
}

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answer = dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answer.status,
        expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.json()
}

/// One call of the stream api over its own `awsJson1_1` wire that has to answer 200.
async fn kinesis_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let answer = dispatch(
        router,
        "POST",
        &format!("kinesis.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", KINESIS_MEDIA_TYPE),
            ("authorization", &authorization("kinesis")),
            (
                "x-amz-target",
                &format!("{KINESIS_TARGET_PREFIX}.{operation}"),
            ),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup {case:?}: expected HTTP 200 from {operation}, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.json()
}

/// One request to the api's own invoke endpoint, as a client of a public method sends it.
async fn invoke(router: &MultiServiceRouter, api: &str, path: &str, body: &str) -> Answer {
    dispatch(
        router,
        "POST",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}{path}"),
        &[("content-type", "application/json")],
        Bytes::from(body.to_string()),
    )
    .await
}

/// The member `name` of a JSON object, as text.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: no {name:?} in {body}"))
        .to_string()
}

/// The text of the first `<name>…</name>` element of an XML body.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// One identity-based policy granting `actions` on `resource`, written with none of the
/// characters a form body treats specially.
fn policy(actions: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":[{actions}],"Resource":"{resource}"}}]}}"#
    )
}

/// One IAM `awsQuery` call that has to succeed, answering its body.
async fn iam_ok(router: &MultiServiceRouter, case: &str, body: String) -> String {
    let answer = dispatch(
        router,
        "POST",
        "iam.amazonaws.com",
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("iam")),
        ],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup {case:?}: expected HTTP 200 from IAM, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.text()
}

/// One IAM role, created through IAM's OWN `CreateRole` under the path `path` and trusted by
/// API Gateway, carrying `document` as its one inline policy when it carries one at all:
/// answering the ARN IAM holds it under.
async fn role(
    router: &MultiServiceRouter,
    name: &str,
    path: &str,
    document: Option<String>,
) -> String {
    let created = iam_ok(
        router,
        &format!("CreateRole({name})"),
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={name}&Path={path}\
             &AssumeRolePolicyDocument={APIGATEWAY_TRUST}"
        ),
    )
    .await;
    let arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup CreateRole({name}): no <Arn> in {created}"));
    if let Some(document) = document {
        iam_ok(
            router,
            &format!("PutRolePolicy({name})"),
            format!(
                "Action=PutRolePolicy&Version=2010-05-08&RoleName={name}\
                 &PolicyName=integration-denial&PolicyDocument={document}"
            ),
        )
        .await;
    }
    arn
}

/// The ARN of the stream `name` in this account and Region, as that api's `StreamARN` pattern
/// states one (`^arn:aws.*:kinesis:.*:\d{12}:stream/\S+$`).
fn stream_arn(name: &str) -> String {
    format!(
        "arn:aws:kinesis:{REGION}:{account}:stream/{name}",
        account = engine::emulator_account_id()
    )
}

/// The session of the role `role_arn` an integration's call is made as: the role's own name,
/// which is the last segment of the ARN IAM holds it under, under the session name API Gateway
/// opens — a session ARN carries no path, whatever path the role is held under.
fn session_of(role_arn: &str) -> String {
    format!(
        "arn:aws:sts::{account}:assumed-role/{name}/{INTEGRATION_SESSION_NAME}",
        account = engine::emulator_account_id(),
        name = role_arn.rsplit('/').next().unwrap_or_default(),
    )
}

/// One stream, created through the stream api's OWN `CreateStream` and polled until that
/// service first reports it `ACTIVE`.
async fn stream(router: &MultiServiceRouter, name: &str) {
    kinesis_ok(
        router,
        "CreateStream",
        "CreateStream",
        json!({ "StreamName": name, "ShardCount": 1 }),
    )
    .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = kinesis_ok(
            router,
            "DescribeStreamSummary",
            "DescribeStreamSummary",
            json!({ "StreamName": name }),
        )
        .await;
        if described["StreamDescriptionSummary"]["StreamStatus"].as_str() == Some("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the stream {name:?} never reported ACTIVE, last {described}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Which role a row's integration names, each differing in how IAM holds it and in what it is
/// granted.
#[derive(Clone, Copy)]
enum RowRole {
    /// Held directly, granted the write on the stream the rows address.
    Granted,
    /// Held directly, carrying no policy at all.
    Bare,
    /// Held under a path several levels deep, carrying no policy at all.
    Pathed,
    /// Held directly, granted the write on ANOTHER stream only.
    Elsewhere,
    /// Held directly, granted another action of the SAME stream only.
    OtherAction,
}

/// What a row's client must be answered.
enum Expected {
    /// The call was made: the stream api's own answer names the shard it landed on.
    Written,
    /// The call was refused: the service's own authorization refusal, naming the action here.
    Denied(&'static str),
}

/// One row: a method whose `AWS` integration names one action of the stream api under one
/// role.
struct Row {
    case: &'static str,
    /// The path part the method is reached at, which is also the `PartitionKey` its template
    /// writes.
    path_part: &'static str,
    /// The operation the `action` form of the `uri` names.
    action: &'static str,
    /// Whether the integration's document names the stream by its ARN rather than by name.
    by_arn: bool,
    role: RowRole,
    expected: Expected,
}

const ROWS: &[Row] = &[
    Row {
        case: "a role granted the write, which the service performs",
        path_part: "put-allowed",
        action: "PutRecord",
        by_arn: false,
        role: RowRole::Granted,
        expected: Expected::Written,
    },
    Row {
        case: "a role carrying no policy at all",
        path_part: "put-bare",
        action: "PutRecord",
        by_arn: false,
        role: RowRole::Bare,
        expected: Expected::Denied("kinesis:PutRecord"),
    },
    Row {
        case: "a role held under a path, whose session ARN carries none of it",
        path_part: "put-pathed",
        action: "PutRecord",
        by_arn: true,
        role: RowRole::Pathed,
        expected: Expected::Denied("kinesis:PutRecord"),
    },
    Row {
        case: "a role granted the write on another stream",
        path_part: "put-elsewhere",
        action: "PutRecord",
        by_arn: false,
        role: RowRole::Elsewhere,
        expected: Expected::Denied("kinesis:PutRecord"),
    },
    Row {
        case: "a batch write, the other action of the same api, refused for a role granted \
               another action of the same stream",
        path_part: "put-batch-denied",
        action: "PutRecords",
        by_arn: false,
        role: RowRole::OtherAction,
        expected: Expected::Denied("kinesis:PutRecords"),
    },
];

/// The request template a row's integration renders as the action's input document.
fn request_template(row: &Row) -> String {
    let named = match row.by_arn {
        true => format!(r#""StreamARN":"{}""#, stream_arn(STREAM)),
        false => format!(r#""StreamName":"{STREAM}""#),
    };
    let key = row.path_part;
    match row.action {
        "PutRecords" => format!(
            r#"{{{named},"Records":[{{"Data":"$util.base64Encode($input.body)","PartitionKey":"{key}"}}]}}"#
        ),
        _ => format!(
            r#"{{{named},"PartitionKey":"{key}","Data":"$util.base64Encode($input.body)"}}"#
        ),
    }
}

/// Stand one row's method up: the method, its `AWS` integration naming `role`, and the ONE
/// integration response the recorded configuration declares — `selectionPattern` empty, which
/// catches every answer and, declaring no response template, hands the client the document the
/// service itself sent.
async fn given_method(router: &MultiServiceRouter, api: &str, root: &str, row: &Row, role: &str) {
    let created = apigw_ok(
        router,
        row.case,
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": row.path_part })),
        201,
    )
    .await;
    let resource = member(row.case, &created, "id");
    apigw_ok(
        router,
        row.case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/POST"),
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw_ok(
        router,
        row.case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/POST/integration"),
        Some(json!({
            "type": "AWS",
            "httpMethod": "POST",
            "uri": format!("arn:aws:apigateway:{REGION}:kinesis:action/{}", row.action),
            "credentials": role,
            "passthroughBehavior": "NEVER",
            "requestTemplates": { "application/json": request_template(row) },
        })),
        201,
    )
    .await;
    apigw_ok(
        router,
        row.case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/POST/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    apigw_ok(
        router,
        row.case,
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/POST/integration/responses/200"),
        Some(json!({ "statusCode": "200", "selectionPattern": "" })),
        201,
    )
    .await;
}

#[tokio::test]
async fn a_refused_aws_integration_names_the_session_of_the_role_it_assumed() {
    let router = emulator();
    stream(&router, STREAM).await;
    stream(&router, OTHER_STREAM).await;

    // The roles, each written through IAM's own `CreateRole`/`PutRolePolicy`: all four are
    // assumable by this api and differ only in how IAM holds them and in what they may do.
    let granted = role(
        &router,
        "integration-denial-granted",
        "/",
        Some(policy(
            r#""kinesis:PutRecord","kinesis:PutRecords""#,
            &stream_arn(STREAM),
        )),
    )
    .await;
    let bare = role(&router, "integration-denial-bare", "/", None).await;
    let pathed = role(&router, "integration-denial-pathed", "/sim2real/stage/", None).await;
    let elsewhere = role(
        &router,
        "integration-denial-elsewhere",
        "/",
        Some(policy(
            r#""kinesis:PutRecord","kinesis:PutRecords""#,
            &stream_arn(OTHER_STREAM),
        )),
    )
    .await;
    let other_action = role(
        &router,
        "integration-denial-other-action",
        "/",
        Some(policy(r#""kinesis:DescribeStream""#, &stream_arn(STREAM))),
    )
    .await;

    // The role held under a path is held under it: the session the refusal names drops that
    // path, so the two spellings have to be told apart.
    assert!(
        pathed.contains("/sim2real/stage/"),
        "setup: expected IAM to hold the role under the path it was created with, got \
         {pathed:?}"
    );

    let created = apigw_ok(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "aws-integration-denial" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");
    for row in ROWS {
        let role = match row.role {
            RowRole::Granted => &granted,
            RowRole::Bare => &bare,
            RowRole::Pathed => &pathed,
            RowRole::Elsewhere => &elsewhere,
            RowRole::OtherAction => &other_action,
        };
        given_method(&router, &api, &root, row, role).await;
    }
    apigw_ok(
        &router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    for row in ROWS {
        let case = row.case;
        let role = match row.role {
            RowRole::Granted => &granted,
            RowRole::Bare => &bare,
            RowRole::Pathed => &pathed,
            RowRole::Elsewhere => &elsewhere,
            RowRole::OtherAction => &other_action,
        };
        let answered = invoke(&router, &api, &format!("/{}", row.path_part), r#"{"n":1}"#).await;
        let body = answered.json();
        // Every row maps through the one catch-all integration response, so the status is the
        // same and what tells the rows apart is the document the service answered.
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected the status the catch-all integration response names, got \
             {} with {:?}",
            answered.status,
            answered.text()
        );
        match row.expected {
            Expected::Written => {
                assert!(
                    !body["ShardId"].as_str().unwrap_or_default().is_empty(),
                    "case {case:?}: expected the shard the write landed on, got {body:?}"
                );
                assert!(
                    body.get("__type").is_none(),
                    "case {case:?}: a call the role may make is not refused, got {body:?}"
                );
            }
            Expected::Denied(action) => {
                assert_eq!(
                    body["__type"].as_str().unwrap_or_default(),
                    ACCESS_DENIED_TYPE,
                    "case {case:?}: expected the bare error code the refusal carries, got \
                     {body:?}"
                );
                assert!(
                    body.get(MODELLED_MESSAGE_MEMBER).is_none(),
                    "case {case:?}: the sentence travels under {MESSAGE_MEMBER:?} alone, got \
                     {body:?}"
                );
                let session = session_of(role);
                let expected = format!(
                    "User: {session} is not authorized to perform: {action} on resource: \
                     {resource} because no identity-based policy allows the {action} action",
                    resource = stream_arn(STREAM)
                );
                assert_eq!(
                    body[MESSAGE_MEMBER].as_str().unwrap_or_default(),
                    expected,
                    "case {case:?}: expected {expected:?}, got {body:?}"
                );
                // The role's own IAM ARN is what was assumed, not who the call speaks for, so
                // it appears nowhere in the sentence.
                assert!(
                    !answered.text().contains(role.as_str()),
                    "case {case:?}: expected the session rather than the role ARN {role:?}, got \
                     {:?}",
                    answered.text()
                );
            }
        }
    }

    // Nothing a refused row sent reached the stream: the one record on the shard is the
    // allowed row's, read back through the stream api's own reads.
    let shards = kinesis_ok(
        &router,
        "ListShards",
        "ListShards",
        json!({ "StreamName": STREAM }),
    )
    .await;
    let shard = member("ListShards", &shards["Shards"][0], "ShardId");
    let iterator = kinesis_ok(
        &router,
        "GetShardIterator",
        "GetShardIterator",
        json!({
            "StreamName": STREAM,
            "ShardId": shard,
            "ShardIteratorType": "TRIM_HORIZON",
        }),
    )
    .await;
    let read = kinesis_ok(
        &router,
        "GetRecords",
        "GetRecords",
        json!({ "ShardIterator": member("GetShardIterator", &iterator, "ShardIterator") }),
    )
    .await;
    let keys: Vec<String> = read["Records"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|record| record["PartitionKey"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        keys,
        vec!["put-allowed".to_string()],
        "case every refused row: a refused call writes nothing, got {keys:?}"
    );
}
