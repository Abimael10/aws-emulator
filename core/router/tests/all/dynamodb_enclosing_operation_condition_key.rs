// SPDX-License-Identifier: CC0-1.0

//! WHICH operation a DynamoDB data-plane action is performed inside, as the condition key
//! that reports it: three services, one decision.
//!
//! RULE: a call that is authorized as the action one of its ENTRIES asks for publishes the
//! operation enclosing that entry as `dynamodb:EnclosingOperation` — a PartiQL statement
//! run as one statement of a transaction or of a batch, and an item written, checked or
//! read as one item of a transactional write or read, each naming the operation it is an
//! entry of — while a call that performs an action of ITS OWN carries no value for the key
//! and answers that it carries none. A grant conditioned on the key therefore reaches
//! exactly the calls whose enclosing operation it names, and IAM's own rules for a key the
//! request carries no value for decide the rest: an unqualified comparison does not hold, a
//! negated one and the `…IfExists` spelling do, and `Null` reports the key as absent. With
//! no grant the call is refused `AccessDeniedException` (HTTP 400) naming the principal, the
//! action and the table, and writes nothing.
//!
//! Three services have to agree for that to hold (IAM holds the role and its conditioned
//! grant, STS mints the session credential and names its principal, DynamoDB publishes the
//! key and makes the decision), so this is the suite that can pin it; a crate-local test
//! reaches only one of them.
//!
//! The rows span what the rule generalises over: which enclosing operation the clause names
//! (the two PartiQL ones and the two transactional item ones), the operator the clause is
//! written with (`StringEquals`, its negated and `…IfExists` spellings, `Null`, and no
//! `Condition` at all as the control), the action being authorized (`dynamodb:PartiQLInsert`
//! inside and outside an enclosing operation, `dynamodb:PutItem`, `dynamodb:GetItem`), and
//! whether the probe performs an action of its own or is one entry of an enclosing operation.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The account every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The Region every request below signs for, which is where the table lives.
const REGION: &str = "us-east-1";

/// The table every probe acts on.
const TABLE: &str = "EnclosedWrites";

/// The long-term key every emulator publishes, which is the account's own credential: the
/// setup and the read-backs are made with it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The role every probe below speaks for a session of.
const ROLE: &str = "ddb-enclosing-operation-role";

/// The session name the mint asks for, which is the last segment of the assumed-role ARN a
/// refusal names.
const SESSION: &str = "probe";

/// The name the one inline policy of that role is embedded under, replaced per row.
const POLICY_NAME: &str = "enclosing-operation";

/// The partition holding the item every READ probe asks for.
const SEEDED_KEY: &str = "seeded";

/// The attribute that item carries, which a served read reports.
const SEEDED_VALUE: &str = "seeded-value";

/// How often to look while waiting for work the clock owes — a created table reaching
/// `ACTIVE` — and how many looks to take. The span is the service's own, so no fixed sleep
/// names the instant it ends, and the deadline is generous so a loaded host cannot decide
/// the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The three services this rule spans, over ONE engine.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is where a service reads the calling principal and the Region.
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
/// credential is `access_key_id` — the key a service reads the caller's identity from.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `signing_name`, made as the account's own credential.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(AMBIENT_KEY, signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One DynamoDB operation over its `awsJson1_0` wire, signed as `access_key_id`.
async fn ddb(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("authorization", authorization(access_key_id, "dynamodb"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The wire error code (`__type`) of a refusal.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The `message`/`Message` member of a refusal, whichever casing it carries.
fn message_of(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// The ARN of the table every statement names and every refusal reports.
fn table_arn() -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}")
}

/// Create the table the probes act on, wait until DynamoDB reports it `ACTIVE` ("you can
/// perform read and write operations only on an ACTIVE table"), and seed the item the read
/// probes ask for.
async fn seed(router: &MultiServiceRouter) {
    let (status, created) = ddb(
        router,
        AMBIENT_KEY,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateTable({TABLE:?}) must succeed, got {status} with {created}"
    );
    let mut described = Value::Null;
    for _ in 0..LOOKS {
        let (_, answered) = ddb(
            router,
            AMBIENT_KEY,
            "DescribeTable",
            &json!({"TableName": TABLE}),
        )
        .await;
        described = answered;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        described["Table"]["TableStatus"].as_str(),
        Some("ACTIVE"),
        "setup: DynamoDB must report {TABLE:?} ACTIVE, got {described}"
    );
    assert_eq!(
        described["Table"]["TableArn"].as_str(),
        Some(table_arn().as_str()),
        "setup: the table's ARN is what a statement and a refusal name, got {described}"
    );
    let (status, put) = ddb(
        router,
        AMBIENT_KEY,
        "PutItem",
        &json!({
            "TableName": TABLE,
            "Item": {"pk": {"S": SEEDED_KEY}, "v": {"S": SEEDED_VALUE}},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutItem({SEEDED_KEY:?}) must succeed, got {status} with {put}"
    );
}

/// Create the role every probe speaks for a session of, trusted by the account root, and
/// return its ARN. It carries no policy yet: each row embeds its own.
async fn create_role(router: &MultiServiceRouter) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", ROLE), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({ROLE:?}) must succeed, got {status} with {created}"
    );
    element(&created, "Arn")
}

/// Embed `document` as the role's one inline policy, replacing whatever the previous row
/// embedded: the documents in force are read at the instant of each call, so one session
/// serves every row and the principal a refusal names stays the same.
async fn put_role_policy(router: &MultiServiceRouter, case: &str, document: &str) {
    let (status, body) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy must succeed, got {status} with {body}"
    );
}

/// Assume the role, returning the access key id a probe signs with and the `AssumedRoleUser`
/// ARN STS reported for it — the principal a refusal names.
async fn assume(router: &MultiServiceRouter, role_arn: &str) -> (String, String) {
    let (status, body) = query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[("RoleArn", role_arn), ("RoleSessionName", SESSION)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: AssumeRole({role_arn}) must succeed, got {status} with {body}"
    );
    // The first `<Arn>` of the answer is the `AssumedRoleUser`'s, which is the identity
    // `sts:GetCallerIdentity` reports for this credential.
    (element(&body, "AccessKeyId"), element(&body, "Arn"))
}

/// The three actions every row's statement grants, so what decides a probe is the row's
/// `Condition` and never a missing action: the PartiQL verb an `INSERT` performs, and the
/// two single-item actions the transactional write and read are the transactional forms of.
const GRANTED_ACTIONS: [&str; 3] = [
    "dynamodb:PartiQLInsert",
    "dynamodb:PutItem",
    "dynamodb:GetItem",
];

/// The row's identity-based document: the three actions on this table, under `condition`
/// when the row states one.
fn document(condition: Option<Value>) -> String {
    let mut statement = json!({
        "Effect": "Allow",
        "Action": GRANTED_ACTIONS,
        "Resource": table_arn(),
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({"Version": "2012-10-17", "Statement": [statement]}).to_string()
}

/// One surface the same INSERT, put or read arrives through: an operation performing an
/// action of its own, or one entry of an operation that encloses it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Surface {
    /// One PartiQL statement, which is an operation of its own.
    Statement,
    /// One statement of a PartiQL transaction.
    Transaction,
    /// One statement of a PartiQL batch.
    Batch,
    /// A single-item put, which is an operation of its own.
    Put,
    /// One put of a transactional write.
    TransactWrite,
    /// A single-item read, which is an operation of its own.
    Get,
    /// One get of a transactional read.
    TransactGet,
}

impl Surface {
    /// The operation the probe calls.
    fn operation(self) -> &'static str {
        match self {
            Self::Statement => "ExecuteStatement",
            Self::Transaction => "ExecuteTransaction",
            Self::Batch => "BatchExecuteStatement",
            Self::Put => "PutItem",
            Self::TransactWrite => "TransactWriteItems",
            Self::Get => "GetItem",
            Self::TransactGet => "TransactGetItems",
        }
    }

    /// The IAM action the probe is authorized as, which is what its refusal names.
    fn action(self) -> &'static str {
        match self {
            Self::Statement | Self::Transaction | Self::Batch => "dynamodb:PartiQLInsert",
            Self::Put | Self::TransactWrite => "dynamodb:PutItem",
            Self::Get | Self::TransactGet => "dynamodb:GetItem",
        }
    }

    /// Whether the probe stores an item at the key it is given, rather than reading the
    /// seeded one.
    fn writes(self) -> bool {
        !matches!(self, Self::Get | Self::TransactGet)
    }

    /// Whether a refusal is reported per ENTRY rather than for the call: a batch answers
    /// each statement in its own slot.
    fn per_entry(self) -> bool {
        self == Self::Batch
    }

    /// The request the probe sends: a write stores an item at `key`, a read asks for the
    /// seeded one.
    fn request(self, key: &str) -> Value {
        let insert = format!("INSERT INTO {TABLE} VALUE {{'pk': '{key}'}}");
        let item = json!({"pk": {"S": key}});
        let seeded = json!({"pk": {"S": SEEDED_KEY}});
        match self {
            Self::Statement => json!({"Statement": insert}),
            Self::Transaction => json!({"TransactStatements": [{"Statement": insert}]}),
            Self::Batch => json!({"Statements": [{"Statement": insert}]}),
            Self::Put => json!({"TableName": TABLE, "Item": item}),
            Self::TransactWrite => {
                json!({"TransactItems": [{"Put": {"TableName": TABLE, "Item": item}}]})
            }
            Self::Get => json!({"TableName": TABLE, "Key": seeded}),
            Self::TransactGet => {
                json!({"TransactItems": [{"Get": {"TableName": TABLE, "Key": seeded}}]})
            }
        }
    }

    /// The item a SERVED read reports, read out of this surface's answer.
    fn served_item(self, answer: &Value) -> Value {
        match self {
            Self::Get => answer["Item"].clone(),
            Self::TransactGet => answer["Responses"][0]["Item"].clone(),
            _ => Value::Null,
        }
    }
}

/// Every surface, which every row states an expectation for.
const SURFACES: [Surface; 7] = [
    Surface::Statement,
    Surface::Transaction,
    Surface::Batch,
    Surface::Put,
    Surface::TransactWrite,
    Surface::Get,
    Surface::TransactGet,
];

/// One row: the clause the row's grant carries, and the surfaces it admits — every other
/// surface is refused.
struct Row {
    case: &'static str,
    condition: Option<Value>,
    admitted: &'static [Surface],
}

impl std::fmt::Debug for Row {
    /// A row quotes itself by its case, which is what every assertion below names it by.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.case)
    }
}

/// The refusal a probe earned, as `(code, message)`, or `None` when it was served, beside
/// the answer itself.
async fn refusal(
    router: &MultiServiceRouter,
    access_key_id: &str,
    case: &str,
    surface: Surface,
    key: &str,
) -> (Option<(String, String)>, Value) {
    let (status, answered) = ddb(
        router,
        access_key_id,
        surface.operation(),
        &surface.request(key),
    )
    .await;
    if surface.per_entry() {
        assert_eq!(
            status, 200,
            "case {case:?} / {}: a batch answers each statement in its own slot, got \
             {status} {answered}",
            surface.operation()
        );
        let slot = answered["Responses"][0].clone();
        let refused = slot
            .get("Error")
            .filter(|error| !error.is_null())
            .map(|error| {
                (
                    error["Code"].as_str().unwrap_or("<no Code>").to_string(),
                    error["Message"]
                        .as_str()
                        .unwrap_or("<no Message>")
                        .to_string(),
                )
            });
        return (refused, answered);
    }
    if status == 200 {
        return (None, answered);
    }
    (Some((error_code(&answered), message_of(&answered))), answered)
}

/// The item `key` holds, read back through the account's own credential, or `Null` for a key
/// holding none.
async fn stored(router: &MultiServiceRouter, key: &str) -> Value {
    let (status, read) = ddb(
        router,
        AMBIENT_KEY,
        "GetItem",
        &json!({"TableName": TABLE, "Key": {"pk": {"S": key}}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the account's own credential reads its table, got {status} {read}"
    );
    read.get("Item").cloned().unwrap_or(Value::Null)
}

#[tokio::test]
async fn a_grant_conditioned_on_the_enclosing_operation_reaches_only_the_calls_inside_it() {
    let router = build_router();
    seed(&router).await;
    let role_arn = create_role(&router).await;
    let (access_key_id, principal) = assume(&router, &role_arn).await;
    let arn = table_arn();

    let rows: Vec<Row> = vec![
        Row {
            // The clause AWS documents for admitting a PartiQL write only as part of a
            // transaction, which is the form the recorded case was written with.
            case: "StringEquals naming the PartiQL transaction",
            condition: Some(
                json!({"StringEquals": {"dynamodb:EnclosingOperation": "ExecuteTransaction"}}),
            ),
            admitted: &[Surface::Transaction],
        },
        Row {
            case: "StringEquals naming the PartiQL batch",
            condition: Some(
                json!({"StringEquals": {"dynamodb:EnclosingOperation": "BatchExecuteStatement"}}),
            ),
            admitted: &[Surface::Batch],
        },
        Row {
            case: "StringEquals naming the transactional write",
            condition: Some(
                json!({"StringEquals": {"dynamodb:EnclosingOperation": "TransactWriteItems"}}),
            ),
            admitted: &[Surface::TransactWrite],
        },
        Row {
            case: "StringEquals naming the transactional read",
            condition: Some(
                json!({"StringEquals": {"dynamodb:EnclosingOperation": "TransactGetItems"}}),
            ),
            admitted: &[Surface::TransactGet],
        },
        Row {
            // "If the key is not present … a negated condition operator returns true": the
            // calls that perform an action of their own are exactly the ones it admits,
            // beside every enclosed call whose operation is not the one it names.
            case: "StringNotEquals naming the PartiQL transaction",
            condition: Some(
                json!({"StringNotEquals": {"dynamodb:EnclosingOperation": "ExecuteTransaction"}}),
            ),
            admitted: &[
                Surface::Statement,
                Surface::Batch,
                Surface::Put,
                Surface::TransactWrite,
                Surface::Get,
                Surface::TransactGet,
            ],
        },
        Row {
            // The `…IfExists` spelling holds for a call that carries no value for the key,
            // so it admits the enclosed calls it names AND every unenclosed one.
            case: "StringEqualsIfExists naming the PartiQL transaction",
            condition: Some(json!({
                "StringEqualsIfExists": {"dynamodb:EnclosingOperation": "ExecuteTransaction"},
            })),
            admitted: &[
                Surface::Statement,
                Surface::Transaction,
                Surface::Put,
                Surface::Get,
            ],
        },
        Row {
            // `Null` asks about PRESENCE alone: asserting the key is absent admits exactly
            // the calls that perform an action of their own.
            case: "Null asserting no enclosing operation",
            condition: Some(json!({"Null": {"dynamodb:EnclosingOperation": "true"}})),
            admitted: &[Surface::Statement, Surface::Put, Surface::Get],
        },
        Row {
            // The control: the same grant with no clause at all admits every surface, which
            // is what shows the clause — and not a missing action or a broken read path — is
            // what scoped the rows above.
            case: "the same grant with no condition at all",
            condition: None,
            admitted: &SURFACES,
        },
    ];

    for (at, row) in rows.iter().enumerate() {
        put_role_policy(&router, row.case, &document(row.condition.clone())).await;
        for surface in SURFACES {
            let admitted = row.admitted.contains(&surface);
            // Each probe writes its own key, so an `INSERT` never meets an item a previous
            // row stored: an `INSERT` of a key the table already holds is a duplicate.
            let key = format!("{at}-{}", surface.operation());
            let (refused, answered) =
                refusal(&router, &access_key_id, row.case, surface, &key).await;
            let action = surface.action();
            if admitted {
                assert_eq!(
                    refused, None,
                    "case {row:?} / {}: the clause names this call's enclosing operation, \
                     so the grant reaches it, got {answered}",
                    surface.operation()
                );
                if surface.writes() {
                    // The write LANDED: read back through the account's own credential.
                    assert_eq!(
                        stored(&router, &key).await,
                        json!({"pk": {"S": key}}),
                        "case {row:?} / {}: the admitted write stores its item, got \
                         {answered}",
                        surface.operation()
                    );
                } else {
                    assert_eq!(
                        surface.served_item(&answered),
                        json!({"pk": {"S": SEEDED_KEY}, "v": {"S": SEEDED_VALUE}}),
                        "case {row:?} / {}: the admitted read reports the seeded item, got \
                         {answered}",
                        surface.operation()
                    );
                }
            } else {
                // Only the wire ENVELOPE differs for a batch, whose slot carries the
                // `BatchStatementErrorCodeEnum` code for an unauthorized statement.
                let code = if surface.per_entry() {
                    "AccessDenied"
                } else {
                    "AccessDeniedException"
                };
                let expected = format!(
                    "User: {principal} is not authorized to perform: {action} on resource: \
                     {arn} because no identity-based policy allows the {action} action"
                );
                assert_eq!(
                    refused,
                    Some((code.to_string(), expected.clone())),
                    "case {row:?} / {}: the clause does not name this call's enclosing \
                     operation, so nothing grants it and it earns {code} {expected:?}, got \
                     {answered}",
                    surface.operation()
                );
                if surface.writes() {
                    assert_eq!(
                        stored(&router, &key).await,
                        Value::Null,
                        "case {row:?} / {}: a refused write stores nothing",
                        surface.operation()
                    );
                }
            }
        }
    }
}
