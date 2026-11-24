// SPDX-License-Identifier: CC0-1.0

//! An AWS AppSync GraphQL API FIELD as the target of a rule: what a delivery to it does, end
//! to end over the wire through the real `MultiServiceRouter`, with AppSync, Amazon DynamoDB,
//! IAM, the token service and Amazon SQS mounted beside EventBridge on one engine — so what
//! the delivery did is read where a client reads it: in the table the API's resolver writes
//! to, and on the dead-letter queue when the delivery failed.
//!
//! The RULE asserted here: a rule target whose `Arn` names one field of one AppSync GraphQL
//! API (`apis/{apiId}/types/{typeName}/fields/{fieldName}`, the form the `appsync:GraphQL`
//! action is granted on) is delivered to by EXECUTING the target's own
//! `AppSyncParameters.GraphQLOperation` on that API with what the target receives as that
//! operation's variables. Such a target is one EventBridge invokes UNDER the target's
//! `RoleArn`, so a role `events.amazonaws.com` cannot assume, and one whose policies do not
//! allow `appsync:GraphQL` on that very field, deliver nothing: the event goes to the
//! target's dead-letter queue carrying the condition. An execution the API ANSWERED is the
//! delivery, whatever the answer says about the fields it resolved — a GraphQL answer is a
//! 200 whether or not every field resolved, so an answer carrying resolver errors is an
//! event the target ACCEPTED and is dead-lettered nowhere.
//!
//! The rows span what the rule generalises over: the TYPE and FIELD the ARN names (a mutation
//! that writes, a query that writes nothing, a mutation whose response template raises), WHY
//! a role bars the delivery (no trust, another action, nothing at all, the action on ANOTHER
//! field of the same API, the action on this one), what the target receives (an
//! `InputTransformer`'s rendering, the target's own constant `Input`, and an input that is
//! not a variables object), whether the ARN names an API this account holds, and whether the
//! target states an operation at all.
//!
//! The resolvers here are defined by VTL MAPPING TEMPLATES, which is the documented way to
//! write a resolver on an `AMAZON_DYNAMODB` data source: the request template renders the
//! `2018-05-29` `PutItem`/`GetItem` the data source is called with, and the response template
//! renders the value the field takes.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    ComAmazonawsEventbridgeAwseventsState, InvocationOutcome, FAILED_TO_ASSUME_ROLE, INVALID_JSON,
    INVALID_PARAMETER, NO_PERMISSIONS, NO_RESOURCE, SERVICE_PRINCIPAL,
};
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call of this suite is signed for, and the one the API's own ARN carries.
const REGION: &str = "eu-west-2";

/// The source every event of this suite is published with; each case matches on its own
/// `detail-type`, so one case's rule never sees another case's event.
const SOURCE: &str = "com.demo.appsync";

/// The table the API's resolvers resolve against, and the attribute its items are keyed by.
const TABLE: &str = "appsync-target-items";
const KEY_ATTRIBUTE: &str = "id";

/// The name of the `AMAZON_DYNAMODB` data source every resolver of the API resolves against.
const DATA_SOURCE: &str = "items";

/// The service principal a data source's service role is assumed by.
const APPSYNC_PRINCIPAL: &str = "appsync.amazonaws.com";

/// The schema the API is created with: one mutation that writes an item, one query that reads
/// one back, and one mutation whose resolver RAISES, under an explicit `schema` definition
/// binding both roots.
const SCHEMA: &str = "schema { query: Query, mutation: Mutation }\n\
                      \n\
                      type Item {\n  id: ID!\n  kind: String\n}\n\
                      \n\
                      type Query {\n  getItem(id: ID!): Item\n}\n\
                      \n\
                      type Mutation {\n  \
                      putItem(id: ID!, kind: String!): Item\n  \
                      refuseItem(id: ID!): Item\n}\n";

/// The sentence the raising resolver's response template calls `$util.error` with, and the
/// `errorType` it classifies it as.
const REFUSAL_MESSAGE: &str = "the response template refused this result";
const REFUSAL_TYPE: &str = "ResolverRefused";

/// How long a case waits for a clock-owed transition (the table becoming `ACTIVE`, the schema
/// creation being performed) and how often it looks again. The deadline is far past the spans
/// those transitions take, so a loaded machine still reaches it and no assertion depends on
/// how fast the host runs.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const POLL_SPACING: Duration = Duration::from_millis(100);

/// The emulator: EventBridge and every service this delivery spans, on ONE engine — the shape
/// `core/router/src/lib.rs` mounts them in.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // What a case pins is what each service ANSWERS, never signature verification, which the
    // emulator's front door owns.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    Emulator { router, engine }
}

/// One `awsJson` RPC call against the service whose signing name is `signing_name` — the
/// credential scope is what routes it.
async fn rpc(
    emulator: &Emulator,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One EventBridge call.
async fn events(emulator: &Emulator, operation: &str, body: &Value) -> (u16, Value) {
    rpc(emulator, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call.
async fn sqs(emulator: &Emulator, operation: &str, body: &Value) -> (u16, Value) {
    rpc(emulator, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// One Amazon DynamoDB call, asserting it was served.
async fn dynamodb(emulator: &Emulator, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = rpc(
        emulator,
        "dynamodb",
        "DynamoDB_20120810",
        "1.0",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DynamoDB {operation} answered {status} {answered}"
    );
    answered
}

/// One IAM call (`awsQuery`, a form-encoded body), answering the XML it was served.
async fn iam(emulator: &Emulator, case: &str, body: String) -> String {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("iam.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/iam/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.clone()))
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: iam {body} answered {status} {text}"
    );
    text
}

/// The text of the first `<name>…</name>` element of an XML body.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// One AppSync control-plane call (restJson1).
async fn appsync(
    emulator: &Emulator,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(bytes::Bytes::new, |value| {
        bytes::Bytes::from(value.to_string())
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://appsync.{REGION}.amazonaws.com{path}"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/appsync/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(payload)
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One AppSync control-plane call that must succeed.
async fn appsync_ok(
    emulator: &Emulator,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let (status, answered) = appsync(emulator, method, path, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: appsync {method} {path} answered {status} {answered}"
    );
    answered
}

/// Create the table the resolvers resolve against, and wait until DynamoDB first reports it
/// `ACTIVE` — the wait is on the CONDITION that service reports, not on a span.
async fn given_table(emulator: &Emulator, case: &str) -> String {
    let created = dynamodb(
        emulator,
        case,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": KEY_ATTRIBUTE, "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let arn = created["TableDescription"]["TableArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !arn.is_empty(),
        "case {case:?}: CreateTable reports the ARN the table is held under, got {created}"
    );
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = dynamodb(
            emulator,
            case,
            "DescribeTable",
            &json!({"TableName": TABLE}),
        )
        .await;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return arn;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the table becomes ACTIVE, last read {described}"
        );
        tokio::time::sleep(POLL_SPACING).await;
    }
}

/// A role called `name` trusting `principal`, carrying an inline policy allowing `action` on
/// `resource` when the row names one; answered with the ARN IAM persisted.
///
/// Both documents are written free of the characters an `application/x-www-form-urlencoded`
/// body treats specially, so they need no encoding.
async fn given_role(
    emulator: &Emulator,
    case: &str,
    name: &str,
    principal: &str,
    allowed: Option<(&str, &str)>,
) -> String {
    let trust = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    );
    let created = iam(
        emulator,
        case,
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={name}\
             &AssumeRolePolicyDocument={trust}"
        ),
    )
    .await;
    let arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole({name}) reports no <Arn>: {created}"));
    if let Some((action, resource)) = allowed {
        let document = format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
             \"Action\":\"{action}\",\"Resource\":\"{resource}\"}}]}}"
        );
        iam(
            emulator,
            case,
            format!(
                "Action=PutRolePolicy&Version=2010-05-08&RoleName={name}\
                 &PolicyName=delivery&PolicyDocument={document}"
            ),
        )
        .await;
    }
    arn
}

/// The request template of the `PutItem` resolver: the `2018-05-29` operation an
/// `AMAZON_DYNAMODB` data source is called with, with the field's own arguments marshalled by
/// `$util.dynamodb.toDynamoDBJson`.
const PUT_ITEM_REQUEST: &str = "{\"version\": \"2018-05-29\", \"operation\": \"PutItem\", \
                                \"key\": {\"id\": $util.dynamodb.toDynamoDBJson($ctx.args.id)}, \
                                \"attributeValues\": \
                                {\"kind\": $util.dynamodb.toDynamoDBJson($ctx.args.kind)}}";

/// The request template of the `GetItem` resolvers, over the same marshalling.
const GET_ITEM_REQUEST: &str = "{\"version\": \"2018-05-29\", \"operation\": \"GetItem\", \
                                \"key\": {\"id\": $util.dynamodb.toDynamoDBJson($ctx.args.id)}}";

/// One created API: the id AppSync minted for it, the ARN the account holds it under, and
/// the address its `GRAPHQL` endpoint is published at.
struct GraphqlApi {
    api_id: String,
    arn: String,
    /// `dns.GRAPHQL`, the `host:port` that endpoint is served at — the address an endpoint
    /// ARN's label is read off.
    graphql: String,
}

/// Create the `AWS_IAM` API with [`SCHEMA`], the `AMAZON_DYNAMODB` data source reached under
/// `role_arn`, and the three resolvers the rows select — each defined by its two VTL mapping
/// templates.
async fn given_api(emulator: &Emulator, case: &str, role_arn: &str) -> GraphqlApi {
    let created = appsync_ok(
        emulator,
        case,
        "POST",
        "/v1/apis",
        Some(json!({"name": "event-driven", "authenticationType": "AWS_IAM"})),
    )
    .await;
    let api = &created["graphqlApi"];
    let api_id = api["apiId"].as_str().unwrap_or_default().to_string();
    let arn = api["arn"].as_str().unwrap_or_default().to_string();
    let graphql = api["dns"]["GRAPHQL"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !api_id.is_empty() && !arn.is_empty() && !graphql.is_empty(),
        "case {case:?}: CreateGraphqlApi reports the API's id, ARN and published address, \
         got {created}"
    );

    // The schema, waiting for the clock to perform the creation: the wait is on the status
    // the service reports, so nothing here depends on how fast the host runs.
    use base64::Engine as _;
    let definition = base64::engine::general_purpose::STANDARD.encode(SCHEMA);
    appsync_ok(
        emulator,
        case,
        "POST",
        &format!("/v1/apis/{api_id}/schemacreation"),
        Some(json!({"definition": definition})),
    )
    .await;
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let settled = appsync_ok(
            emulator,
            case,
            "GET",
            &format!("/v1/apis/{api_id}/schemacreation"),
            None,
        )
        .await;
        if settled["status"].as_str() == Some("SUCCESS") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the schema creation is performed, last read {settled}"
        );
        tokio::time::sleep(POLL_SPACING).await;
    }

    appsync_ok(
        emulator,
        case,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({
            "name": DATA_SOURCE,
            "type": "AMAZON_DYNAMODB",
            "serviceRoleArn": role_arn,
            "dynamodbConfig": {"tableName": TABLE, "awsRegion": REGION},
        })),
    )
    .await;

    let resolvers: [(&str, &str, &str, String); 3] = [
        (
            "Mutation",
            "putItem",
            PUT_ITEM_REQUEST,
            "$util.toJson($ctx.result)".to_string(),
        ),
        (
            "Query",
            "getItem",
            GET_ITEM_REQUEST,
            "$util.toJson($ctx.result)".to_string(),
        ),
        (
            // A resolver whose response template RAISES: the request reads rather than
            // writes, so the field resolves to nothing and the table is left alone.
            "Mutation",
            "refuseItem",
            GET_ITEM_REQUEST,
            format!("$util.error('{REFUSAL_MESSAGE}', '{REFUSAL_TYPE}')"),
        ),
    ];
    for (type_name, field, request, response) in resolvers {
        appsync_ok(
            emulator,
            case,
            "POST",
            &format!("/v1/apis/{api_id}/types/{type_name}/resolvers"),
            Some(json!({
                "fieldName": field,
                "kind": "UNIT",
                "dataSourceName": DATA_SOURCE,
                "requestMappingTemplate": request,
                "responseMappingTemplate": response,
            })),
        )
        .await;
    }
    GraphqlApi {
        api_id,
        arn,
        graphql,
    }
}

/// How an endpoint ARN names the API: the resource an AppSync GraphQL endpoint's ARN carries,
/// with `label` read off the address that endpoint is published at.
fn endpoint_arn(label: &str) -> String {
    engine::emulator_arn("appsync", REGION, &format!("endpoints/graphql-api/{label}"))
}

/// How an IAM policy — and a rule's target — addresses one field of one API: the API's own
/// ARN extended with the type and the field.
fn field_arn(api_arn: &str, type_name: &str, field: &str) -> String {
    format!("{api_arn}/types/{type_name}/fields/{field}")
}

/// A dead-letter queue called `name`, carrying the policy EventBridge's own dead-letter write
/// needs ("grant permissions to the EventBridge service principal"); answers its url and ARN.
async fn given_dead_letter_queue(emulator: &Emulator, case: &str, name: &str) -> (String, String) {
    let (status, created) = sqs(emulator, "CreateQueue", &json!({"QueueName": name})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue answered {status} {created}"
    );
    let url = created["QueueUrl"].as_str().unwrap_or_default().to_string();
    let (status, attributes) = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes answered {status} {attributes}"
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": SERVICE_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    })
    .to_string();
    let (status, written) = sqs(
        emulator,
        "SetQueueAttributes",
        &json!({"QueueUrl": url, "Attributes": {"Policy": policy}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetQueueAttributes answered {status} {written}"
    );
    (url, arn)
}

/// A rule matching the events of ONE case — its own `detail-type` — carrying `targets`.
async fn given_rule(emulator: &Emulator, case: &str, name: &str, detail_type: &str, targets: Value) {
    let pattern = json!({"source": [SOURCE], "detail-type": [detail_type]}).to_string();
    let (status, rule) = events(
        emulator,
        "PutRule",
        &json!({"Name": name, "EventPattern": pattern, "State": "ENABLED"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRule answered {status} {rule}"
    );
    let (status, written) = events(
        emulator,
        "PutTargets",
        &json!({"Rule": name, "Targets": targets}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutTargets answered {status} {written}"
    );
    assert_eq!(
        written["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {written}"
    );
}

/// Publish ONE event of this case, which its rule alone matches.
async fn publish(emulator: &Emulator, case: &str, detail_type: &str, detail: &Value) {
    let (status, published) = events(
        emulator,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": detail_type,
                "Detail": detail.to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutEvents answered {status} {published}"
    );
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutEvents reported a failed entry: {published}"
    );
}

/// What EventBridge recorded about each handover of `rule`: its outcome.
fn outcomes(emulator: &Emulator, rule: &str) -> Vec<InvocationOutcome> {
    emulator
        .engine
        .state::<ComAmazonawsEventbridgeAwseventsState>()
        .lock()
        .invocations
        .iter()
        .filter(|invocation| invocation.rule == rule)
        .map(|invocation| invocation.outcome.clone())
        .collect()
}

/// The item the table holds under `id`, as a client reads it — `None` when it holds none.
async fn item(emulator: &Emulator, case: &str, id: &str) -> Option<Value> {
    let held = dynamodb(
        emulator,
        case,
        "GetItem",
        &json!({"TableName": TABLE, "Key": {KEY_ATTRIBUTE: {"S": id}}}),
    )
    .await;
    held.get("Item").cloned()
}

/// Every message waiting on the queue at `url`, with its attributes.
async fn dead_letters(emulator: &Emulator, case: &str, url: &str) -> Vec<Value> {
    let (status, received) = sqs(
        emulator,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "MessageAttributeNames": ["All"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage answered {status} {received}"
    );
    received["Messages"].as_array().cloned().unwrap_or_default()
}

/// The condition and the sentence one dead letter reports, as a consumer of the queue reads
/// them off its message attributes.
fn parked_refusal(parked: &Value) -> (String, String) {
    let attribute = |name: &str| {
        parked["MessageAttributes"][name]["StringValue"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    (attribute("ERROR_CODE"), attribute("ERROR_MESSAGE"))
}

/// AppSync's own error type for a call it has not authorized, which the sentence of a
/// delivery the target's role barred carries beside the status that endpoint answers.
const UNAUTHORIZED_ERROR_TYPE: &str = "UnauthorizedException";

/// What the dead letter of a delivery `expected` must report: the condition, and — for a
/// delivery the role's policies barred — the answer the API's own endpoint gives an identity
/// it has not authorized, the `401` and that error type.
fn assert_parked(case: &str, parked: &[Value], expected: &str) {
    assert_eq!(
        parked.len(),
        1,
        "case {case:?}: a failed delivery parks the event, got {parked:?}"
    );
    let (code, message) = parked_refusal(&parked[0]);
    assert_eq!(
        code, expected,
        "case {case:?}: the dead letter names the condition, got {:?}",
        parked[0]
    );
    if expected == NO_PERMISSIONS {
        assert!(
            message.contains("\"401\"") && message.contains(UNAUTHORIZED_ERROR_TYPE),
            "case {case:?}: the dead letter reports the answer the endpoint gives an \
             identity it has not authorized, got {message:?}"
        );
    }
}

/// What the target receives, which is what fills the operation's variables.
enum Receives {
    /// An `InputTransformer` rendering the named members of the event's detail into the
    /// variables object.
    Transformed(&'static [&'static str]),
    /// The target's own constant `Input` — "nothing from the event itself is passed to the
    /// target".
    Input(&'static str),
}

/// One row of the table: a target of one field, under one role, receiving one input.
struct Row {
    case: &'static str,
    /// The type and field the target's ARN names.
    type_name: &'static str,
    field: &'static str,
    /// The operation the target carries, `None` for a target that states none.
    operation: Option<&'static str>,
    /// Which of the roles created up front the target names.
    role: &'static str,
    /// Whether the ARN names the API this account holds.
    api_exists: bool,
    /// What the target receives.
    receives: Receives,
    /// The condition the delivery must fail with, `None` for a delivery the API accepted.
    failed_with: Option<&'static str>,
    /// Whether the table holds this row's item afterwards.
    writes: bool,
}

/// The operation each row's target carries: a document with named variables, which is what
/// the target's input fills.
const PUT_OPERATION: &str = "mutation PutItem($id: ID!, $kind: String!) \
                             { putItem(id: $id, kind: $kind) { id kind } }";
const GET_OPERATION: &str = "query GetItem($id: ID!) { getItem(id: $id) { id kind } }";
const REFUSE_OPERATION: &str = "mutation RefuseItem($id: ID!) { refuseItem(id: $id) { id } }";

/// An operation selecting TWO root fields: the call reaches both, so the role the target
/// names has to be granted `appsync:GraphQL` on each of them.
const BOTH_FIELDS_OPERATION: &str = "mutation Both($id: ID!, $kind: String!) \
                                     { putItem(id: $id, kind: $kind) { id kind } \
                                     refuseItem(id: $id) { id } }";

/// A rule target naming one field of an AppSync GraphQL API is delivered to by EXECUTING the
/// target's own GraphQL operation on that API, under the target's role, with what the target
/// receives as the operation's variables.
#[tokio::test]
async fn an_appsync_field_target_executes_the_targets_operation_on_the_api() {
    let rows: &[Row] = &[
        Row {
            case: "a mutation whose resolver writes the mapped item",
            type_name: "Mutation",
            field: "putItem",
            operation: Some(PUT_OPERATION),
            role: "granted",
            api_exists: true,
            receives: Receives::Transformed(&["id", "kind"]),
            failed_with: None,
            writes: true,
        },
        Row {
            case: "a role granting the action on this very field",
            type_name: "Mutation",
            field: "putItem",
            operation: Some(PUT_OPERATION),
            role: "field-scoped",
            api_exists: true,
            receives: Receives::Transformed(&["id", "kind"]),
            failed_with: None,
            writes: true,
        },
        Row {
            case: "a role granting the action on another field of the same API",
            type_name: "Mutation",
            field: "putItem",
            operation: Some(PUT_OPERATION),
            role: "other-field",
            api_exists: true,
            receives: Receives::Transformed(&["id", "kind"]),
            failed_with: Some(NO_PERMISSIONS),
            writes: false,
        },
        Row {
            case: "a role whose policy names another action",
            type_name: "Mutation",
            field: "putItem",
            operation: Some(PUT_OPERATION),
            role: "other-action",
            api_exists: true,
            receives: Receives::Transformed(&["id", "kind"]),
            failed_with: Some(NO_PERMISSIONS),
            writes: false,
        },
        Row {
            case: "a role whose policies state nothing",
            type_name: "Mutation",
            field: "putItem",
            operation: Some(PUT_OPERATION),
            role: "silent",
            api_exists: true,
            receives: Receives::Transformed(&["id", "kind"]),
            failed_with: Some(NO_PERMISSIONS),
            writes: false,
        },
        Row {
            case: "a role EventBridge cannot assume",
            type_name: "Mutation",
            field: "putItem",
            operation: Some(PUT_OPERATION),
            role: "untrusted",
            api_exists: true,
            receives: Receives::Transformed(&["id", "kind"]),
            failed_with: Some(FAILED_TO_ASSUME_ROLE),
            writes: false,
        },
        Row {
            case: "a query operation, which writes nothing",
            type_name: "Query",
            field: "getItem",
            operation: Some(GET_OPERATION),
            role: "granted",
            api_exists: true,
            receives: Receives::Transformed(&["id"]),
            failed_with: None,
            writes: false,
        },
        Row {
            // The delivery AWS counts as successful: a resolver that raises is still a 200,
            // so the event was accepted and nothing is dead-lettered.
            case: "a resolver error in a 200 answer",
            type_name: "Mutation",
            field: "refuseItem",
            operation: Some(REFUSE_OPERATION),
            role: "granted",
            api_exists: true,
            receives: Receives::Transformed(&["id"]),
            failed_with: None,
            writes: false,
        },
        Row {
            // What a constant input WRITES is asserted on its own
            // ([`a_constant_input_is_what_fills_the_operations_variables`]): here the point is
            // that the delivery is made and that nothing of the EVENT reached the operation,
            // so this row's own item is not in the table.
            case: "the target's own constant input as the variables",
            type_name: "Mutation",
            field: "putItem",
            operation: Some(PUT_OPERATION),
            role: "granted",
            api_exists: true,
            receives: Receives::Input(r#"{"id": "constant-input", "kind": "fixed"}"#),
            failed_with: None,
            writes: false,
        },
        Row {
            case: "an input that is not a variables object",
            type_name: "Mutation",
            field: "putItem",
            operation: Some(PUT_OPERATION),
            role: "granted",
            api_exists: true,
            receives: Receives::Input(r#""not-an-object""#),
            failed_with: Some(INVALID_JSON),
            writes: false,
        },
        Row {
            case: "a target naming an API this account does not hold",
            type_name: "Mutation",
            field: "putItem",
            operation: Some(PUT_OPERATION),
            role: "granted",
            api_exists: false,
            receives: Receives::Transformed(&["id", "kind"]),
            failed_with: Some(NO_RESOURCE),
            writes: false,
        },
        Row {
            case: "a target stating no operation to execute",
            type_name: "Mutation",
            field: "putItem",
            operation: None,
            role: "granted",
            api_exists: true,
            receives: Receives::Transformed(&["id", "kind"]),
            failed_with: Some(INVALID_PARAMETER),
            writes: false,
        },
    ];

    let setup = "the topology every row is delivered through";
    let emulator = emulator();
    let table_arn = given_table(&emulator, setup).await;
    let data_source_role = given_role(
        &emulator,
        setup,
        "appsync-items-role",
        APPSYNC_PRINCIPAL,
        Some(("dynamodb:*", &table_arn)),
    )
    .await;
    let api = given_api(&emulator, setup, &data_source_role).await;
    let api_arn = api.arn.clone();

    // The roles the rows name, each stating what it allows about this delivery.
    let put_field = field_arn(&api_arn, "Mutation", "putItem");
    let get_field = field_arn(&api_arn, "Query", "getItem");
    let roles: Vec<(&str, String)> = vec![
        (
            "granted",
            given_role(
                &emulator,
                setup,
                "events-graphql",
                SERVICE_PRINCIPAL,
                Some(("appsync:GraphQL", "*")),
            )
            .await,
        ),
        (
            "field-scoped",
            given_role(
                &emulator,
                setup,
                "events-graphql-field",
                SERVICE_PRINCIPAL,
                Some(("appsync:GraphQL", &put_field)),
            )
            .await,
        ),
        (
            "other-field",
            given_role(
                &emulator,
                setup,
                "events-graphql-other-field",
                SERVICE_PRINCIPAL,
                Some(("appsync:GraphQL", &get_field)),
            )
            .await,
        ),
        (
            "other-action",
            given_role(
                &emulator,
                setup,
                "events-graphql-other-action",
                SERVICE_PRINCIPAL,
                Some(("appsync:GetGraphqlApi", "*")),
            )
            .await,
        ),
        (
            "silent",
            given_role(&emulator, setup, "events-graphql-silent", SERVICE_PRINCIPAL, None).await,
        ),
        (
            "untrusted",
            given_role(
                &emulator,
                setup,
                "events-graphql-untrusted",
                APPSYNC_PRINCIPAL,
                Some(("appsync:GraphQL", "*")),
            )
            .await,
        ),
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let id = format!("item-{index}");
        let kind = format!("kind-{index}");
        let role = roles
            .iter()
            .find(|(name, _)| *name == row.role)
            .map(|(_, arn)| arn.clone())
            .unwrap_or_else(|| panic!("case {case:?}: the row names a role of its own table"));
        let api = if row.api_exists {
            api_arn.clone()
        } else {
            engine::emulator_arn("appsync", REGION, "apis/noSuchApiOfThisAccount")
        };
        let target_arn = field_arn(&api, row.type_name, row.field);
        let rule = format!("to-the-field-{index}");
        let detail_type = format!("Case {index}");
        let (queue_url, queue_arn) =
            given_dead_letter_queue(&emulator, case, &format!("dlq-{index}")).await;

        let mut target = json!({
            "Id": "graphql",
            "Arn": target_arn,
            "RoleArn": role,
            "DeadLetterConfig": {"Arn": queue_arn},
        });
        if let Some(operation) = row.operation {
            target["AppSyncParameters"] = json!({"GraphQLOperation": operation});
        }
        match row.receives {
            Receives::Transformed(members) => {
                let paths: serde_json::Map<String, Value> = members
                    .iter()
                    .map(|member| {
                        (
                            (*member).to_string(),
                            Value::String(format!("$.detail.{member}")),
                        )
                    })
                    .collect();
                let template = members
                    .iter()
                    .map(|member| format!("\"{member}\": <{member}>"))
                    .collect::<Vec<String>>()
                    .join(", ");
                target["InputTransformer"] = json!({
                    "InputPathsMap": paths,
                    "InputTemplate": format!("{{{template}}}"),
                });
            }
            Receives::Input(input) => target["Input"] = Value::String(input.to_string()),
        }
        given_rule(&emulator, case, &rule, &detail_type, json!([target])).await;

        publish(
            &emulator,
            case,
            &detail_type,
            &json!({"id": id, "kind": kind}),
        )
        .await;

        // What the delivery came to, as EventBridge recorded it.
        let recorded = outcomes(&emulator, &rule);
        assert_eq!(
            recorded.len(),
            1,
            "case {case:?}: expected one handover, got {recorded:?}"
        );
        match (&recorded[0], row.failed_with) {
            (InvocationOutcome::Invoked, None) => {}
            (InvocationOutcome::Failed { code, .. }, Some(expected)) => assert_eq!(
                code, expected,
                "case {case:?}: expected the delivery to fail with {expected}, got {recorded:?}"
            ),
            (outcome, expected) => panic!(
                "case {case:?}: expected the delivery to come to {expected:?}, got {outcome:?}"
            ),
        }

        // What the resolver DID, read back through DynamoDB's own reader.
        let held = item(&emulator, case, &id).await;
        assert_eq!(
            held.is_some(),
            row.writes,
            "case {case:?}: expected the table to hold the item: {}, got {held:?}",
            row.writes
        );
        if row.writes {
            assert_eq!(
                held,
                Some(json!({"id": {"S": id}, "kind": {"S": kind}})),
                "case {case:?}: expected the mapped item, got {held:?}"
            );
        }

        // And what was parked: a failed delivery is dead-lettered carrying its condition, an
        // accepted one is dead-lettered nowhere.
        let parked = dead_letters(&emulator, case, &queue_url).await;
        match row.failed_with {
            None => assert!(
                parked.is_empty(),
                "case {case:?}: an accepted delivery parks nothing, got {parked:?}"
            ),
            Some(expected) => assert_parked(case, &parked, expected),
        }
    }
}

/// The item the constant-input row writes is keyed by the id that input states, which is not
/// the row's own: the variables come from what the TARGET receives, so a target carrying its
/// own `Input` writes what that input says and nothing of the event reaches the API.
#[tokio::test]
async fn a_constant_input_is_what_fills_the_operations_variables() {
    let case = "a target whose own Input states the variables";
    let emulator = emulator();
    let table_arn = given_table(&emulator, case).await;
    let data_source_role = given_role(
        &emulator,
        case,
        "appsync-items-role",
        APPSYNC_PRINCIPAL,
        Some(("dynamodb:*", &table_arn)),
    )
    .await;
    let api = given_api(&emulator, case, &data_source_role).await;
    let api_arn = api.arn.clone();
    let role = given_role(
        &emulator,
        case,
        "events-graphql",
        SERVICE_PRINCIPAL,
        Some(("appsync:GraphQL", "*")),
    )
    .await;
    let detail_type = "Constant Input";
    given_rule(
        &emulator,
        case,
        "to-the-field",
        detail_type,
        json!([{
            "Id": "graphql",
            "Arn": field_arn(&api_arn, "Mutation", "putItem"),
            "RoleArn": role,
            "Input": r#"{"id": "from-the-input", "kind": "stated"}"#,
            "AppSyncParameters": {"GraphQLOperation": PUT_OPERATION},
        }]),
    )
    .await;

    publish(
        &emulator,
        case,
        detail_type,
        &json!({"id": "from-the-event", "kind": "published"}),
    )
    .await;

    assert_eq!(
        item(&emulator, case, "from-the-input").await,
        Some(json!({"id": {"S": "from-the-input"}, "kind": {"S": "stated"}})),
        "case {case:?}: the item the constant input states is the one that was written"
    );
    assert_eq!(
        item(&emulator, case, "from-the-event").await,
        None,
        "case {case:?}: nothing of the event reaches an operation whose variables the \
         target's own input states"
    );
}

/// One row of the endpoint table: the case, the label the endpoint ARN carries, the operation
/// the target carries, the role it names, the condition the delivery must fail with (`None`
/// for one the API accepted) and whether the table holds the row's item afterwards.
type EndpointRow<'a> = (
    &'static str,
    String,
    &'static str,
    &'a str,
    Option<&'static str>,
    bool,
);

/// A target that names the API by its published GRAPHQL ENDPOINT — the form a rule's AppSync
/// target carries — is delivered to the same API, and the `appsync:GraphQL` of that delivery
/// is judged on the FIELDS its operation selects rather than on the ARN the target states.
///
/// The rows differ along the LABEL the endpoint ARN carries: every part of the address the
/// API published for that endpoint names it (the whole authority, its host, the first label
/// of that host) as its own id does, and a label no API of the account published names none.
/// The last three keep one of those labels and change the ROLE and the OPERATION instead: a
/// role granted the action on another field of the same API delivers nothing, and an
/// operation selecting TWO root fields needs the action on both — a role granted it on one of
/// them delivers nothing, and one granted both delivers.
#[tokio::test]
async fn an_endpoint_target_names_the_api_by_its_published_address() {
    let setup = "the API every row below is addressed by its endpoint";
    let emulator = emulator();
    let table_arn = given_table(&emulator, setup).await;
    let data_source_role = given_role(
        &emulator,
        setup,
        "appsync-items-role",
        APPSYNC_PRINCIPAL,
        Some(("dynamodb:*", &table_arn)),
    )
    .await;
    let api = given_api(&emulator, setup, &data_source_role).await;
    let host = api
        .graphql
        .rsplit_once(':')
        .map_or(api.graphql.clone(), |(host, _)| host.to_string());
    let first_label = host.split('.').next().unwrap_or(&host).to_string();
    let granted = given_role(
        &emulator,
        setup,
        "events-graphql",
        SERVICE_PRINCIPAL,
        Some(("appsync:GraphQL", "*")),
    )
    .await;
    let on_another_field = given_role(
        &emulator,
        setup,
        "events-graphql-other-field",
        SERVICE_PRINCIPAL,
        Some((
            "appsync:GraphQL",
            &field_arn(&api.arn, "Query", "getItem"),
        )),
    )
    .await;
    let on_the_written_field = given_role(
        &emulator,
        setup,
        "events-graphql-written-field",
        SERVICE_PRINCIPAL,
        Some((
            "appsync:GraphQL",
            &field_arn(&api.arn, "Mutation", "putItem"),
        )),
    )
    .await;

    // (case, the label the endpoint ARN carries, the operation the target carries, the role
    //  the target names, the condition the delivery must fail with, whether the item is
    //  written)
    let rows: [EndpointRow<'_>; 8] = [
        (
            "the whole address the endpoint publishes",
            api.graphql.clone(),
            PUT_OPERATION,
            &granted,
            None,
            true,
        ),
        (
            "the host of that address",
            host.clone(),
            PUT_OPERATION,
            &granted,
            None,
            true,
        ),
        (
            "the first label of that host",
            first_label,
            PUT_OPERATION,
            &granted,
            None,
            true,
        ),
        (
            "the API's own id, which is the label AWS publishes",
            api.api_id.clone(),
            PUT_OPERATION,
            &granted,
            None,
            true,
        ),
        (
            "a label no API of this account publishes",
            "noSuchEndpointLabel".to_string(),
            PUT_OPERATION,
            &granted,
            Some(NO_RESOURCE),
            false,
        ),
        (
            "a role granted the action on another field of the API",
            api.graphql.clone(),
            PUT_OPERATION,
            &on_another_field,
            Some(NO_PERMISSIONS),
            false,
        ),
        (
            // Two root fields, the action granted on one of them: the call reaches both, so
            // it is refused and neither field runs.
            "an operation selecting two fields under a role granted one of them",
            api.graphql.clone(),
            BOTH_FIELDS_OPERATION,
            &on_the_written_field,
            Some(NO_PERMISSIONS),
            false,
        ),
        (
            // The same operation under a role granted both: the delivery is made, the
            // mutation writes, and the raising field's error inside the 200 changes nothing.
            "the same operation under a role granted both fields",
            api.graphql.clone(),
            BOTH_FIELDS_OPERATION,
            &granted,
            None,
            true,
        ),
    ];

    for (index, (case, label, operation, role, failed_with, writes)) in
        rows.into_iter().enumerate()
    {
        let id = format!("endpoint-{index}");
        let kind = format!("kind-{index}");
        let rule = format!("to-the-endpoint-{index}");
        let detail_type = format!("Endpoint {index}");
        let (queue_url, queue_arn) =
            given_dead_letter_queue(&emulator, case, &format!("endpoint-dlq-{index}")).await;
        given_rule(
            &emulator,
            case,
            &rule,
            &detail_type,
            json!([{
                "Id": "graphql",
                "Arn": endpoint_arn(&label),
                "RoleArn": role,
                "DeadLetterConfig": {"Arn": queue_arn},
                "AppSyncParameters": {"GraphQLOperation": operation},
                "InputTransformer": {
                    "InputPathsMap": {"id": "$.detail.id", "kind": "$.detail.kind"},
                    "InputTemplate": "{\"id\": <id>, \"kind\": <kind>}",
                },
            }]),
        )
        .await;

        publish(
            &emulator,
            case,
            &detail_type,
            &json!({"id": id, "kind": kind}),
        )
        .await;

        let recorded = outcomes(&emulator, &rule);
        assert_eq!(
            recorded.len(),
            1,
            "case {case:?}: expected one handover, got {recorded:?}"
        );
        match (&recorded[0], failed_with) {
            (InvocationOutcome::Invoked, None) => {}
            (InvocationOutcome::Failed { code, .. }, Some(expected)) => assert_eq!(
                code, expected,
                "case {case:?}: expected the delivery to fail with {expected}, got {recorded:?}"
            ),
            (outcome, expected) => panic!(
                "case {case:?}: expected the delivery to come to {expected:?}, got {outcome:?}"
            ),
        }

        let held = item(&emulator, case, &id).await;
        assert_eq!(
            held.is_some(),
            writes,
            "case {case:?}: expected the table to hold the item: {writes}, got {held:?}"
        );
        let parked = dead_letters(&emulator, case, &queue_url).await;
        match failed_with {
            None => assert!(
                parked.is_empty(),
                "case {case:?}: an accepted delivery parks nothing, got {parked:?}"
            ),
            Some(expected) => assert_parked(case, &parked, expected),
        }
    }
}
