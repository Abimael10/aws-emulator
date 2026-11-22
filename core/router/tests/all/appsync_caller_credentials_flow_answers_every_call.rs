// SPDX-License-Identifier: CC0-1.0

//! The `AWS_IAM` caller-credentials path of a GraphQL API over an `AMAZON_DYNAMODB` data
//! source, driven on a router that VERIFIES request signatures: every call of the flow is
//! SIGNED, the data-plane POST with the session an identity pool vended for a guest identity.
//!
//! RULE: every step of this flow reaches a TERMINAL answer. A state the control plane reports
//! while it is being reached settles — `GetSchemaCreationStatus` leaves `PROCESSING` for one
//! of the terminal values its `SchemaStatus` declares (`SUCCESS`, `FAILED`) and stays there
//! over later reads — a guest-credential vend answers a usable session each time it is asked
//! for, and a POST on the address the API published is ANSWERED a GraphQL document, with
//! `data` for what resolved or `errors` for what did not and never neither, under a status the
//! client can branch on. A call the permissions withheld is one of those answers rather than
//! silence: the field reports the refusal as its own error and the stored item is left exactly
//! as it was. A client that gets no answer at all cannot tell a denial from an outage, and an
//! authorization regression — a denied read that quietly mutates the item — cannot be
//! observed.
//!
//! The rows span what the rule generalises over: the kind of answer a call is owed — a field
//! that resolved to an item, a key holding no item, a read and a write the caller's own
//! permissions withhold, a write they admit, a selection the API's schema does not declare, a
//! call no field of which the caller's policies permit, and a call presenting no credentials at
//! all. Every row asserts what the answer was, and what the TABLE holds afterwards, read back
//! through Amazon DynamoDB's own API.
//!
//! The sibling suite `appsync_dynamodb_data_source_resolvers` spans the DynamoDB operations a
//! resolver names and the policy forms that admit them, on a router with request authentication
//! OFF and a header standing in for a signature. This suite is the one that pins the same path
//! where the emulator VERIFIES what every setup call signed with, where the data-plane POST
//! carries a real SigV4 signature made with the vended session, and where what is asserted is
//! that each step ANSWERS.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appsync_lambda_data_source_and_authorizer::{
    post_published_served, published_parts, ServedResponse,
};
use crate::query_wire::{element, form_encode};

/// The Region every call here is signed for, and so the Region every resource lives in.
const REGION: &str = "us-east-1";

/// The identity-provider principal an identity pool's exchange is made under.
const COGNITO_IDENTITY_PRINCIPAL: &str = "cognito-identity.amazonaws.com";

/// The service principal a data source's role is assumed by.
const APPSYNC_PRINCIPAL: &str = "appsync.amazonaws.com";

/// The table the resolvers of this suite resolve against, and its one key attribute.
const TABLE: &str = "appsync-caller-credentials-items";
const KEY_ATTRIBUTE: &str = "id";

/// The item every row reads or writes, and the key the table holds no item for.
const HELD_ID: &str = "caller-read";
const HELD_TITLE: &str = "held";
const ABSENT_ID: &str = "caller-absent";

/// The data source every resolver of this suite resolves against: the one reached under the
/// credentials of whoever made the GraphQL call (`useCallerCredentials`).
const CALLER_DATA_SOURCE: &str = "ddbCaller";

/// The one inline policy the guest role carries, replaced per row.
const ROLE_POLICY_NAME: &str = "caller-credentials-access";

/// The terminal values of `SchemaStatus` a schema creation settles on — of `PROCESSING`,
/// `ACTIVE`, `DELETING`, `FAILED`, `SUCCESS` and `NOT_APPLICABLE` (smithy_query shape
/// `SchemaStatus`), the two a started creation ends at.
const TERMINAL_SCHEMA_STATUSES: &[&str] = &["SUCCESS", "FAILED"];

/// The IAM API version every awsQuery call to it carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// The response field every answer the API's GraphQL endpoint writes carries the id of the
/// request it answers in, which is how a client correlates an answer with the call it made.
const REQUEST_ID_FIELD: &str = "x-amzn-requestid";

/// The `errorType` a refusal of the API's own authorization mode carries, and the one a
/// refusal Amazon DynamoDB answered is reported under: that service's model declares no error
/// for an authorization refusal, so it is raised as its base exception.
const UNAUTHORIZED_ERROR_TYPE: &str = "UnauthorizedException";
const DYNAMODB_ERROR_TYPE: &str = "DynamoDB:DynamoDbException";

/// The fixed opening of a GraphQL validation failure's sentence, which a selection the schema
/// does not declare is reported with.
const VALIDATION_ERROR: &str = "Validation error of type FieldUndefined";

/// How long a wait on a CONDITION may take, and how often this suite looks again. Generous:
/// what is under test is that a step answers, never how fast the host reaches a transition.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The schema every API of this suite is created with: the item the table holds, the read of
/// one and the write of one.
const SCHEMA: &str = "\
type Item {\n  id: String\n  title: String\n  count: Int\n}\n\
\n\
type Query {\n  callerItem(id: String): Item\n}\n\
\n\
type Mutation {\n  callerUpdateItem(id: String, title: String): Item\n}\n";

/// The response half every resolver of this suite carries: a call the table REFUSED is
/// reported as the field's own error, carrying the class the refusal is of, and anything else
/// is the value the call answered.
const RESPONSE_FUNCTION: &str = "export function response(ctx) {\n  \
                                 if (ctx.error) {\n    \
                                 util.error(ctx.error.message, ctx.error.type);\n  }\n  \
                                 return ctx.result;\n}\n";

/// The five services this flow spans, over ONE engine, with request authentication ON: whether
/// the credential each call signed with is one this emulator holds is part of what this suite
/// asks.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(true);
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
    router.register_service(
        service_com_amazonaws_cognitoidentity_awscognitoidentityservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService::new(engine),
    );
    router
}

/// The key pair this process publishes once request authentication is on, which every SETUP
/// call is signed with.
fn published() -> &'static (String, String) {
    static PUBLISHED: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    PUBLISHED.get_or_init(|| {
        let minted = ::emulator_server::SigV4Authenticator::minted_credentials()
            .expect("turning request authentication on publishes this process's keys");
        (
            minted.access_key_id.clone(),
            minted.secret_access_key.clone(),
        )
    })
}

/// The session a vend handed back: the pair plus the token that travels with it.
///
/// No `Debug`, on purpose: a value holding a secret must not be renderable into a panic
/// message. What an assertion about a session quotes is its access key id, the half that
/// names it.
#[derive(Clone)]
struct Session {
    access_key_id: String,
    secret_key: String,
    session_token: String,
}

/// The credential a request is signed with. No `Debug`, for the reason [`Session`] has none.
#[derive(Clone)]
enum SignedWith {
    /// The key pair this process published, which every setup call signs with.
    Published,
    /// A session an identity pool vended, which the data-plane calls sign with.
    Vended(Session),
}

impl SignedWith {
    fn identity(&self) -> Credentials {
        match self {
            Self::Published => {
                let (key_id, secret) = published();
                Credentials::new(key_id, secret, None, None, "the published key pair")
            }
            Self::Vended(session) => Credentials::new(
                &session.access_key_id,
                &session.secret_key,
                Some(session.session_token.clone()),
                None,
                "the vended session",
            ),
        }
    }
}

/// One request to make: what it is addressed to, what it is signed for, and what it carries.
struct Wire<'a> {
    signing_name: &'a str,
    method: &'a str,
    /// The authority the request states it is addressed to: the service's host for a
    /// control-plane call, and the `host:port` the API PUBLISHED for a call on its endpoint.
    authority: String,
    path: &'a str,
    headers: Vec<(&'a str, String)>,
    body: Vec<u8>,
}

/// `wire` SIGNED with `credential` the way an SDK signs it: the signer writes the
/// `Authorization` header, the `x-amz-date` and — for a session — the `x-amz-security-token`
/// that travels beside the signature. The ONE signing site of this suite, so a control-plane
/// call and a call on the API's own socket cannot differ on how they are signed.
fn signed(wire: &Wire<'_>, credential: &SignedWith) -> http::Request<Vec<u8>> {
    let mut builder = http::Request::builder()
        .method(wire.method)
        .uri(format!("https://{}{}", wire.authority, wire.path))
        .header("host", wire.authority.as_str());
    for (name, value) in &wire.headers {
        builder = builder.header(*name, value.as_str());
    }
    let mut request = builder
        .body(wire.body.clone())
        .expect("the test request builds");
    let identity = credential.identity().into();
    let params = SigningParams::builder()
        .identity(&identity)
        .region(REGION)
        .name(wire.signing_name)
        .time(SystemTime::now())
        .settings(SigningSettings::default())
        .build()
        .expect("the signing parameters are complete")
        .into();
    let (method, uri) = (request.method().to_string(), request.uri().to_string());
    let fields: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().expect("a text header").to_string(),
            )
        })
        .collect();
    let signable = SignableRequest::new(
        &method,
        &uri,
        fields
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        SignableBody::Bytes(request.body()),
    )
    .expect("the request is signable");
    let (instructions, _) = sign(signable, &params)
        .expect("the request signs")
        .into_parts();
    instructions.apply_to_request_http1x(&mut request);
    request
}

/// Dispatch one SIGNED control-plane call through the router's front door, where the signature
/// is verified, and answer its status and body.
async fn call(
    router: &MultiServiceRouter,
    credential: &SignedWith,
    wire: Wire<'_>,
) -> (u16, Vec<u8>) {
    let (parts, body) = signed(&wire, credential).into_parts();
    let response = router
        .dispatch(http::Request::from_parts(parts, bytes::Bytes::from(body)))
        .await;
    let (parts, body) = response.into_parts();
    (parts.status.as_u16(), body.to_vec())
}

/// One AppSync control-plane call (restJson1), signed with the published key, asserting it was
/// served.
async fn appsync_ok(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let payload = body.map_or_else(Vec::new, |body| body.to_string().into_bytes());
    let (status, answered) = call(
        router,
        &SignedWith::Published,
        Wire {
            signing_name: "appsync",
            method,
            authority: format!("appsync.{REGION}.amazonaws.com"),
            path,
            headers: vec![("content-type", "application/json".to_string())],
            body: payload,
        },
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    assert_eq!(
        status, 200,
        "setup: AppSync {method} {path} must answer 200, got {status} and {text}"
    );
    serde_json::from_slice(&answered).unwrap_or(Value::Null)
}

/// One Amazon DynamoDB call over its own `awsJson1_0` wire, signed with the published key,
/// asserting it was served.
async fn dynamodb_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, answered) = call(
        router,
        &SignedWith::Published,
        Wire {
            signing_name: "dynamodb",
            method: "POST",
            authority: format!("dynamodb.{REGION}.amazonaws.com"),
            path: "/",
            headers: vec![
                ("content-type", "application/x-amz-json-1.0".to_string()),
                ("x-amz-target", format!("DynamoDB_20120810.{operation}")),
            ],
            body: body.to_string().into_bytes(),
        },
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    assert_eq!(
        status, 200,
        "setup: DynamoDB {operation} must answer 200, got {status} and {text}"
    );
    serde_json::from_slice(&answered).unwrap_or(Value::Null)
}

/// One Amazon Cognito Identity call (awsJson1_1), signed with the published key, answering its
/// status and the document it answered.
async fn cognito(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let (status, answered) = call(
        router,
        &SignedWith::Published,
        Wire {
            signing_name: "cognito-identity",
            method: "POST",
            authority: format!("cognito-identity.{REGION}.amazonaws.com"),
            path: "/",
            headers: vec![
                ("content-type", "application/x-amz-json-1.1".to_string()),
                (
                    "x-amz-target",
                    format!("AWSCognitoIdentityService.{operation}"),
                ),
            ],
            body: body.to_string().into_bytes(),
        },
    )
    .await;
    (
        status,
        serde_json::from_slice(&answered).unwrap_or(Value::Null),
    )
}

/// The same, asserting the call was served.
async fn cognito_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, answered) = cognito(router, operation, body).await;
    assert_eq!(
        status, 200,
        "setup: Cognito Identity {operation} must answer 200, got {status} and {answered}"
    );
    answered
}

/// One IAM call over its awsQuery wire, signed with the published key, answering the XML body.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={IAM_API_VERSION}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, answered) = call(
        router,
        &SignedWith::Published,
        Wire {
            signing_name: "iam",
            method: "POST",
            authority: format!("iam.{REGION}.amazonaws.com"),
            path: "/",
            headers: vec![(
                "content-type",
                "application/x-www-form-urlencoded".to_string(),
            )],
            body: body.into_bytes(),
        },
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    assert_eq!(
        status, 200,
        "setup: IAM {action} must answer 200, got {status} and {text}"
    );
    text
}

/// The trust policy of a role an identity pool vends sessions of, or of one AppSync assumes.
fn trust(principal: &str, pool_id: Option<&str>) -> String {
    match pool_id {
        Some(pool) => json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {"Federated": principal},
                "Action": "sts:AssumeRoleWithWebIdentity",
                "Condition": {"StringEquals": {
                    format!("{COGNITO_IDENTITY_PRINCIPAL}:aud"): pool,
                }},
            }],
        }),
        None => json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {"Service": principal},
                "Action": "sts:AssumeRole",
            }],
        }),
    }
    .to_string()
}

/// One identity-based policy: an `Allow` of each `(action, resource)` pair and a `Deny` of
/// each.
fn policy(allowed: &[(&str, String)], denied: &[(&str, String)]) -> String {
    let statement = |effect: &str, (action, resource): &(&str, String)| {
        json!({"Effect": effect, "Action": *action, "Resource": resource})
    };
    let statements: Vec<Value> = allowed
        .iter()
        .map(|grant| statement("Allow", grant))
        .chain(denied.iter().map(|grant| statement("Deny", grant)))
        .collect();
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// Create the role `name` with `trust_policy` and `document`, answering its ARN.
async fn create_role(
    router: &MultiServiceRouter,
    name: &str,
    trust_policy: &str,
    document: &str,
) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", trust_policy),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    put_role_policy(router, name, document).await;
    arn
}

/// Put `document` on `role_name` as its one inline policy, replacing whatever the last row put
/// there.
async fn put_role_policy(router: &MultiServiceRouter, role_name: &str, document: &str) {
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", ROLE_POLICY_NAME),
            ("PolicyDocument", document),
        ],
    )
    .await;
}

/// The ARN of the table every policy of this suite is written about.
fn table_arn() -> String {
    format!(
        "arn:aws:dynamodb:{REGION}:{}:table/{TABLE}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// Create the table the resolvers resolve against and wait until that service first reports it
/// `ACTIVE` — a table that is not is one no call lands on. The wait is on the CONDITION.
async fn create_table(router: &MultiServiceRouter) {
    dynamodb_ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": KEY_ATTRIBUTE, "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = dynamodb_ok(router, "DescribeTable", json!({"TableName": TABLE})).await;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: the table must become ACTIVE, last read {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Write the one item the rows read, through DynamoDB's own API.
async fn put_held(router: &MultiServiceRouter) {
    dynamodb_ok(
        router,
        "PutItem",
        json!({
            "TableName": TABLE,
            "Item": {
                KEY_ATTRIBUTE: {"S": HELD_ID},
                "title": {"S": HELD_TITLE},
                "count": {"N": "2"},
            },
        }),
    )
    .await;
}

/// What the table holds for `id`, read through DynamoDB's OWN api — so it says what that
/// service holds rather than what the endpoint answered. `None` for a key holding no item.
async fn held(router: &MultiServiceRouter, id: &str) -> Option<Value> {
    let read = dynamodb_ok(
        router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {KEY_ATTRIBUTE: {"S": id}}}),
    )
    .await;
    read.get("Item").filter(|item| !item.is_null()).cloned()
}

/// One created GraphQL API: how it is addressed, what a policy names it by, and where it is
/// served.
struct Api {
    api_id: String,
    arn: String,
    /// The URL the API publishes for its `GRAPHQL` endpoint, which is what a client POSTs to.
    graphql_uri: String,
}

/// Create the API with the `AWS_IAM` authentication type, install [`SCHEMA`], attach the
/// caller-credentials DynamoDB data source and the two resolvers the rows drive.
///
/// The schema creation is POLLED to a terminal status, and the status it settled on is
/// answered beside the API, so the caller can assert WHICH terminal value it reached.
async fn create_api(router: &MultiServiceRouter, name: &str, role_arn: &str) -> (Api, String) {
    let created = appsync_ok(
        router,
        "POST",
        "/v1/apis",
        Some(json!({"name": name, "authenticationType": "AWS_IAM"})),
    )
    .await;
    let api = &created["graphqlApi"];
    let api_id = api["apiId"].as_str().unwrap_or_default().to_string();
    let arn = api["arn"].as_str().unwrap_or_default().to_string();
    let graphql_uri = api["uris"]["GRAPHQL"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        graphql_uri.contains("127.0.0.1:") && graphql_uri.ends_with("/graphql"),
        "setup: uris.GRAPHQL must be a URL this emulator serves, got {graphql_uri:?}"
    );
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/schemacreation"),
        Some(json!({"definition": ::aws_smithy_types::base64::encode(SCHEMA)})),
    )
    .await;
    let settled = settled_schema_status(router, &api_id).await;
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({
            "name": CALLER_DATA_SOURCE,
            "type": "AMAZON_DYNAMODB",
            "serviceRoleArn": role_arn,
            "dynamodbConfig": {
                "tableName": TABLE,
                "awsRegion": REGION,
                "useCallerCredentials": true,
            },
        })),
    )
    .await;
    // The two operations the recorded flow drives, written with the utilities module imported
    // and the key marshalled by `util.dynamodb.toMapValues`.
    create_resolver(
        router,
        &api_id,
        "Query",
        "callerItem",
        "export function request(ctx) {\n  \
         return {operation: 'GetItem', key: util.dynamodb.toMapValues({id: ctx.args.id})};\n}",
    )
    .await;
    create_resolver(
        router,
        &api_id,
        "Mutation",
        "callerUpdateItem",
        "export function request(ctx) {\n  \
         return {operation: 'UpdateItem', \
         key: util.dynamodb.toMapValues({id: ctx.args.id}), \
         update: {expression: 'SET #t = :t', \
         expressionNames: {'#t': 'title'}, \
         expressionValues: util.dynamodb.toMapValues({':t': ctx.args.title})}};\n}",
    )
    .await;
    (
        Api {
            api_id,
            arn,
            graphql_uri,
        },
        settled,
    )
}

/// Poll `GetSchemaCreationStatus` until it reports one of [`TERMINAL_SCHEMA_STATUSES`], and
/// answer that status. The wait is on the CONDITION, with a generous deadline: what is under
/// test is that the creation SETTLES, never how long it takes.
async fn settled_schema_status(router: &MultiServiceRouter, api_id: &str) -> String {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = appsync_ok(
            router,
            "GET",
            &format!("/v1/apis/{api_id}/schemacreation"),
            None,
        )
        .await;
        let status = read["status"].as_str().unwrap_or_default().to_string();
        if TERMINAL_SCHEMA_STATUSES.contains(&status.as_str()) {
            return status;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case \"a started schema creation\": expected one of \
             {TERMINAL_SCHEMA_STATUSES:?}, last read {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Attach a UNIT resolver defined by APPSYNC_JS `code` to `field` of `type_name`.
async fn create_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    type_name: &str,
    field: &str,
    request_function: &str,
) {
    let code = format!(
        "import {{ util }} from '@aws-appsync/utils';\n{request_function}\n{RESPONSE_FUNCTION}"
    );
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/{type_name}/resolvers"),
        Some(json!({
            "fieldName": field,
            "kind": "UNIT",
            "dataSourceName": CALLER_DATA_SOURCE,
            "runtime": {"name": "APPSYNC_JS", "runtimeVersion": "1.0.0"},
            "code": code,
        })),
    )
    .await;
}

/// The GUEST identity of `pool_id` a client is handed by `GetId`, which an exchange is then
/// made for. A `GetId` carrying no `Logins` names a new unauthenticated identity each time, so
/// a suite that vends twice for ONE identity takes this once.
async fn guest_identity(router: &MultiServiceRouter, case: &str, pool_id: &str) -> String {
    let identity = cognito_ok(router, "GetId", &json!({"IdentityPoolId": pool_id})).await;
    identity["IdentityId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: GetId must report an IdentityId, got {identity}"))
        .to_string()
}

/// Vend a session for `identity_id` with `GetCredentialsForIdentity`, as a client signing a
/// call with it does, asserting every member it needs to sign with is there.
async fn vend_session(router: &MultiServiceRouter, case: &str, identity_id: &str) -> Session {
    let vended = cognito_ok(
        router,
        "GetCredentialsForIdentity",
        &json!({"IdentityId": identity_id}),
    )
    .await;
    let member = |name: &str| {
        vended["Credentials"][name]
            .as_str()
            .filter(|value| !value.is_empty())
            // The document itself is not quoted: it carries the session's secrets.
            .unwrap_or_else(|| {
                panic!("case {case:?}: the vend must carry a non-empty Credentials.{name}")
            })
            .to_string()
    };
    Session {
        access_key_id: member("AccessKeyId"),
        secret_key: member("SecretKey"),
        session_token: member("SessionToken"),
    }
}

/// POST one GraphQL document to the URL the API PUBLISHED, as a client does: the host, the port
/// and the path all come from `uris.GRAPHQL` rather than from anything this suite knows, and
/// the request is SIGNED with `credential` for this service — or presents nothing at all, where
/// a row presents no credentials. The exchange is answered as [`ServedResponse`].
///
/// The exchange itself is the ONE socket client of this target's AppSync data-plane suites
/// ([`post_published_served`]); what this adds is the material the request presents, which is
/// what the rule under test here turns on.
async fn post_published(
    case: &str,
    api: &Api,
    credential: Option<&SignedWith>,
    document: &Value,
) -> ServedResponse {
    let (authority, path) = published_parts(case, &api.graphql_uri);
    let wire = Wire {
        signing_name: "appsync",
        method: "POST",
        authority,
        path: &path,
        headers: vec![("content-type", "application/json".to_string())],
        body: document.to_string().into_bytes(),
    };
    // The fields the SIGNER wrote, which is what the call presents: the `Authorization`, the
    // `x-amz-date` and the session's own `x-amz-security-token`. The host and the content type
    // the socket client writes itself, so they are not sent twice.
    let written: Vec<(String, String)> = credential
        .map(|credential| {
            signed(&wire, credential)
                .headers()
                .iter()
                .filter(|(name, _)| !["host", "content-type"].contains(&name.as_str()))
                .map(|(name, value)| {
                    (
                        name.as_str().to_string(),
                        value.to_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let presented: Vec<(&str, String)> = written
        .iter()
        .map(|(name, value)| (name.as_str(), value.clone()))
        .collect();
    post_published_served(case, &api.graphql_uri, document, &presented).await
}

/// The pool whose guest sessions the data-plane calls sign with, the role every one of its
/// exchanges vends a session of, and that role's name — so a row can replace its one policy.
struct Pool {
    id: String,
    role_name: String,
}

/// Create the identity pool, the role its unauthenticated exchanges vend a session of, and
/// bind the two with `SetIdentityPoolRoles`.
async fn create_pool(router: &MultiServiceRouter, name: &str, api_arn: &str) -> Pool {
    let pool = cognito_ok(
        router,
        "CreateIdentityPool",
        &json!({"IdentityPoolName": name, "AllowUnauthenticatedIdentities": true}),
    )
    .await;
    let pool_id = pool["IdentityPoolId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: a created pool reports an id, got {pool}"))
        .to_string();
    let role_name = format!("{name}-guest");
    let role = create_role(
        router,
        &role_name,
        &trust(COGNITO_IDENTITY_PRINCIPAL, Some(&pool_id)),
        &policy(&[("appsync:GraphQL", format!("{api_arn}/*"))], &[]),
    )
    .await;
    cognito_ok(
        router,
        "SetIdentityPoolRoles",
        &json!({
            "IdentityPoolId": &pool_id,
            "Roles": {"authenticated": &role, "unauthenticated": &role},
        }),
    )
    .await;
    Pool {
        id: pool_id,
        role_name,
    }
}

#[tokio::test]
async fn every_polled_step_of_the_caller_credentials_flow_reaches_a_terminal_state() {
    let router = build_router();
    create_table(&router).await;
    let data_source_role = create_role(
        &router,
        "appsync-settles-data-source",
        &trust(APPSYNC_PRINCIPAL, None),
        &policy(&[("dynamodb:*", table_arn())], &[]),
    )
    .await;
    let (api, settled) = create_api(&router, "ddb-caller-settles", &data_source_role).await;

    // The schema creation SETTLED, and on the value a creation that was performed settles on:
    // a status that stays `PROCESSING` is what leaves a client polling forever.
    let case = "the schema creation a started flow polls";
    assert_eq!(
        settled, "SUCCESS",
        "case {case:?}: expected the terminal status of a performed creation, got {settled:?}"
    );
    // And it STAYS there: reading the status is a report, so a later read answers the same
    // terminal value rather than restarting the creation.
    for look in 1..=3 {
        let read = appsync_ok(
            &router,
            "GET",
            &format!("/v1/apis/{}/schemacreation", api.api_id),
            None,
        )
        .await;
        assert_eq!(
            read["status"].as_str(),
            Some(settled.as_str()),
            "case {case:?}: read {look} must report the same terminal status, got {read}"
        );
    }

    // The guest-credential vend answers a usable session every time it is asked for: a vend is
    // a read of what the exchange produces, not a consumption of it, so a second one for the
    // same identity hands back material that signs a call of its own just as the first does.
    let pool = create_pool(&router, "appsync-settles-pool", &api.arn).await;
    let case = "the guest-credential vend the signed call uses";
    let identity_id = guest_identity(&router, case, &pool.id).await;
    let first = vend_session(&router, case, &identity_id).await;
    let second = vend_session(&router, case, &identity_id).await;

    // And the signed POST on the published endpoint answers the item, which is the step the
    // polls above exist for — once for each session the two vends handed back.
    put_held(&router).await;
    put_role_policy(
        &router,
        &pool.role_name,
        &policy(
            &[
                ("appsync:GraphQL", format!("{}/*", api.arn)),
                ("dynamodb:GetItem", table_arn()),
            ],
            &[],
        ),
    )
    .await;
    let document =
        json!({"query": format!("query {{ callerItem(id: \"{HELD_ID}\") {{ id title }} }}")});
    for (vend, session) in [("the first vend", first), ("the second vend", second)] {
        let case = format!("{case} ({vend})");
        let credential = SignedWith::Vended(session);
        let answered = post_published(&case, &api, Some(&credential), &document).await;
        assert_eq!(
            (
                answered.status,
                answered.document["data"]["callerItem"]["title"].as_str()
            ),
            (200, Some(HELD_TITLE)),
            "case {case:?}: a call signed with this session must be served the item, got {} and \
             {}",
            answered.status,
            answered.document
        );
    }
}

/// What a row's call is OWED: an answer a client can branch on, never silence.
enum Owed {
    /// The field resolves to the item, whose title is this value, and the table holds it.
    Item(&'static str),
    /// The field resolves to nothing, because the table holds no item for the key, and no
    /// error is reported.
    Nothing,
    /// An error of the FIELD carrying this `errorType`, with nothing read and nothing written.
    FieldError(&'static str),
    /// A refusal of the REQUEST: this status and an error document carrying this `errorType`,
    /// with nothing of the API reached.
    Refusal(u16, &'static str),
    /// GraphQL's own validation failure of the document: errors naming the class, no `data`.
    Invalid(&'static str),
}

/// One row: the policy in force on the caller, what it posts, and the answer the call is owed.
struct Row {
    case: &'static str,
    allowed: Vec<(&'static str, String)>,
    denied: Vec<(&'static str, String)>,
    query: String,
    field: &'static str,
    /// Whether the row presents the vended session at all: a call presenting nothing is one
    /// the API admits no caller for, and it is still owed an answer.
    signed: bool,
    owed: Owed,
}

#[tokio::test]
async fn every_call_on_the_caller_credentials_path_is_answered_a_document() {
    let router = build_router();
    create_table(&router).await;
    let data_source_role = create_role(
        &router,
        "appsync-answers-data-source",
        &trust(APPSYNC_PRINCIPAL, None),
        &policy(&[("dynamodb:*", table_arn())], &[]),
    )
    .await;
    let (api, settled) = create_api(&router, "ddb-caller-answers", &data_source_role).await;
    assert_eq!(
        settled, "SUCCESS",
        "setup: the schema creation must be performed, got {settled:?}"
    );
    put_held(&router).await;
    let pool = create_pool(&router, "appsync-answers-pool", &api.arn).await;
    let case = "the rows' guest session";
    let identity_id = guest_identity(&router, case, &pool.id).await;
    let session = vend_session(&router, case, &identity_id).await;
    let credential = SignedWith::Vended(session);
    let graphql = ("appsync:GraphQL", format!("{}/*", api.arn));
    let read = format!("query {{ callerItem(id: \"{HELD_ID}\") {{ id title count }} }}");
    let read_absent = format!("query {{ callerItem(id: \"{ABSENT_ID}\") {{ id title }} }}");
    let write = format!(
        "mutation {{ callerUpdateItem(id: \"{HELD_ID}\", title: \"rewritten\") \
         {{ id title }} }}"
    );
    let undeclared = format!("query {{ callerItem(id: \"{HELD_ID}\") {{ id headline }} }}");

    // The admitted write comes LAST, so the rows reading the held title read what the setup
    // wrote.
    let rows: Vec<Row> = vec![
        Row {
            case: "a read the caller may make",
            allowed: vec![graphql.clone(), ("dynamodb:GetItem", table_arn())],
            denied: Vec::new(),
            query: read.clone(),
            field: "callerItem",
            signed: true,
            owed: Owed::Item(HELD_TITLE),
        },
        Row {
            case: "a read of a key the table holds no item for",
            allowed: vec![graphql.clone(), ("dynamodb:GetItem", table_arn())],
            denied: Vec::new(),
            query: read_absent,
            field: "callerItem",
            signed: true,
            owed: Owed::Nothing,
        },
        Row {
            case: "a read the caller's own permissions withhold",
            allowed: vec![graphql.clone()],
            denied: Vec::new(),
            query: read.clone(),
            field: "callerItem",
            signed: true,
            owed: Owed::FieldError(DYNAMODB_ERROR_TYPE),
        },
        Row {
            case: "a read an explicit deny withholds over a wildcard grant",
            allowed: vec![graphql.clone(), ("dynamodb:*", table_arn())],
            denied: vec![("dynamodb:GetItem", table_arn())],
            query: read.clone(),
            field: "callerItem",
            signed: true,
            owed: Owed::FieldError(DYNAMODB_ERROR_TYPE),
        },
        Row {
            case: "a write the caller's own permissions withhold",
            allowed: vec![graphql.clone(), ("dynamodb:GetItem", table_arn())],
            denied: Vec::new(),
            query: write.clone(),
            field: "callerUpdateItem",
            signed: true,
            owed: Owed::FieldError(DYNAMODB_ERROR_TYPE),
        },
        Row {
            case: "a selection the API's schema does not declare",
            allowed: vec![graphql.clone(), ("dynamodb:*", table_arn())],
            denied: Vec::new(),
            query: undeclared,
            field: "callerItem",
            signed: true,
            owed: Owed::Invalid(VALIDATION_ERROR),
        },
        Row {
            case: "a call no field of which the caller's policies permit",
            allowed: vec![("dynamodb:*", table_arn())],
            denied: Vec::new(),
            query: read.clone(),
            field: "callerItem",
            signed: true,
            owed: Owed::Refusal(401, UNAUTHORIZED_ERROR_TYPE),
        },
        Row {
            case: "a call presenting no credentials at all",
            allowed: vec![graphql.clone(), ("dynamodb:*", table_arn())],
            denied: Vec::new(),
            query: read.clone(),
            field: "callerItem",
            signed: false,
            owed: Owed::Refusal(401, UNAUTHORIZED_ERROR_TYPE),
        },
        Row {
            case: "a write the caller may make",
            allowed: vec![graphql.clone(), ("dynamodb:UpdateItem", table_arn())],
            denied: Vec::new(),
            query: write,
            field: "callerUpdateItem",
            signed: true,
            owed: Owed::Item("rewritten"),
        },
    ];

    for row in &rows {
        put_role_policy(
            &router,
            &pool.role_name,
            &policy(&row.allowed, &row.denied),
        )
        .await;
        let before = held(&router, HELD_ID).await;
        let presented = if row.signed { Some(&credential) } else { None };
        let answered =
            post_published(row.case, &api, presented, &json!({"query": &row.query})).await;
        // Whatever the outcome, the answer is a GraphQL document naming the request it is of:
        // a client correlates it with the call it made, and branches on `data` or `errors`.
        assert!(
            answered.document.is_object(),
            "case {:?}: the endpoint must answer a GraphQL document, got {} and {}",
            row.case,
            answered.status,
            answered.document
        );
        assert!(
            answered.head.to_ascii_lowercase().contains(REQUEST_ID_FIELD),
            "case {:?}: the answer must name the request it is of in {REQUEST_ID_FIELD}, got \
             {:?}",
            row.case,
            answered.head
        );
        let carries_data = answered
            .document
            .get("data")
            .is_some_and(|data| !data.is_null());
        let errors = answered
            .document
            .get("errors")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            carries_data || !errors.is_empty(),
            "case {:?}: the answer must carry data or errors, got {}",
            row.case,
            answered.document
        );
        match row.owed {
            Owed::Item(title) => {
                assert_eq!(
                    (answered.status, errors.is_empty()),
                    (200, true),
                    "case {:?}: a call the caller may make must be served cleanly, got {} and {}",
                    row.case,
                    answered.status,
                    answered.document
                );
                assert_eq!(
                    (
                        answered.document["data"][row.field]["id"].as_str(),
                        answered.document["data"][row.field]["title"].as_str(),
                    ),
                    (Some(HELD_ID), Some(title)),
                    "case {:?}: expected the item the call answered, got {}",
                    row.case,
                    answered.document
                );
                // What the field reported is what that service holds: read back through
                // DynamoDB's own API, so the answer is backed by the owning service.
                let after = held(&router, HELD_ID).await;
                assert_eq!(
                    after
                        .as_ref()
                        .and_then(|item| item["title"]["S"].as_str()),
                    Some(title),
                    "case {:?}: expected the table to hold the title the call answered, got \
                     {after:?}",
                    row.case
                );
            }
            Owed::Nothing => {
                assert_eq!(
                    (
                        answered.status,
                        &answered.document["data"][row.field],
                        errors.is_empty(),
                    ),
                    (200, &Value::Null, true),
                    "case {:?}: an absent item resolves to nothing, got {} and {}",
                    row.case,
                    answered.status,
                    answered.document
                );
                assert_eq!(
                    held(&router, HELD_ID).await,
                    before,
                    "case {:?}: a read of another key leaves the item alone",
                    row.case
                );
            }
            Owed::FieldError(error_type) => {
                assert_eq!(
                    (
                        answered.status,
                        &answered.document["data"][row.field],
                        answered.document["errors"][0]["errorType"].as_str(),
                    ),
                    (200, &Value::Null, Some(error_type)),
                    "case {:?}: expected the field's own refusal, got {} and {}",
                    row.case,
                    answered.status,
                    answered.document
                );
                let after = held(&router, HELD_ID).await;
                assert_eq!(
                    after, before,
                    "case {:?}: a call the permissions withheld reads and writes nothing, got \
                     {after:?} where the table held {before:?}",
                    row.case
                );
            }
            Owed::Refusal(status, error_type) => {
                assert_eq!(
                    (
                        answered.status,
                        answered.document["errors"][0]["errorType"].as_str(),
                    ),
                    (status, Some(error_type)),
                    "case {:?}: expected the API's own refusal, got {} and {}",
                    row.case,
                    answered.status,
                    answered.document
                );
                let after = held(&router, HELD_ID).await;
                assert_eq!(
                    after, before,
                    "case {:?}: a refused request changes nothing, got {after:?} where the \
                     table held {before:?}",
                    row.case
                );
            }
            Owed::Invalid(stated) => {
                let message = answered.document["errors"][0]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert_eq!(
                    (answered.status, answered.document.get("data")),
                    (200, None),
                    "case {:?}: a document the schema rejects is answered its errors and no \
                     data, got {} and {}",
                    row.case,
                    answered.status,
                    answered.document
                );
                assert!(
                    message.contains(stated),
                    "case {:?}: expected a validation failure stating {stated:?}, got \
                     {message:?}",
                    row.case
                );
                let after = held(&router, HELD_ID).await;
                assert_eq!(
                    after, before,
                    "case {:?}: a document that reached no resolver changes nothing, got \
                     {after:?} where the table held {before:?}",
                    row.case
                );
            }
        }
    }
}
