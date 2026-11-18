// SPDX-License-Identifier: CC0-1.0

//! Where the identity-based half of a DynamoDB data-plane decision sits in the order a
//! request is served in: BEFORE the request's keys are read against the table's key schema.
//!
//! RULE pinned here: **a data-plane request of this service is authorized against the
//! policies in force on the principal its access key id speaks for, for
//! `dynamodb:<Action>` on the ARN of the table it addresses, before any value it carries is
//! read against that table's key schema. A principal whose identity-based policies name
//! another table's ARN is therefore refused `AccessDeniedException` (HTTP 400, the status
//! the service framework behind DynamoDB answers an authorization refusal with) whatever
//! else the request got wrong about the addressed table's keys — the key defect is reported
//! only to a caller the policies let through.**
//!
//! That is the diagnosis a client is meant to reach when a role's resource scoping has gone
//! stale (an execution role still naming the table a handler was repointed away from): the
//! call is refused for the scope, not complained about for a key the caller never gets told
//! about.
//!
//! The rows span what the rule generalises over: the OPERATION (every data-plane operation
//! that reads a key — single-item reads and writes, a query, a batched read and a
//! transactional one), and the DEFECT the key carries against the addressed table's schema
//! (an attribute the schema declares no key for, a key attribute of another type, an omitted
//! sort key, a spare attribute beside the key, and a key the schema admits, which carries no
//! defect at all). Each row is taken twice: once as the scoped session, which earns the
//! refusal, and once as the account's own credential, which owns both tables and so reaches
//! the key validation the session never does.
//!
//! Three services have to agree for the rule to hold — IAM holds the role and its inline
//! policy, STS mints the session credential and names its principal, DynamoDB decides the
//! call — so this is the suite that can pin it; a crate-local test can only ever reach one
//! of them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The account every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The Region every request here signs for.
const REGION: &str = "us-east-1";

/// The table the scoped role's policy NAMES, keyed on one string partition key.
const GRANTED_TABLE: &str = "ScopeGranted";

/// The table the scoped role's policy does NOT name, keyed on a numeric partition key and a
/// string sort key — a different key schema, so a key written for [`GRANTED_TABLE`] is no
/// key of this one.
const OTHER_TABLE: &str = "ScopeWithheld";

/// The long-term key every emulator publishes, which is the account's own credential.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The session name every `AssumeRole` here asks for, which is the last segment of the
/// assumed-role ARN a refusal names.
const SESSION: &str = "probe";

/// How long a created table is given to report itself `ACTIVE`, and how often that is looked
/// for. The `CREATING -> ACTIVE` move is owed by the clock, so the setup looks again until
/// the condition holds rather than waiting a fixed time.
const ACTIVE_BUDGET: Duration = Duration::from_secs(120);

/// How long one poll of a created table waits before looking again.
const POLL: Duration = Duration::from_millis(100);

/// A trust policy granting the account root permission to assume a role. It carries none of
/// the characters that are significant in an `application/x-www-form-urlencoded` body.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"AWS":"arn:aws:iam::000000000000:root"},"Action":"sts:AssumeRole"}]}"#
);

/// The three services this rule spans, over ONE engine, as `core/router/src/lib.rs` mounts
/// them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Unsigned requests: the `Authorization` header below still names the Region and the
    // access key id, which is what the service reads the caller's identity from.
    router.set_request_authentication(false);
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
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id` — the key the service reads the caller's identity from.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Post one awsQuery form body to a service as the account's own credential.
async fn query(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(AMBIENT_KEY, signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The text of the first `<name>…</name>` element of an XML body.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// Send one DynamoDB operation as `access_key_id` and answer `(status, body)`.
async fn ddb(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("authorization", authorization(access_key_id, "dynamodb"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The wire error code of a refusal: the shape name its `__type` carries.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The `message` of a refusal, under either casing a client reads it by.
fn message_of(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// Create an IAM role trusted by the account and answer its ARN.
async fn create_role(router: &MultiServiceRouter, role_name: &str) -> String {
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={role_name}\
             &AssumeRolePolicyDocument={TRUST_POLICY}"
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({role_name}) expected 200, got {status} {body}"
    );
    xml_element(&body, "Arn").unwrap_or_else(|| panic!("setup: no <Arn> in {body}"))
}

/// Embed one inline policy in an IAM role.
async fn put_role_policy(router: &MultiServiceRouter, role_name: &str, document: &str) {
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={role_name}\
             &PolicyName=scope-policy&PolicyDocument={document}"
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({role_name}) expected 200, got {status} {body}"
    );
}

/// Assume a role and answer the access key id of the session STS minted.
async fn assume_role(router: &MultiServiceRouter, role_arn: &str) -> String {
    let (status, body) = query(
        router,
        "sts",
        format!("Action=AssumeRole&Version=2011-06-15&RoleArn={role_arn}&RoleSessionName={SESSION}"),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: AssumeRole({role_arn}) expected 200, got {status} {body}"
    );
    xml_element(&body, "AccessKeyId").unwrap_or_else(|| panic!("setup: no <AccessKeyId> in {body}"))
}

/// One identity-based statement, as a document carrying nothing a form body treats
/// specially.
fn policy(action: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"{action}","Resource":"{resource}"}}]}}"#
    )
}

/// The ARN of one table of this account in [`REGION`], in the form AWS documents for a
/// DynamoDB table (`arn:aws:dynamodb:<region>:<account>:table/<name>`).
fn table_arn(name: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{name}")
}

/// The principal an assumed-role session of `role` speaks for, as `sts:GetCallerIdentity`
/// reports it and a refusal names it.
fn session_principal(role: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{SESSION}")
}

/// The sentence AWS refuses a request no identity-based policy allows with.
fn implicit_deny(role: &str, action: &str, table: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action",
        principal = session_principal(role),
        resource = table_arn(table),
    )
}

/// Create one table through DynamoDB's own API as the account's own credential, and look
/// again until that service first reports it `ACTIVE` — "you can perform read and write
/// operations only on an ACTIVE table" (`TableDescription$TableStatus`).
async fn create_active_table(router: &MultiServiceRouter, name: &str, schema: &[(&str, &str)]) {
    let key_schema: Vec<Value> = schema
        .iter()
        .enumerate()
        .map(|(position, (attribute, _))| {
            let role = if position == 0 { "HASH" } else { "RANGE" };
            json!({"AttributeName": attribute, "KeyType": role})
        })
        .collect();
    let definitions: Vec<Value> = schema
        .iter()
        .map(|(attribute, kind)| json!({"AttributeName": attribute, "AttributeType": kind}))
        .collect();
    let (status, created) = ddb(
        router,
        AMBIENT_KEY,
        "CreateTable",
        &json!({
            "TableName": name,
            "KeySchema": key_schema,
            "AttributeDefinitions": definitions,
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateTable({name}) expected 200, got {status} {created}"
    );
    let deadline = Instant::now() + ACTIVE_BUDGET;
    loop {
        let (status, described) =
            ddb(router, AMBIENT_KEY, "DescribeTable", &json!({"TableName": name})).await;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: DynamoDB never reported {name} ACTIVE, last {status} {described}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Both tables, plus a role whose inline policy names [`GRANTED_TABLE`]'s ARN and nothing
/// else, and the access key id of a session of that role.
async fn scoped_session(router: &MultiServiceRouter, role_name: &str) -> String {
    create_active_table(router, GRANTED_TABLE, &[("pk", "S")]).await;
    create_active_table(router, OTHER_TABLE, &[("id", "N"), ("sk", "S")]).await;
    let role_arn = create_role(router, role_name).await;
    put_role_policy(
        router,
        role_name,
        &policy("dynamodb:*", &table_arn(GRANTED_TABLE)),
    )
    .await;
    assume_role(router, &role_arn).await
}

/// One operation row: the data-plane call, the IAM action it performs, a body addressing
/// [`OTHER_TABLE`] with a key that is no key of its schema, and a body addressing
/// [`GRANTED_TABLE`] with one that is.
struct Operation {
    operation: &'static str,
    action: &'static str,
    withheld: Value,
    granted: Value,
    /// The wire error code the key defect earns the caller the policies DO let through: the
    /// request's own `ValidationException` for every operation that refuses the call, and
    /// `TransactionCanceledException` for a transactional element, whose key defect cancels
    /// the transaction with a `ValidationError` cancellation reason rather than refusing the
    /// request.
    owner_refusal: &'static str,
}

/// The key a caller written for [`GRANTED_TABLE`] sends: one that table's own schema admits,
/// and — because [`OTHER_TABLE`] declares no key for that attribute — no key of the table the
/// role's policy withholds.
fn granted_table_key() -> Value {
    json!({"pk": {"S": "k"}})
}

/// The wire error code a request refused against a table's key schema earns.
const VALIDATION_EXCEPTION: &str = "ValidationException";

#[tokio::test]
async fn an_unauthorized_data_plane_call_is_refused_before_its_key_meets_the_schema() {
    let router = build_router();
    let role = "ddb-scope-operations";
    let access_key_id = scoped_session(&router, role).await;

    // One row per data-plane operation that reads a key. The IAM action each performs is the
    // operation's own `dynamodb:<Action>`, except a transactional get, which performs
    // `dynamodb:GetItem` inside `TransactGetItems`.
    let rows = vec![
        Operation {
            operation: "GetItem",
            action: "dynamodb:GetItem",
            withheld: json!({"TableName": OTHER_TABLE, "Key": granted_table_key()}),
            granted: json!({"TableName": GRANTED_TABLE, "Key": granted_table_key()}),
            owner_refusal: VALIDATION_EXCEPTION,
        },
        Operation {
            operation: "PutItem",
            action: "dynamodb:PutItem",
            withheld: json!({"TableName": OTHER_TABLE, "Item": granted_table_key()}),
            granted: json!({"TableName": GRANTED_TABLE, "Item": granted_table_key()}),
            owner_refusal: VALIDATION_EXCEPTION,
        },
        Operation {
            operation: "UpdateItem",
            action: "dynamodb:UpdateItem",
            withheld: json!({
                "TableName": OTHER_TABLE,
                "Key": granted_table_key(),
                "UpdateExpression": "SET v = :v",
                "ExpressionAttributeValues": {":v": {"S": "x"}},
            }),
            granted: json!({
                "TableName": GRANTED_TABLE,
                "Key": granted_table_key(),
                "UpdateExpression": "SET v = :v",
                "ExpressionAttributeValues": {":v": {"S": "x"}},
            }),
            owner_refusal: VALIDATION_EXCEPTION,
        },
        Operation {
            operation: "DeleteItem",
            action: "dynamodb:DeleteItem",
            withheld: json!({"TableName": OTHER_TABLE, "Key": granted_table_key()}),
            granted: json!({"TableName": GRANTED_TABLE, "Key": granted_table_key()}),
            owner_refusal: VALIDATION_EXCEPTION,
        },
        Operation {
            operation: "Query",
            action: "dynamodb:Query",
            withheld: json!({
                "TableName": OTHER_TABLE,
                "KeyConditionExpression": "pk = :p",
                "ExpressionAttributeValues": {":p": {"S": "k"}},
            }),
            granted: json!({
                "TableName": GRANTED_TABLE,
                "KeyConditionExpression": "pk = :p",
                "ExpressionAttributeValues": {":p": {"S": "k"}},
            }),
            owner_refusal: VALIDATION_EXCEPTION,
        },
        Operation {
            operation: "BatchGetItem",
            action: "dynamodb:BatchGetItem",
            withheld: json!({"RequestItems": {OTHER_TABLE: {"Keys": [granted_table_key()]}}}),
            granted: json!({"RequestItems": {GRANTED_TABLE: {"Keys": [granted_table_key()]}}}),
            owner_refusal: VALIDATION_EXCEPTION,
        },
        Operation {
            operation: "TransactGetItems",
            action: "dynamodb:GetItem",
            withheld: json!({
                "TransactItems": [{"Get": {"TableName": OTHER_TABLE, "Key": granted_table_key()}}],
            }),
            granted: json!({
                "TransactItems": [{"Get": {"TableName": GRANTED_TABLE, "Key": granted_table_key()}}],
            }),
            owner_refusal: "TransactionCanceledException",
        },
    ];

    for Operation {
        operation,
        action,
        withheld,
        granted,
        owner_refusal,
    } in rows
    {
        // The scoped session: refused for the SCOPE, naming the action and the table it
        // addressed, with nothing said about the key.
        let (status, answered) = ddb(&router, &access_key_id, operation, &withheld).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "AccessDeniedException"),
            "case {operation:?}: a table the role's policy does not name is refused for the \
             scope, got {status} with {answered}"
        );
        assert_eq!(
            message_of(&answered),
            implicit_deny(role, action, OTHER_TABLE),
            "case {operation:?}: expected the refusal to be built from the request, got {:?}",
            message_of(&answered)
        );

        // The same request as the account's own credential, which owns both tables: it
        // reaches the key validation the session never did, so the key defect the session was
        // NOT told about is what this caller is refused for.
        let (status, owner) = ddb(&router, AMBIENT_KEY, operation, &withheld).await;
        assert_eq!(
            (status, error_code(&owner).as_str()),
            (400, owner_refusal),
            "case {operation:?}: the authorized owner is the caller that reaches the key \
             validation, got {status} with {owner}"
        );

        // And the table the role's policy DOES name is served it, so the refusal above is
        // about the resource the request addressed and not about the operation.
        let (status, served) = ddb(&router, &access_key_id, operation, &granted).await;
        assert_eq!(
            status, 200,
            "case {operation:?}: the table the role's policy names is served, got {status} \
             with {served}"
        );
    }
}

/// One key row: what the `Key` carries against [`OTHER_TABLE`]'s schema (`id` of type `N`,
/// `sk` of type `S`), and whether that key is one the schema admits.
struct Key {
    case: &'static str,
    key: Value,
    /// Whether the account's own credential — which the policies let through — is SERVED the
    /// read, which is so exactly for the key that carries no defect.
    served_to_the_owner: bool,
}

#[tokio::test]
async fn the_refusal_is_the_same_whatever_the_key_got_wrong() {
    let router = build_router();
    let role = "ddb-scope-keys";
    let access_key_id = scoped_session(&router, role).await;

    let rows = vec![
        Key {
            case: "an attribute the schema declares no key for",
            key: json!({"pk": {"S": "k"}}),
            served_to_the_owner: false,
        },
        Key {
            case: "the partition key in another type than the schema declares",
            key: json!({"id": {"S": "1"}, "sk": {"S": "s"}}),
            served_to_the_owner: false,
        },
        Key {
            case: "no value for the sort key the schema declares",
            key: json!({"id": {"N": "1"}}),
            served_to_the_owner: false,
        },
        Key {
            case: "a spare attribute beside the key",
            key: json!({"id": {"N": "1"}, "sk": {"S": "s"}, "spare": {"S": "x"}}),
            served_to_the_owner: false,
        },
        Key {
            case: "a key the schema admits, which carries no defect at all",
            key: json!({"id": {"N": "1"}, "sk": {"S": "s"}}),
            served_to_the_owner: true,
        },
    ];

    for Key {
        case,
        key,
        served_to_the_owner,
    } in rows
    {
        let body = json!({"TableName": OTHER_TABLE, "Key": key});
        let (status, answered) = ddb(&router, &access_key_id, "GetItem", &body).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "AccessDeniedException"),
            "case {case:?}: the scope decides the call whatever the key carries, got {status} \
             with {answered}"
        );
        assert_eq!(
            message_of(&answered),
            implicit_deny(role, "dynamodb:GetItem", OTHER_TABLE),
            "case {case:?}: expected one refusal for every key, got {:?}",
            message_of(&answered)
        );

        // The owner's answer is what the key itself earns: the defect for the four keys that
        // carry one, and the read for the one that does not — which is the validation the
        // scoped session never reached.
        let (status, owner) = ddb(&router, AMBIENT_KEY, "GetItem", &body).await;
        if served_to_the_owner {
            assert_eq!(
                (status, owner.get("Item")),
                (200, None),
                "case {case:?}: the owner reads the admitted key and finds no item, got \
                 {status} with {owner}"
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&owner).as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case:?}: the owner is the caller the key defect is reported to, got {status} \
             with {owner}"
        );
    }
}
