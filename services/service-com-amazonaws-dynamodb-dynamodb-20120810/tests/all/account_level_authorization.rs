// SPDX-License-Identifier: CC0-1.0

//! Authorization of the calls that address the account's TABLE NAMESPACE rather than one
//! table.
//!
//! RULE pinned here: **a request of this service that addresses no single table — a listing
//! of the account's table resources, or a report of the account's own quotas — is authorized
//! before it is served, against the identity-based policies in force on the principal its
//! access key id speaks for, for `dynamodb:<Action>` on the table namespace it reports from
//! (`arn:aws:dynamodb:<region>:<account>:table/*`, or the one table the request narrows
//! itself to). An explicit `Deny` refuses it whatever any grant says; otherwise it needs an
//! `Allow` about it, and with none it is refused — IAM's implicit deny. The refusal is
//! `AccessDeniedException` with HTTP 400 (the status the service framework behind DynamoDB
//! answers an authorization refusal with) on this service's own `awsJson1_0` surface, worded
//! from the request: the principal, the action and the resource.**
//!
//! The rows span what the rule generalises over: the OPERATION (every one of the six that
//! addresses no single table), what the caller's own policy says (nothing at all, another
//! service's action, another action of this service, exactly this action, a wildcard covering
//! it, this action on another table's ARN, an explicit deny over a wildcard allow), and WHICH
//! resource the call addresses (the namespace, or the single table a listing filters to).
//!
//! Driven over the wire through one `MultiServiceRouter` serving DynamoDB beside IAM and STS
//! on ONE shared engine — the role lives in IAM's store and its session credential is minted
//! by STS, exactly as `core/router` assembles the three.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account every generated emulator presents.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which is the account's own credential.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The session name every `AssumeRole` here asks for, which is the last segment of the
/// assumed-role ARN a refusal names.
const SESSION: &str = "probe";

/// The policy-language version every document here states.
const POLICY_VERSION: &str = "2012-10-17";

/// A trust policy letting the account assume the role.
fn trust_policy() -> String {
    json!({
        "Version": POLICY_VERSION,
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Every operation of this service that addresses no single table, with the IAM action it
/// performs: the class the rule above is about.
///
/// The names and their actions are this service's own operation names
/// (`smithy_query(query="list_operations")`), each one whose input names no resource of its
/// own beside an optional filter.
const ACCOUNT_LEVEL: &[(&str, &str)] = &[
    ("ListTables", "dynamodb:ListTables"),
    ("DescribeLimits", "dynamodb:DescribeLimits"),
    ("ListBackups", "dynamodb:ListBackups"),
    ("ListExports", "dynamodb:ListExports"),
    ("ListImports", "dynamodb:ListImports"),
    ("ListContributorInsights", "dynamodb:ListContributorInsights"),
];

/// One row: the policy the role carries, and which of the operations it leaves served.
struct Row {
    case: &'static str,
    role: &'static str,
    /// The role's inline policy, or `None` for a role the account grants nothing.
    document: Option<String>,
    /// The operations this row expects to be SERVED.
    served: &'static [&'static str],
    /// The operations this row expects to be refused by an EXPLICIT deny, which is worded
    /// differently from the implicit one.
    explicitly_denied: &'static [&'static str],
}

/// One row of the narrowed-listing suite: the policy the role carries, the table the
/// listing filters to (or none, which addresses the whole namespace), and the status the
/// call then earns.
struct NarrowedRow {
    case: &'static str,
    role: &'static str,
    document: Option<String>,
    filter: Option<&'static str>,
    expected: u16,
}

/// The emulator the rows drive: DynamoDB, IAM and STS over ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Unsigned requests: the `Authorization` header below still names the Region and the
    // access key id, which is what the service reads the caller's identity from.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id`.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
    )
}

/// Send one DynamoDB operation as `access_key_id`, answering the whole HTTP response.
async fn ddb_response(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: Value,
) -> http::Response<Bytes> {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("authorization", authorization(access_key_id, "dynamodb"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    router.dispatch(request).await
}

/// [`ddb_response`] as `(status, parsed body)`.
async fn ddb(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let response = ddb_response(router, access_key_id, operation, body).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// Post one awsQuery form body to `signing_name` as the account's own credential.
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

/// Percent-encode one `application/x-www-form-urlencoded` component through the maintained
/// encoder rather than by hand.
fn form_encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The text of the first `<name>…</name>` element of an XML body.
fn element(case: &str, body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let (_, rest) = body
        .split_once(&open)
        .unwrap_or_else(|| panic!("case {case:?}: the answer must carry <{name}>, got {body}"));
    let (text, _) = rest
        .split_once(&close)
        .unwrap_or_else(|| panic!("case {case:?}: <{name}> must be closed, got {body}"));
    text.to_string()
}

/// Create one IAM role trusted by the account and answer its ARN.
async fn create_role(router: &MultiServiceRouter, case: &str, role: &str) -> String {
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={}&AssumeRolePolicyDocument={}",
            form_encode(role),
            form_encode(&trust_policy())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole({role}) must answer 200, got {status} with {body}"
    );
    element(case, &body, "Arn")
}

/// Embed one inline policy in `role`.
async fn put_role_policy(router: &MultiServiceRouter, case: &str, role: &str, document: &str) {
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={}&PolicyName=probe&PolicyDocument={}",
            form_encode(role),
            form_encode(document)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy({role}) must answer 200, got {status} with {body}"
    );
}

/// Assume `role_arn` and answer the access key id of the session STS minted.
async fn assume_role(router: &MultiServiceRouter, case: &str, role_arn: &str) -> String {
    let (status, body) = query(
        router,
        "sts",
        format!(
            "Action=AssumeRole&Version=2011-06-15&RoleArn={}&RoleSessionName={SESSION}",
            form_encode(role_arn)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssumeRole({role_arn}) must answer 200, got {status} with {body}"
    );
    element(case, &body, "AccessKeyId")
}

/// One identity-based statement list, as a document.
fn policy(statements: Value) -> String {
    json!({"Version": POLICY_VERSION, "Statement": statements}).to_string()
}

/// One `Allow` of `action` on `resource`.
fn allow(action: &str, resource: &str) -> String {
    policy(json!([{"Effect": "Allow", "Action": action, "Resource": resource}]))
}

/// The ARN of the account's whole table namespace in [`REGION`] — the resource a call that
/// addresses no single table acts on, in the ARN format AWS documents for a DynamoDB table
/// (`arn:aws:dynamodb:<region>:<account>:table/<name>`).
fn table_namespace() -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/*")
}

/// The ARN of one table of this account in [`REGION`].
fn table_arn(name: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{name}")
}

/// The principal an assumed-role session of `role` speaks for, as `sts:GetCallerIdentity`
/// reports it and a refusal names it.
fn session_principal(role: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{SESSION}")
}

/// The sentence AWS refuses an unauthorized request with.
fn refusal(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!("User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}")
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

#[tokio::test]
async fn an_account_level_call_is_served_only_where_the_callers_policies_allow_it() {
    let router = emulator();
    let rows = vec![
        Row {
            case: "a role the account grants nothing",
            role: "ddb-account-none",
            document: None,
            served: &[],
            explicitly_denied: &[],
        },
        Row {
            case: "another service's action",
            role: "ddb-account-s3",
            document: Some(allow("s3:ListAllMyBuckets", "*")),
            served: &[],
            explicitly_denied: &[],
        },
        Row {
            case: "another action of this service",
            role: "ddb-account-getitem",
            document: Some(allow("dynamodb:GetItem", "*")),
            served: &[],
            explicitly_denied: &[],
        },
        Row {
            case: "exactly one of the actions, on the table namespace",
            role: "ddb-account-exact",
            document: Some(allow("dynamodb:ListTables", &table_namespace())),
            served: &["ListTables"],
            explicitly_denied: &[],
        },
        Row {
            case: "a wildcard covering every action and resource",
            role: "ddb-account-wildcard",
            document: Some(allow("dynamodb:*", "*")),
            served: &[
                "ListTables",
                "DescribeLimits",
                "ListBackups",
                "ListExports",
                "ListImports",
                "ListContributorInsights",
            ],
            explicitly_denied: &[],
        },
        Row {
            case: "the action on another table's ARN, which the namespace is not",
            role: "ddb-account-other-table",
            document: Some(allow("dynamodb:ListBackups", &table_arn("Elsewhere"))),
            served: &[],
            explicitly_denied: &[],
        },
        Row {
            case: "an explicit deny over a wildcard allow",
            role: "ddb-account-denied",
            document: Some(policy(json!([
                {"Effect": "Allow", "Action": "dynamodb:*", "Resource": "*"},
                {"Effect": "Deny", "Action": "dynamodb:ListTables", "Resource": "*"},
            ]))),
            served: &[
                "DescribeLimits",
                "ListBackups",
                "ListExports",
                "ListImports",
                "ListContributorInsights",
            ],
            explicitly_denied: &["ListTables"],
        },
    ];

    for row in rows {
        let case = row.case;
        let role_arn = create_role(&router, case, row.role).await;
        if let Some(document) = &row.document {
            put_role_policy(&router, case, row.role, document).await;
        }
        let access_key_id = assume_role(&router, case, &role_arn).await;
        let principal = session_principal(row.role);

        for (operation, action) in ACCOUNT_LEVEL {
            let (status, answered) = ddb(&router, &access_key_id, operation, json!({})).await;
            if row.served.contains(operation) {
                assert_eq!(
                    status, 200,
                    "case {case:?} / {operation}: the policy allows {action}, so the call is \
                     served, got {status} with {answered}"
                );
                continue;
            }
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "AccessDeniedException"),
                "case {case:?} / {operation}: expected the denial of {action}, got {status} with \
                 {answered}"
            );
            let reason = if row.explicitly_denied.contains(operation) {
                "with an explicit deny in an identity-based policy".to_string()
            } else {
                format!("because no identity-based policy allows the {action} action")
            };
            assert_eq!(
                message_of(&answered),
                refusal(&principal, action, &table_namespace(), &reason),
                "case {case:?} / {operation}: expected the refusal to be built from the request, \
                 got {:?}",
                message_of(&answered)
            );

            // Whatever the row, the account's own credential keeps being served: it holds no
            // IAM policies and the account owns its own resources.
            let (status, owner) = ddb(&router, AMBIENT_KEY, operation, json!({})).await;
            assert_eq!(
                status, 200,
                "case {case:?} / {operation}: the account's own credential keeps being served, \
                 got {status} with {owner}"
            );
        }
    }
}

#[tokio::test]
async fn a_listing_narrowed_to_one_table_is_authorized_on_that_tables_arn() {
    let router = emulator();
    let table = "Narrowed";
    let (status, created) = ddb(
        &router,
        AMBIENT_KEY,
        "CreateTable",
        json!({
            "TableName": table,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the account's own credential creates the table, got {status} with {created}"
    );

    let rows = vec![
        NarrowedRow {
            case: "a role the account grants nothing, narrowed to the table",
            role: "ddb-narrowed-none",
            document: None,
            filter: Some(table),
            expected: 400,
        },
        NarrowedRow {
            case: "a grant on that one table's ARN, narrowed to it",
            role: "ddb-narrowed-table",
            document: Some(allow("dynamodb:ListBackups", &table_arn(table))),
            filter: Some(table),
            expected: 200,
        },
        NarrowedRow {
            case: "the same grant, over the whole namespace instead",
            role: "ddb-narrowed-namespace",
            document: Some(allow("dynamodb:ListBackups", &table_arn(table))),
            filter: None,
            expected: 400,
        },
    ];

    for NarrowedRow {
        case,
        role,
        document,
        filter,
        expected,
    } in rows
    {
        let role_arn = create_role(&router, case, role).await;
        if let Some(document) = &document {
            put_role_policy(&router, case, role, document).await;
        }
        let access_key_id = assume_role(&router, case, &role_arn).await;
        let mut body = json!({});
        if let Some(filter) = filter {
            body["TableName"] = json!(filter);
        }
        let (status, answered) = ddb(&router, &access_key_id, "ListBackups", body.clone()).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected HTTP {expected} for {body}, got {status} with {answered}"
        );
        if expected == 400 {
            // The resource the refusal names is what the call addressed: the one table it
            // filters to, or the namespace when it names none.
            let resource = filter.map_or_else(table_namespace, table_arn);
            assert_eq!(
                message_of(&answered),
                refusal(
                    &session_principal(role),
                    "dynamodb:ListBackups",
                    &resource,
                    "because no identity-based policy allows the dynamodb:ListBackups action",
                ),
                "case {case:?}: the refusal names the resource the call addressed, got {:?}",
                message_of(&answered)
            );
        }
    }
}

#[tokio::test]
async fn the_denial_takes_this_services_own_json_error_shape() {
    let router = emulator();
    let case = "the denial's wire shape";
    let role = "ddb-shape-none";
    let role_arn = create_role(&router, case, role).await;
    let access_key_id = assume_role(&router, case, &role_arn).await;

    let response = ddb_response(&router, &access_key_id, "ListTables", json!({"Limit": 1})).await;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("<none>")
        .to_string();
    let query_error = response.headers().get("x-amzn-query-error").cloned();
    let rendered = String::from_utf8_lossy(response.body()).to_string();

    assert_eq!(
        status, 400,
        "case {case:?}: an implicit deny is HTTP 400 on this service, got {status} with {rendered}"
    );
    assert_eq!(
        content_type, "application/x-amz-json-1.0",
        "case {case:?}: the body is this service's own JSON media type, got {content_type:?} with \
         {rendered}"
    );
    assert!(
        rendered.contains("\"__type\""),
        "case {case:?}: an awsJson1_0 error names its shape in __type, got {rendered}"
    );
    assert!(
        query_error.is_none(),
        "case {case:?}: awsJson1_0 carries no x-amzn-query-error header, got {query_error:?}"
    );
    let parsed: Value = serde_json::from_str(&rendered).unwrap_or(Value::Null);
    assert_eq!(
        error_code(&parsed),
        "AccessDeniedException",
        "case {case:?}: expected the authorization refusal's own code, got {rendered}"
    );
}
