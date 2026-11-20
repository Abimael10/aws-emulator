// SPDX-License-Identifier: CC0-1.0

//! WHICH IAM action a PartiQL statement of `ExecuteTransaction` is charged, and what a grant
//! scoped to the transactional PartiQL path admits: three services, one decision.
//!
//! RULE: every statement of `ExecuteTransaction` is authorized as the PartiQL action of ITS
//! OWN verb — `dynamodb:PartiQLSelect` for a `SELECT`, `dynamodb:PartiQLInsert` for an
//! `INSERT`, `dynamodb:PartiQLUpdate` for an `UPDATE`, `dynamodb:PartiQLDelete` for a
//! `DELETE` — on the table that statement names, performed inside `ExecuteTransaction` as
//! the enclosing operation the call publishes. So any identity grant covering that verb's
//! action for the table admits the statement, whether it names the action exactly or by a
//! wildcard the IAM language admits, and whether it is unconditioned or scoped by a clause
//! naming that enclosing operation; a transaction is served only when EVERY statement of it
//! is granted, and a principal no grant reaches is refused `AccessDeniedException` (HTTP
//! 400) naming itself, that verb's action and the table's ARN, with the table left as it
//! stood.
//!
//! Three services have to agree for that to hold (IAM holds the role and its grant, STS
//! mints the session credential and names its principal, DynamoDB charges the action and
//! makes the decision), so this is the suite that can pin it; a crate-local test reaches only
//! one of them.
//!
//! The rows span what the rule generalises over: the statement's verb (all four, each with
//! the action it charges and the effect it has on the item it addresses), how the grant NAMES
//! the action (the verb's own action, the PartiQL family by prefix wildcard, every action of
//! the service, the three verbs that are not this statement's, an action no statement
//! performs), whether the grant is scoped by a clause naming the enclosing operation, which
//! enclosing operation that clause names, and how many statements the transaction carries
//! (one, and three whose grant covers all of them or all but one).
//!
//! The condition key itself — its value for every surface that publishes one, and the
//! operators a clause over it can be written with — is pinned beside this in
//! [`super::dynamodb_enclosing_operation_condition_key`].

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

/// The table every statement names.
const TABLE: &str = "TransactedStatements";

/// The long-term key every emulator publishes, which is the account's own credential: the
/// setup, the seeding and the read-backs are made with it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The role every probe below speaks for a session of.
const ROLE: &str = "ddb-partiql-transaction-role";

/// The session name the mint asks for, which is the last segment of the assumed-role ARN a
/// refusal names.
const SESSION: &str = "probe";

/// The name the one inline policy of that role is embedded under, replaced per row.
const POLICY_NAME: &str = "partiql-transaction";

/// The value an item carries when it is seeded, which is what a `SELECT` reports and what an
/// unapplied statement leaves behind.
const SEEDED: &str = "seeded-value";

/// The value an `INSERT` of this suite stores.
const INSERTED: &str = "inserted-value";

/// The value an `UPDATE` of this suite writes.
const UPDATED: &str = "updated-value";

/// How often to look while waiting for work the clock owes — a created table reaching
/// `ACTIVE` — and how many looks to take. The span is the service's own, so no fixed sleep
/// names the instant it ends, and the deadline is generous so a loaded host cannot decide the
/// outcome.
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

/// Create the table every statement names and wait until DynamoDB reports it `ACTIVE` ("you
/// can perform read and write operations only on an ACTIVE table").
async fn create_table(router: &MultiServiceRouter) {
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
        "setup: the table's ARN is what a refusal names, got {described}"
    );
}

/// Store the item `key` holds before a probe runs, through the account's own credential.
async fn seed(router: &MultiServiceRouter, key: &str) {
    let (status, put) = ddb(
        router,
        AMBIENT_KEY,
        "PutItem",
        &json!({
            "TableName": TABLE,
            "Item": {"pk": {"S": key}, "v": {"S": SEEDED}},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutItem({key:?}) must succeed, got {status} with {put}"
    );
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

/// One statement VERB of this dialect, with the action it charges and what it does to the
/// item it addresses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verb {
    Select,
    Insert,
    Update,
    Delete,
}

/// Every verb of the dialect, which the single-statement rows below are all taken over.
const VERBS: [Verb; 4] = [Verb::Select, Verb::Insert, Verb::Update, Verb::Delete];

impl Verb {
    /// The IAM action a statement of this verb performs, which is what its refusal names.
    fn action(self) -> &'static str {
        match self {
            Self::Select => "dynamodb:PartiQLSelect",
            Self::Insert => "dynamodb:PartiQLInsert",
            Self::Update => "dynamodb:PartiQLUpdate",
            Self::Delete => "dynamodb:PartiQLDelete",
        }
    }

    /// Whether the key this statement addresses must already hold an item: only an `INSERT`
    /// adds one, and it is the statement a key that already holds an item refuses.
    fn needs_item(self) -> bool {
        self != Self::Insert
    }

    /// The statement text, addressing the ONE item a statement of a transaction must name.
    fn statement(self, key: &str) -> String {
        match self {
            Self::Select => format!("SELECT * FROM {TABLE} WHERE pk = '{key}'"),
            Self::Insert => {
                format!("INSERT INTO {TABLE} VALUE {{'pk': '{key}', 'v': '{INSERTED}'}}")
            }
            Self::Update => format!("UPDATE {TABLE} SET v = '{UPDATED}' WHERE pk = '{key}'"),
            Self::Delete => format!("DELETE FROM {TABLE} WHERE pk = '{key}'"),
        }
    }

    /// The item `key` holds once a SERVED statement of this verb has been applied — for a
    /// `SELECT`, the seeded item it reports and leaves where it was.
    fn applied(self, key: &str) -> Value {
        match self {
            Self::Select => json!({"pk": {"S": key}, "v": {"S": SEEDED}}),
            Self::Insert => json!({"pk": {"S": key}, "v": {"S": INSERTED}}),
            Self::Update => json!({"pk": {"S": key}, "v": {"S": UPDATED}}),
            Self::Delete => Value::Null,
        }
    }

    /// The item `key` holds while nothing has been applied to it, which is what a REFUSED
    /// statement must leave behind.
    fn untouched(self, key: &str) -> Value {
        if self.needs_item() {
            json!({"pk": {"S": key}, "v": {"S": SEEDED}})
        } else {
            Value::Null
        }
    }
}

/// How a row's grant NAMES the action(s) it covers.
#[derive(Clone, Copy, Debug)]
enum Named {
    /// The statement's own verb action, spelled out.
    Verb,
    /// The PartiQL action family, by the prefix wildcard the IAM language admits inside an
    /// action name.
    PartiqlFamily,
    /// Every action of the service.
    Service,
    /// The three PartiQL verbs that are NOT this statement's own.
    OtherVerbs,
    /// An action of this service that no PartiQL statement performs.
    Unrelated,
}

impl Named {
    /// The `Action` element this spelling writes for a statement of `verb`.
    fn actions(self, verb: Verb) -> Value {
        match self {
            Self::Verb => json!([verb.action()]),
            Self::PartiqlFamily => json!(["dynamodb:PartiQL*"]),
            Self::Service => json!(["dynamodb:*"]),
            Self::OtherVerbs => json!(VERBS
                .iter()
                .filter(|other| **other != verb)
                .map(|other| other.action())
                .collect::<Vec<&str>>()),
            Self::Unrelated => json!(["dynamodb:DescribeTable"]),
        }
    }
}

/// The clause a row scopes its grant with, when it states one.
fn enclosing(operation: &str) -> Value {
    json!({"StringEquals": {"dynamodb:EnclosingOperation": operation}})
}

/// One identity-based document: `actions` on this table, under `condition` when the row
/// states one.
fn document(actions: Value, condition: Option<Value>) -> String {
    let mut statement = json!({
        "Effect": "Allow",
        "Action": actions,
        "Resource": table_arn(),
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({"Version": "2012-10-17", "Statement": [statement]}).to_string()
}

/// One row: how its grant names the action, the clause it scopes that grant with, and whether
/// the statement is then served.
struct Row {
    case: &'static str,
    named: Named,
    condition: Option<&'static str>,
    served: bool,
}

impl std::fmt::Debug for Row {
    /// A row quotes itself by its case, which is what every assertion below names it by.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.case)
    }
}

/// The refusal a probe earned as `(code, message)`, or `None` when it was served, beside the
/// answer itself.
async fn transact(
    router: &MultiServiceRouter,
    access_key_id: &str,
    statements: &[String],
) -> (Option<(String, String)>, Value) {
    let entries: Vec<Value> = statements
        .iter()
        .map(|statement| json!({"Statement": statement}))
        .collect();
    let (status, answered) = ddb(
        router,
        access_key_id,
        "ExecuteTransaction",
        &json!({"TransactStatements": entries}),
    )
    .await;
    if status == 200 {
        return (None, answered);
    }
    (Some((error_code(&answered), message_of(&answered))), answered)
}

/// The sentence a request no identity-based document grants is refused with.
fn refusal_sentence(principal: &str, action: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {} because no \
         identity-based policy allows the {action} action",
        table_arn()
    )
}

#[tokio::test]
async fn each_statement_of_a_partiql_transaction_is_charged_its_own_verbs_action() {
    let router = build_router();
    create_table(&router).await;
    let role_arn = create_role(&router).await;
    let (access_key_id, principal) = assume(&router, &role_arn).await;

    let rows: [Row; 7] = [
        Row {
            // The grant the recorded case was written with: the verb's own action, admitted
            // only as part of a transaction.
            case: "the verb's own action, scoped to the PartiQL transaction",
            named: Named::Verb,
            condition: Some("ExecuteTransaction"),
            served: true,
        },
        Row {
            case: "the PartiQL family by prefix wildcard, scoped to the PartiQL transaction",
            named: Named::PartiqlFamily,
            condition: Some("ExecuteTransaction"),
            served: true,
        },
        Row {
            case: "every action of the service, unconditioned",
            named: Named::Service,
            condition: None,
            served: true,
        },
        Row {
            case: "the verb's own action, unconditioned",
            named: Named::Verb,
            condition: None,
            served: true,
        },
        Row {
            // The three verbs this statement is not: the statement is charged its OWN verb's
            // action, so a grant over the others reaches nothing.
            case: "the three other PartiQL verbs, scoped to the PartiQL transaction",
            named: Named::OtherVerbs,
            condition: Some("ExecuteTransaction"),
            served: false,
        },
        Row {
            case: "the verb's own action, scoped to the PartiQL batch instead",
            named: Named::Verb,
            condition: Some("BatchExecuteStatement"),
            served: false,
        },
        Row {
            case: "an action no PartiQL statement performs",
            named: Named::Unrelated,
            condition: None,
            served: false,
        },
    ];

    for (at, row) in rows.iter().enumerate() {
        for verb in VERBS {
            put_role_policy(
                &router,
                row.case,
                &document(row.named.actions(verb), row.condition.map(enclosing)),
            )
            .await;
            // Every probe addresses its own key, so no statement meets what another one
            // stored: an `INSERT` of a key the table already holds is refused.
            let key = format!("row{at}-{verb:?}");
            if verb.needs_item() {
                seed(&router, &key).await;
            }
            let (refused, answered) =
                transact(&router, &access_key_id, &[verb.statement(&key)]).await;
            let action = verb.action();
            if row.served {
                assert_eq!(
                    refused, None,
                    "case {row:?} / {verb:?}: the grant covers {action} inside the \
                     transaction, so the statement is served, got {answered}"
                );
                if verb == Verb::Select {
                    assert_eq!(
                        answered["Responses"][0]["Item"],
                        json!({"pk": {"S": key}, "v": {"S": SEEDED}}),
                        "case {row:?} / {verb:?}: the served read reports the item it \
                         addresses, got {answered}"
                    );
                }
                assert_eq!(
                    stored(&router, &key).await,
                    verb.applied(&key),
                    "case {row:?} / {verb:?}: the served statement leaves its own effect on \
                     the item, got {answered}"
                );
            } else {
                let expected = refusal_sentence(&principal, action);
                assert_eq!(
                    refused,
                    Some(("AccessDeniedException".to_string(), expected.clone())),
                    "case {row:?} / {verb:?}: nothing grants {action} inside the transaction, \
                     so it earns AccessDeniedException {expected:?}, got {answered}"
                );
                assert_eq!(
                    stored(&router, &key).await,
                    verb.untouched(&key),
                    "case {row:?} / {verb:?}: a refused statement leaves the item as it \
                     stood, got {answered}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_transaction_is_served_only_where_every_statements_action_is_granted() {
    let router = build_router();
    create_table(&router).await;
    let role_arn = create_role(&router).await;
    let (access_key_id, principal) = assume(&router, &role_arn).await;

    // The three WRITE verbs in one transaction, each addressing its own item: the grant below
    // is what decides it, and nothing else about the statements changes between the two rows.
    let writes = [Verb::Insert, Verb::Update, Verb::Delete];
    for (at, (case, granted, refused_on)) in [
        (
            "every verb of the transaction granted",
            writes.as_slice(),
            None,
        ),
        (
            "one verb of the transaction left ungranted",
            [Verb::Insert, Verb::Delete].as_slice(),
            Some(Verb::Update),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let keys: Vec<String> = writes
            .iter()
            .map(|verb| format!("multi{at}-{verb:?}"))
            .collect();
        for (verb, key) in writes.iter().zip(&keys) {
            if verb.needs_item() {
                seed(&router, key).await;
            }
        }
        put_role_policy(
            &router,
            case,
            &document(
                json!(granted
                    .iter()
                    .map(|verb| verb.action())
                    .collect::<Vec<&str>>()),
                Some(enclosing("ExecuteTransaction")),
            ),
        )
        .await;
        let statements: Vec<String> = writes
            .iter()
            .zip(&keys)
            .map(|(verb, key)| verb.statement(key))
            .collect();
        let (refused, answered) = transact(&router, &access_key_id, &statements).await;
        match refused_on {
            None => {
                assert_eq!(
                    refused, None,
                    "case {case:?}: the grant covers every statement, so the transaction is \
                     served, got {answered}"
                );
                for (verb, key) in writes.iter().zip(&keys) {
                    assert_eq!(
                        stored(&router, key).await,
                        verb.applied(key),
                        "case {case:?} / {verb:?}: every statement of a served transaction \
                         is applied, got {answered}"
                    );
                }
            }
            Some(verb) => {
                let action = verb.action();
                let expected = refusal_sentence(&principal, action);
                assert_eq!(
                    refused,
                    Some(("AccessDeniedException".to_string(), expected.clone())),
                    "case {case:?}: the statement nothing grants names {action} in its \
                     refusal, got {answered}"
                );
                for (verb, key) in writes.iter().zip(&keys) {
                    assert_eq!(
                        stored(&router, key).await,
                        verb.untouched(key),
                        "case {case:?} / {verb:?}: a refused transaction applies no \
                         statement at all, got {answered}"
                    );
                }
            }
        }
    }
}
