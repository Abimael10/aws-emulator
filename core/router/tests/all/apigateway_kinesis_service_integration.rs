// SPDX-License-Identifier: CC0-1.0

//! An API Gateway stage whose method integrates an AWS SERVICE ACTION of AMAZON KINESIS DATA
//! STREAMS: the whole path a client takes from the stage invoke URL to the record on the
//! shard, with no compute in between, across three services on one engine.
//!
//! RULE: a deployed method whose integration is of type `AWS` is served by MAKING the call its
//! `uri` names against the service that `uri` names — `Integration$uri` gives the form
//! `arn:aws:apigateway:{region}:{subdomain}:path|action/{service_api}` — under the role
//! `Integration$credentials` names, and the method response is the answer that service gave,
//! mapped through the integration response whose `selectionPattern` matches the status it
//! answered with ("For all other HTTP and AWS back ends, the HTTP status code is matched",
//! `IntegrationResponse$selectionPattern`). For a service whose api names its action out of
//! band and carries that action's whole input as a JSON document — the `awsJson1_1` protocol
//! the stream api's model declares — the call is the ACTION the `action` form names with the
//! rendered `Integration$requestTemplates` as its request, so what the action receives is what
//! the developer's template wrote and what the client reads is what that service answered.
//! The role decides whether the call is made at all: one this service cannot assume is an
//! integration that could not be performed (`API_CONFIGURATION_ERROR`, HTTP 500 and
//! `{"message":"Internal server error"}`), and a call the policies in force on the role do not
//! allow is refused BY THE SERVICE, in its own error envelope and under the class that api
//! declares for it, which the integration responses then map like any other backend answer.
//!
//! The second rule is the one the mapping rests on: a backend answer is mapped by the
//! integration response whose `selectionPattern` matches the status it answered with, and
//! otherwise by the one declaring no pattern — the api's DEFAULT, which catches every answer
//! no pattern selects and which, declaring no response template, hands the caller the body the
//! service sent. So an api that declares a mapping for the 4xx class answers its own status
//! for a refusal, and one that declares none answers its default's status carrying the
//! service's own error document; either way the refusal itself is what the caller reads,
//! rather than an integration this stage could not perform.
//!
//! Only a cross-service suite can pin either: the stream and the records on it are the stream
//! service's, the role and its policies are IAM's, and the api, the stage and the integration
//! are API Gateway's, so the rules are observable only where all three are mounted on one
//! engine. Each crate's own tests reach exactly one of them.
//!
//! The rows vary what the rule generalises over: the ACTION the `uri` names (a single-record
//! write, a batch write), HOW the integration names the stream (by name, by the ARN that api
//! documents as the preferred identifier), what the client's payload carries (plain ASCII,
//! text outside ASCII, which the template's base64 conversion encodes as its UTF-8), WHICH
//! role the integration names (one that may make the call, one whose grant names another
//! stream, one this service cannot assume), and whether the api declares an integration
//! response for the class the refusal falls in.

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

/// The stream every row writes to.
const STREAM: &str = "stage-integration-orders";

/// Another stream, which the row whose role is granted on it alone addresses nothing of: it
/// exists only so that grant names a real resource of this account.
const OTHER_STREAM: &str = "stage-integration-elsewhere";

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// The service shape an `awsJson1_1` `X-Amz-Target` names an operation of the stream api
/// under (`smithy_query(query="service_info")` on that model).
const KINESIS_TARGET_PREFIX: &str = "Kinesis_20131202";

/// The media type that protocol carries.
const KINESIS_MEDIA_TYPE: &str = "application/x-amz-json-1.1";

/// A trust policy that lets API Gateway assume the role it is attached to. It carries none of
/// the characters that are significant in an `application/x-www-form-urlencoded` body.
const APIGATEWAY_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"apigateway.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The same policy for ANOTHER service's principal: a role this api cannot assume, which is
/// the only thing that differs between it and the row above.
const LAMBDA_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"lambda.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The actions an integration role here is granted.
const STREAM_ACTIONS: &str = r#""kinesis:PutRecord","kinesis:PutRecords""#;

/// How long to wait between polls of a clock-owed transition, and the generous budget a
/// transition is given before the suite calls it a failure. Nothing here asserts on how long a
/// transition took: the condition is polled for until it holds.
const POLL: Duration = Duration::from_millis(50);

/// See [`POLL`].
const BUDGET: Duration = Duration::from_secs(120);

/// The status API Gateway answers an integration it could not perform with, and the sentence
/// that gateway response's default template carries.
const CONFIGURATION_ERROR_STATUS: u16 = 500;

/// See [`CONFIGURATION_ERROR_STATUS`].
const CONFIGURATION_ERROR_MESSAGE: &str = "Internal server error";

/// The class the stream api names a refusal of a call the caller may not make by: that model
/// declares `AccessDeniedException` a client error (`smithy_query(query="operation",
/// operation="PutRecord")`). The STATUS a caller reads is the one the api's own integration
/// response names; the class is the service's, and travels in the document it sent — as the
/// BARE code, which is what the layer that decides authorization ahead of the service writes.
const ACCESS_DENIED_TYPE: &str = "AccessDeniedException";

/// The member that refusal carries its sentence in: the AWS common error envelope's
/// capitalized `Message`.
const MESSAGE_MEMBER: &str = "Message";

/// The name of the session API Gateway assumes an integration's `credentials` role into,
/// which is the identity such a call is made as and therefore the principal its refusal names.
const INTEGRATION_SESSION_NAME: &str = "BackplaneAssumeRoleSession";

/// The session of the role `role_arn` an integration's call acts as:
/// `arn:aws:sts::{account}:assumed-role/{role name}/{session name}` — a role is never acted as
/// directly.
fn integration_session(role_arn: &str) -> String {
    let name = role_arn.rsplit('/').next().unwrap_or_default();
    format!(
        "arn:aws:sts::{account}:assumed-role/{name}/{INTEGRATION_SESSION_NAME}",
        account = engine::emulator_account_id()
    )
}

/// The `statusCode` of the integration response every row declares with NO
/// `selectionPattern` — the api's DEFAULT, which catches an answer no pattern selects.
const DEFAULT_RESPONSE_STATUS: u16 = 200;

/// The status the api itself maps the 4xx class onto, where a row declares that mapping: a
/// status of its own, told apart from the one the service answered.
const MAPPED_REFUSAL_STATUS: u16 = 403;

/// See [`MAPPED_REFUSAL_STATUS`]: the same status as `IntegrationResponse$statusCode` carries
/// it, and the pattern that response selects the service's refusal by.
const MAPPED_REFUSAL_STATUS_CODE: &str = "403";

/// See [`MAPPED_REFUSAL_STATUS_CODE`].
const REFUSAL_SELECTION_PATTERN: &str = r"4\d{2}";

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

/// One call of the stream api over its own `awsJson1_1` wire.
async fn kinesis(router: &MultiServiceRouter, operation: &str, body: Value) -> Answer {
    dispatch(
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
    .await
}

/// One call of the stream api that has to answer 200.
async fn kinesis_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let answer = kinesis(router, operation, body).await;
    assert_eq!(
        answer.status,
        200,
        "setup {case:?}: expected HTTP 200 from {operation}, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.json()
}

/// One request to the api's own invoke endpoint — the host
/// `{rest-api-id}.execute-api.{region}.amazonaws.com` — carrying no credential at all, as a
/// client of a public method sends it.
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

/// One IAM role: created with `trust` as its trust policy and `document` as its one inline
/// policy, answering its ARN.
async fn role(router: &MultiServiceRouter, name: &str, trust: &str, document: &str) -> String {
    let created = iam_ok(
        router,
        &format!("CreateRole({name})"),
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={name}\
             &AssumeRolePolicyDocument={trust}"
        ),
    )
    .await;
    let arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup CreateRole({name}): no <Arn> in {created}"));
    iam_ok(
        router,
        &format!("PutRolePolicy({name})"),
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={name}\
             &PolicyName=stage-integration&PolicyDocument={document}"
        ),
    )
    .await;
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

/// One stream, created through the stream api's OWN `CreateStream` and waited for until that
/// service first reports it `ACTIVE` — a stream that is not is one no write lands on.
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

/// Every record the stream holds on its one shard, read through the stream api's OWN
/// `GetShardIterator`/`GetRecords` — so what this answers is what that service holds rather
/// than what the stage said.
async fn records_on(router: &MultiServiceRouter, name: &str) -> Vec<Value> {
    let shards = kinesis_ok(
        router,
        "ListShards",
        "ListShards",
        json!({ "StreamName": name }),
    )
    .await;
    let shard = member(
        "ListShards",
        &shards["Shards"][0],
        "ShardId",
    );
    let iterator = kinesis_ok(
        router,
        "GetShardIterator",
        "GetShardIterator",
        json!({
            "StreamName": name,
            "ShardId": shard,
            "ShardIteratorType": "TRIM_HORIZON",
        }),
    )
    .await;
    let read = kinesis_ok(
        router,
        "GetRecords",
        "GetRecords",
        json!({ "ShardIterator": member("GetShardIterator", &iterator, "ShardIterator") }),
    )
    .await;
    read["Records"].as_array().cloned().unwrap_or_default()
}

/// One row: a method of the api whose `AWS` integration names one action of the stream api.
struct Row {
    case: &'static str,
    /// The path part the method is reached at, which is also the `PartitionKey` its template
    /// writes — one per row, so each row's record is told apart on the shard.
    path_part: &'static str,
    /// The operation the `action` form of the `uri` names.
    action: &'static str,
    /// Whether the integration's input document names the stream by its ARN rather than by
    /// its name: "It is recommended that you use the StreamARN input parameter when you
    /// invoke this API" (`PutRecord`), so both forms address the same stream.
    by_arn: bool,
    /// The role the integration names.
    role: RowRole,
    /// Whether the api declares an integration response for the 4xx class, which maps a
    /// refusal onto a status of the api's own choosing — a row that declares none has only
    /// the DEFAULT response, which is the one a successful answer maps through.
    maps_refusals: bool,
    /// The payload the client posts, which the template base64-encodes into the blob member.
    sent: &'static str,
    /// That payload as the blob member carries it: its base64, the encoding RFC 4648 states
    /// and the one a JSON protocol carries a blob in — written out here because this test
    /// target carries no base64 coder of its own, and so that the record read back off the
    /// shard is compared against the payload rather than against itself. Empty for a row that
    /// writes nothing.
    encoded: &'static str,
    /// What the client must be answered.
    expected: Expected,
}

/// Which of the three roles a row's integration names.
enum RowRole {
    /// Assumable, and granted the write on the stream the row addresses.
    Granted,
    /// Assumable, but granted the write on ANOTHER stream only.
    Elsewhere,
    /// Granted the write, but not assumable by this service at all.
    Unassumable,
}

/// What a row's client is answered.
enum Expected {
    /// The single-record write's own answer, mapped: a shard and a sequence number.
    Written,
    /// The batch write's answer, mapped: the shard of its one entry.
    Batched,
    /// The stream api's own refusal of a call the role may not make, answered with the
    /// status named here: the one the api mapped the 4xx class onto where it declares that
    /// mapping, and its DEFAULT response's where it declares none — the service's own error
    /// document either way.
    Denied(u16),
    /// An integration that could not be performed at all.
    Unperformed,
}

const ROWS: &[Row] = &[
    Row {
        case: "a single-record write naming its stream by name",
        path_part: "put",
        action: "PutRecord",
        by_arn: false,
        role: RowRole::Granted,
        maps_refusals: true,
        sent: r#"{"order":"a-1"}"#,
        encoded: "eyJvcmRlciI6ImEtMSJ9",
        expected: Expected::Written,
    },
    Row {
        case: "a single-record write naming its stream by ARN",
        path_part: "put-by-arn",
        action: "PutRecord",
        by_arn: true,
        role: RowRole::Granted,
        maps_refusals: true,
        sent: r#"{"order":"a-2"}"#,
        encoded: "eyJvcmRlciI6ImEtMiJ9",
        expected: Expected::Written,
    },
    Row {
        case: "a payload carrying text outside ASCII, encoded as its UTF-8",
        path_part: "put-unicode",
        action: "PutRecord",
        by_arn: false,
        role: RowRole::Granted,
        maps_refusals: true,
        sent: r#"{"order":"café"}"#,
        encoded: "eyJvcmRlciI6ImNhZsOpIn0=",
        expected: Expected::Written,
    },
    Row {
        case: "a batch write, the other action of the same api",
        path_part: "put-batch",
        action: "PutRecords",
        by_arn: false,
        role: RowRole::Granted,
        maps_refusals: true,
        sent: r#"{"order":"a-3"}"#,
        encoded: "eyJvcmRlciI6ImEtMyJ9",
        expected: Expected::Batched,
    },
    Row {
        case: "a role whose grant names another stream, which the service refuses, mapped onto \
               the status the api declares for the 4xx class",
        path_part: "put-denied",
        action: "PutRecord",
        by_arn: false,
        role: RowRole::Elsewhere,
        maps_refusals: true,
        sent: r#"{"order":"never"}"#,
        encoded: "",
        expected: Expected::Denied(MAPPED_REFUSAL_STATUS),
    },
    Row {
        case: "the same refusal on an api that declares no mapping for it, which the \
               pattern-less default catches with the service's own document",
        path_part: "put-denied-unmapped",
        action: "PutRecord",
        by_arn: false,
        role: RowRole::Elsewhere,
        maps_refusals: false,
        sent: r#"{"order":"never"}"#,
        encoded: "",
        expected: Expected::Denied(DEFAULT_RESPONSE_STATUS),
    },
    Row {
        case: "a role this service cannot assume, so no call is made at all",
        path_part: "put-unassumable",
        action: "PutRecord",
        by_arn: false,
        role: RowRole::Unassumable,
        maps_refusals: true,
        sent: r#"{"order":"never"}"#,
        encoded: "",
        expected: Expected::Unperformed,
    },
];

/// The request template a row's integration renders as the action's input document: the stream
/// the row addresses, the row's own partition key, and the client's payload in the blob member
/// — base64, which is how a JSON protocol carries a blob.
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

/// The response template a row's DEFAULT branch maps the service's answer through, which is
/// what shows the method response is built from what that service sent — and `None` for the
/// row whose api declares no error mapping at all, whose default therefore declares no
/// template either, so the document the service sent reaches the caller as it is.
fn response_template(row: &Row) -> Option<&'static str> {
    if !row.maps_refusals {
        return None;
    }
    match row.action {
        "PutRecords" => Some(r#"{"shard":"$input.path('$.Records[0].ShardId')"}"#),
        _ => Some(
            r#"{"shard":"$input.path('$.ShardId')","seq":"$input.path('$.SequenceNumber')"}"#,
        ),
    }
}

/// Stand one row's method up: the method, its `AWS` integration, and the two integration
/// responses that map what the service answers — the success branch, which renders the
/// document this api publishes, and the 4xx branch, which carries the service's own refusal.
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
    // The DEFAULT integration response — the one declaring no `selectionPattern` — is what a
    // successful answer maps through; the patterned one, where the row declares it, is what
    // maps the 4xx class the service refuses an unauthorized call with.
    let mapped = match row.maps_refusals {
        true => vec![
            ("200", None),
            (
                MAPPED_REFUSAL_STATUS_CODE,
                Some(REFUSAL_SELECTION_PATTERN),
            ),
        ],
        false => vec![("200", None)],
    };
    for (status, pattern) in mapped {
        apigw_ok(
            router,
            row.case,
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/POST/responses/{status}"),
            Some(json!({})),
            201,
        )
        .await;
        let mut response = json!({ "statusCode": status });
        match pattern {
            Some(pattern) => response["selectionPattern"] = json!(pattern),
            // The DEFAULT branch renders the document this api publishes out of what the
            // service answered, where the api declares that template; a patterned branch, and
            // a default that declares no template, pass the service's own envelope through.
            None => {
                if let Some(template) = response_template(row) {
                    response["responseTemplates"] = json!({ "application/json": template });
                }
            }
        }
        apigw_ok(
            router,
            row.case,
            "PUT",
            &format!(
                "/restapis/{api}/resources/{resource}/methods/POST/integration/responses/{status}"
            ),
            Some(response),
            201,
        )
        .await;
    }
}

#[tokio::test]
async fn a_deployed_method_integrating_a_kinesis_action_performs_it() {
    let router = emulator();
    stream(&router, STREAM).await;
    stream(&router, OTHER_STREAM).await;

    // Three roles, each written by IAM's own `CreateRole`/`PutRolePolicy`: one this api can
    // assume that may write to the stream the rows address, one it can assume whose grant
    // names ANOTHER stream, and one it cannot assume at all.
    let granted = role(
        &router,
        "kinesis-integration-granted",
        APIGATEWAY_TRUST,
        &policy(STREAM_ACTIONS, &stream_arn(STREAM)),
    )
    .await;
    let elsewhere = role(
        &router,
        "kinesis-integration-elsewhere",
        APIGATEWAY_TRUST,
        &policy(STREAM_ACTIONS, &stream_arn(OTHER_STREAM)),
    )
    .await;
    let unassumable = role(
        &router,
        "kinesis-integration-unassumable",
        LAMBDA_TRUST,
        &policy(STREAM_ACTIONS, &stream_arn(STREAM)),
    )
    .await;

    let created = apigw_ok(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "kinesis-service-integration" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");
    for row in ROWS {
        let role = match row.role {
            RowRole::Granted => &granted,
            RowRole::Elsewhere => &elsewhere,
            RowRole::Unassumable => &unassumable,
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
        let answered = invoke(&router, &api, &format!("/{}", row.path_part), row.sent).await;
        let body = answered.json();
        match row.expected {
            Expected::Written => {
                assert_eq!(
                    answered.status,
                    200,
                    "case {case:?}: expected HTTP 200 for a write the role may make, got {} \
                     with {:?}",
                    answered.status,
                    answered.text()
                );
                let shard = body["shard"].as_str().unwrap_or_default();
                let sequence = body["seq"].as_str().unwrap_or_default();
                assert!(
                    !shard.is_empty() && !sequence.is_empty(),
                    "case {case:?}: expected the shard and sequence number the service \
                     answered, got {body:?}"
                );
                // `PutRecordOutput$SequenceNumber` is `^(0|([1-9]\d{0,128}))$`
                // (`smithy_query(query="operation", operation="PutRecord")`).
                assert!(
                    sequence.chars().all(|digit| digit.is_ascii_digit())
                        && (sequence == "0" || !sequence.starts_with('0')),
                    "case {case:?}: expected a sequence number of the modelled form, got \
                     {sequence:?}"
                );
            }
            Expected::Batched => {
                assert_eq!(
                    answered.status,
                    200,
                    "case {case:?}: expected HTTP 200 for a batch the role may make, got {} \
                     with {:?}",
                    answered.status,
                    answered.text()
                );
                assert!(
                    !body["shard"].as_str().unwrap_or_default().is_empty(),
                    "case {case:?}: expected the shard the batch's entry was written to, got \
                     {body:?}"
                );
            }
            Expected::Denied(status) => {
                assert_eq!(
                    answered.status, status,
                    "case {case:?}: expected HTTP {status} for a call the role may not make, \
                     got {} with {:?}",
                    answered.status,
                    answered.text()
                );
                assert_eq!(
                    body["__type"].as_str().unwrap_or_default(),
                    ACCESS_DENIED_TYPE,
                    "case {case:?}: expected the bare code that api names an unauthorized \
                     call by, got {body:?}"
                );
                let action = format!("kinesis:{}", row.action);
                assert_eq!(
                    body[MESSAGE_MEMBER].as_str().unwrap_or_default(),
                    format!(
                        "User: {session} is not authorized to perform: {action} on resource: \
                         {resource} because no identity-based policy allows the {action} action",
                        session = integration_session(&elsewhere),
                        resource = stream_arn(STREAM)
                    ),
                    "case {case:?}: expected the sentence IAM words an authorization failure \
                     in, got {body:?}"
                );
            }
            Expected::Unperformed => {
                assert_eq!(
                    answered.status,
                    CONFIGURATION_ERROR_STATUS,
                    "case {case:?}: expected the api's own integration-failure response, got \
                     {} with {:?}",
                    answered.status,
                    answered.text()
                );
                assert_eq!(
                    body["message"].as_str().unwrap_or_default(),
                    CONFIGURATION_ERROR_MESSAGE,
                    "case {case:?}: expected the sentence that gateway response carries, got \
                     {body:?}"
                );
            }
        }
    }

    // The writes reached the STREAM: every row that was answered a success put exactly one
    // record, carrying the payload its client posted, and no row that was refused put any —
    // read back through the stream api's own reader, which is what makes the calls real ones
    // rather than answers shaped like them.
    let held = records_on(&router, STREAM).await;
    let expected: Vec<&Row> = ROWS
        .iter()
        .filter(|row| matches!(row.expected, Expected::Written | Expected::Batched))
        .collect();
    assert_eq!(
        held.len(),
        expected.len(),
        "expected one record per row the stage answered a success, got {held:?}"
    );
    for (record, row) in held.iter().zip(expected) {
        let case = row.case;
        assert_eq!(
            record["PartitionKey"].as_str().unwrap_or_default(),
            row.path_part,
            "case {case:?}: expected the partition key the template wrote, got {record:?}"
        );
        assert_eq!(
            record["Data"].as_str().unwrap_or_default(),
            row.encoded,
            "case {case:?}: expected the record to carry the payload the client posted \
             ({:?}), got {record:?}",
            row.sent
        );
    }

    // The stream the refused row's role was granted on took nothing: the refusal is a refusal,
    // not a write somewhere else.
    let elsewhere_records = records_on(&router, OTHER_STREAM).await;
    assert!(
        elsewhere_records.is_empty(),
        "expected the other stream to hold no record at all, got {elsewhere_records:?}"
    );
}
