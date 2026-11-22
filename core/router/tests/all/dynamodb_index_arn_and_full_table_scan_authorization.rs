// SPDX-License-Identifier: CC0-1.0

//! WHICH resource a DynamoDB read is authorized against, and what a PartiQL statement's own
//! scan-or-not publishes into the decision: three services, one answer.
//!
//! RULE 1: a request that names a SECONDARY INDEX of a table acts on that index, whose IAM
//! resource is the index's own ARN (`<table-arn>/index/<index-name>`), and a request that
//! names none acts on the table's ARN. So the documents in force on the caller are read for
//! the resource the request really addressed and the refusal names it: a grant written about
//! the table alone does not reach an index-qualified request, a grant written about an index
//! alone does not reach one that names no index, and a refusal is `AccessDeniedException`
//! (HTTP 400) naming the principal, the action the request performs and that resource.
//!
//! RULE 2: a PartiQL request publishes `dynamodb:FullTableScan` into the condition context —
//! true for a statement whose `WHERE` clause pins no partition key of the source it reads
//! (so the plan reads every entry of it), false for one that pins it — so a statement
//! conditioned on that key applies to the statements it selects and to no others, whether it
//! states a `Deny` or carries the grant.
//!
//! Three services have to agree for either to hold (IAM holds the role and its grant, STS
//! mints the session credential and names its principal, DynamoDB derives the resource and
//! the context and makes the decision), so this is the suite that can pin them; a crate-local
//! test reaches only one of them.
//!
//! The rows span what the rules generalise over: for rule 1 the surface that names an index
//! (an index-qualified PartiQL `SELECT`, a `Query`, a `Scan`, a `SearchVectors` of a vector
//! index) beside the same surfaces naming none, the KIND of index (a global secondary index
//! and a vector index), and what the grant's `Resource` covers (the table alone, the indexes
//! alone, both by wildcard, another table); for rule 2 the statement (a table scan, a keyed
//! read, a read filtered on a non-key attribute, a keyed read of an index, a walk of an
//! index), the literal the clause compares against, how the document spells it (quoted text
//! and a JSON boolean), the operator's `…IfExists` spelling, and whether the clause gates a
//! `Deny` or the `Allow` itself.

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

/// The table every probe reads.
const TABLE: &str = "IndexScopedReads";

/// The global secondary index the index-qualified reads name.
const SECONDARY_INDEX: &str = "by-gsi";

/// The vector index `SearchVectors` searches.
const VECTOR_INDEX: &str = "by-vector";

/// The attribute the secondary index is keyed by.
const INDEX_KEY: &str = "gsi_pk";

/// The one partition every probe reads, which the seeded item carries in the table's key and
/// in the index's.
const PARTITION: &str = "seed";

/// The non-key attribute a statement filtered on something other than a key compares.
const PAYLOAD: &str = "payload";

/// The long-term key every emulator publishes, which is the account's own credential: the
/// setup, the seeding and the read-backs are made with it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The role every probe below speaks for a session of.
const ROLE: &str = "ddb-index-scope-role";

/// The session name the mint asks for, which is the last segment of the assumed-role ARN a
/// refusal names.
const SESSION: &str = "probe";

/// The name the one inline policy of that role is embedded under, replaced per row.
const POLICY_NAME: &str = "index-scope";

/// How often to look while waiting for work the clock owes — a created table and its indexes
/// reaching `ACTIVE` — and how many looks to take. The span is the service's own, so no fixed
/// sleep names the instant it ends, and the deadline is generous so a loaded host cannot
/// decide the outcome.
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

/// The ARN of the table every probe reads.
fn table_arn() -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}")
}

/// The ARN of one index of that table, which is the resource a request naming it acts on.
fn index_arn(index: &str) -> String {
    format!("{}/index/{index}", table_arn())
}

/// Create the table every probe reads — one global secondary index and one vector index over
/// the same items — and wait until DynamoDB reports the table and BOTH indexes `ACTIVE` ("you
/// can perform read and write operations only on an ACTIVE table", and "the index must be in
/// the ACTIVE state" for a search).
async fn create_table(router: &MultiServiceRouter) {
    let (status, created) = ddb(
        router,
        AMBIENT_KEY,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": INDEX_KEY, "AttributeType": "S"},
            ],
            "GlobalSecondaryIndexes": [{
                "IndexName": SECONDARY_INDEX,
                "KeySchema": [{"AttributeName": INDEX_KEY, "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }],
            "VectorIndexes": [{
                "IndexName": VECTOR_INDEX,
                "VectorAttribute": {"AttributeName": "emb"},
                "Dimensions": 3,
                "DistanceFunction": "COSINE",
                "Projection": {"ProjectionType": "ALL"},
            }],
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
        if everything_active(&described) {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert!(
        everything_active(&described),
        "setup: DynamoDB must report {TABLE:?} and both of its indexes ACTIVE, got {described}"
    );
    assert_eq!(
        described["Table"]["TableArn"].as_str(),
        Some(table_arn().as_str()),
        "setup: the table's ARN is the base of every resource a refusal names, got {described}"
    );
}

/// Whether a description reports the table and both of its indexes `ACTIVE`.
fn everything_active(described: &Value) -> bool {
    let active = |member: &str, name: &str| {
        described["Table"][member]
            .as_array()
            .is_some_and(|indexes| {
                indexes.iter().any(|index| {
                    index["IndexName"] == json!(name) && index["IndexStatus"] == json!("ACTIVE")
                })
            })
    };
    described["Table"]["TableStatus"].as_str() == Some("ACTIVE")
        && active("GlobalSecondaryIndexes", SECONDARY_INDEX)
        && active("VectorIndexes", VECTOR_INDEX)
}

/// Store the one item every probe reads: it carries the table's key, the index's key and the
/// vector the search scores, so every probe below has something to report when it is served.
async fn seed(router: &MultiServiceRouter) {
    let (status, put) = ddb(
        router,
        AMBIENT_KEY,
        "PutItem",
        &json!({
            "TableName": TABLE,
            "Item": {
                "pk": {"S": PARTITION},
                INDEX_KEY: {"S": PARTITION},
                PAYLOAD: {"S": "stored"},
                "emb": {"L": [{"N": "1"}, {"N": "0"}, {"N": "0"}]},
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutItem must succeed, got {status} with {put}"
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

/// The sentence a request no identity-based document grants is refused with, over the
/// resource the request addressed.
fn ungranted_sentence(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// The sentence a request an identity-based document denies outright is refused with.
fn denied_sentence(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} with \
         an explicit deny in an identity-based policy"
    )
}

/// One probe: the operation, the body it sends, the IAM action it performs, and the index it
/// names — `None` for a probe that names none and so acts on the table itself.
struct Probe {
    case: &'static str,
    operation: &'static str,
    action: &'static str,
    index: Option<&'static str>,
}

impl std::fmt::Debug for Probe {
    /// A probe quotes itself by its case, which is what every assertion below names it by.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.case)
    }
}

impl Probe {
    /// The resource this probe acts on: its index's ARN, or the table's where it names none.
    fn resource(&self) -> String {
        match self.index {
            Some(index) => index_arn(index),
            None => table_arn(),
        }
    }

    /// The request body this probe sends.
    fn body(&self) -> Value {
        match (self.operation, self.index) {
            ("ExecuteStatement", Some(index)) => json!({
                "Statement": format!(
                    "SELECT * FROM \"{TABLE}\".\"{index}\" WHERE {INDEX_KEY} = ?"
                ),
                "Parameters": [{"S": PARTITION}],
            }),
            ("ExecuteStatement", None) => json!({
                "Statement": format!("SELECT * FROM \"{TABLE}\" WHERE pk = ?"),
                "Parameters": [{"S": PARTITION}],
            }),
            ("Query", Some(index)) => json!({
                "TableName": TABLE,
                "IndexName": index,
                "KeyConditionExpression": format!("{INDEX_KEY} = :p"),
                "ExpressionAttributeValues": {":p": {"S": PARTITION}},
            }),
            ("Query", None) => json!({
                "TableName": TABLE,
                "KeyConditionExpression": "pk = :p",
                "ExpressionAttributeValues": {":p": {"S": PARTITION}},
            }),
            ("Scan", Some(index)) => json!({"TableName": TABLE, "IndexName": index}),
            ("Scan", None) => json!({"TableName": TABLE}),
            ("SearchVectors", index) => json!({
                "TableName": TABLE,
                "IndexName": index.unwrap_or(VECTOR_INDEX),
                "SearchVector": [{"N": "1"}, {"N": "0"}, {"N": "0"}],
                "TopK": 1,
            }),
            (operation, _) => panic!("no body is written for {operation}"),
        }
    }

    /// Whether a served answer reports the seeded item, which is how a probe shows it read
    /// the source it was authorized for. A `SearchVectors` reports its matches under its own
    /// member, so it is checked there.
    fn reports_the_item(&self, answered: &Value) -> bool {
        match self.operation {
            "ExecuteStatement" => answered["Items"][0]["pk"] == json!({"S": PARTITION}),
            "Query" | "Scan" => answered["Items"][0]["pk"] == json!({"S": PARTITION}),
            _ => answered["SearchResults"][0]["Item"]["pk"] == json!({"S": PARTITION}),
        }
    }
}

/// Every probe of rule 1: the four surfaces that name an index, and the three of them that
/// can also name none.
fn probes() -> Vec<Probe> {
    vec![
        Probe {
            case: "an index-qualified PartiQL SELECT",
            operation: "ExecuteStatement",
            action: "dynamodb:PartiQLSelect",
            index: Some(SECONDARY_INDEX),
        },
        Probe {
            case: "a PartiQL SELECT naming no index",
            operation: "ExecuteStatement",
            action: "dynamodb:PartiQLSelect",
            index: None,
        },
        Probe {
            case: "a Query of the index",
            operation: "Query",
            action: "dynamodb:Query",
            index: Some(SECONDARY_INDEX),
        },
        Probe {
            case: "a Query of the table",
            operation: "Query",
            action: "dynamodb:Query",
            index: None,
        },
        Probe {
            case: "a Scan of the index",
            operation: "Scan",
            action: "dynamodb:Scan",
            index: Some(SECONDARY_INDEX),
        },
        Probe {
            case: "a Scan of the table",
            operation: "Scan",
            action: "dynamodb:Scan",
            index: None,
        },
        Probe {
            case: "a SearchVectors of the vector index",
            operation: "SearchVectors",
            action: "dynamodb:SearchVectors",
            index: Some(VECTOR_INDEX),
        },
    ]
}

/// What a row's grant covers, as the `Resource` element it writes.
#[derive(Clone, Copy, Debug)]
enum Scope {
    /// The table's own ARN alone.
    Table,
    /// Each index's ARN alone, and not the table's.
    Indexes,
    /// The table and every index of it, the pair a client scoping a reader writes.
    TableAndIndexes,
    /// Another table of the same account, which covers neither.
    AnotherTable,
}

impl Scope {
    /// The `Resource` element this scope writes.
    fn resources(self) -> Value {
        match self {
            Self::Table => json!([table_arn()]),
            Self::Indexes => json!([index_arn(SECONDARY_INDEX), index_arn(VECTOR_INDEX)]),
            Self::TableAndIndexes => json!([table_arn(), format!("{}/index/*", table_arn())]),
            Self::AnotherTable => {
                json!([format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/SomewhereElse")])
            }
        }
    }

    /// Whether a probe naming `index` is covered by this scope.
    fn covers(self, index: Option<&str>) -> bool {
        match self {
            Self::Table => index.is_none(),
            Self::Indexes => index.is_some(),
            Self::TableAndIndexes => true,
            Self::AnotherTable => false,
        }
    }
}

/// An identity document allowing every read action of this service on `resources`.
fn allow_reads(resources: Value) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "dynamodb:PartiQLSelect",
                "dynamodb:Query",
                "dynamodb:Scan",
                "dynamodb:SearchVectors",
            ],
            "Resource": resources,
        }],
    })
    .to_string()
}

#[tokio::test]
async fn a_request_naming_an_index_is_authorized_against_that_indexs_arn() {
    let router = build_router();
    create_table(&router).await;
    seed(&router).await;
    let role_arn = create_role(&router).await;
    let (access_key_id, principal) = assume(&router, &role_arn).await;

    for scope in [
        Scope::Table,
        Scope::Indexes,
        Scope::TableAndIndexes,
        Scope::AnotherTable,
    ] {
        for probe in probes() {
            let case = format!("{scope:?} / {probe:?}");
            put_role_policy(&router, &case, &allow_reads(scope.resources())).await;
            let (status, answered) =
                ddb(&router, &access_key_id, probe.operation, &probe.body()).await;
            if scope.covers(probe.index) {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the grant covers {}, so the read is served, got {status} \
                     with {answered}",
                    probe.resource()
                );
                assert!(
                    probe.reports_the_item(&answered),
                    "case {case:?}: a served read reports the seeded item, got {answered}"
                );
            } else {
                let expected = ungranted_sentence(&principal, probe.action, &probe.resource());
                assert_eq!(
                    (status, error_code(&answered).as_str(), message_of(&answered)),
                    (400, "AccessDeniedException", expected.clone()),
                    "case {case:?}: nothing grants {} on {}, so it earns \
                     AccessDeniedException {expected:?}, got {status} with {answered}",
                    probe.action,
                    probe.resource()
                );
            }
        }
    }
}

/// One statement of rule 2: its text, the source it names, and whether its plan reads every
/// entry of that source rather than one partition of it.
struct Statement {
    case: &'static str,
    text: String,
    /// The index its `FROM` target names, when it names one: the resource the statement acts
    /// on, which is what a refusal about it names (rule 1).
    index: Option<&'static str>,
    scans: bool,
}

impl std::fmt::Debug for Statement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.case)
    }
}

impl Statement {
    /// The resource this statement acts on: its index's ARN, or the table's where its source
    /// names no index.
    fn resource(&self) -> String {
        match self.index {
            Some(index) => index_arn(index),
            None => table_arn(),
        }
    }
}

/// Every statement of rule 2: a walk of the table, a read keyed by its partition key, a read
/// whose only predicate is on a non-key attribute (so the plan still walks), a read keyed by
/// the INDEX's partition key, and a walk of that index.
fn statements() -> Vec<Statement> {
    vec![
        Statement {
            case: "a walk of the whole table",
            text: format!("SELECT * FROM \"{TABLE}\""),
            index: None,
            scans: true,
        },
        Statement {
            case: "a read keyed by the table's partition key",
            text: format!("SELECT * FROM \"{TABLE}\" WHERE pk = '{PARTITION}'"),
            index: None,
            scans: false,
        },
        Statement {
            case: "a read whose only predicate is on a non-key attribute",
            text: format!("SELECT * FROM \"{TABLE}\" WHERE {PAYLOAD} = 'stored'"),
            index: None,
            scans: true,
        },
        Statement {
            case: "a read keyed by the index's partition key",
            text: format!(
                "SELECT * FROM \"{TABLE}\".\"{SECONDARY_INDEX}\" WHERE {INDEX_KEY} = \
                 '{PARTITION}'"
            ),
            index: Some(SECONDARY_INDEX),
            scans: false,
        },
        Statement {
            case: "a walk of the whole index",
            text: format!("SELECT * FROM \"{TABLE}\".\"{SECONDARY_INDEX}\""),
            index: Some(SECONDARY_INDEX),
            scans: true,
        },
    ]
}

/// The `Resource` element every document of rule 2 covers: the table and its indexes, so the
/// decision turns on the condition alone and not on which source the statement names.
fn every_source() -> Value {
    json!([table_arn(), format!("{}/index/*", table_arn())])
}

/// One clause row: the `Condition` it states, and which statements it selects.
struct Clause {
    case: &'static str,
    condition: Value,
    /// Whether the clause selects a statement that SCANS its source.
    selects_scans: bool,
}

impl std::fmt::Debug for Clause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.case)
    }
}

/// The clause rows: the literal compared against, the two spellings a document may state it
/// in, and the `…IfExists` form of the operator — which behaves as the plain one for a key
/// the request does carry a value for.
fn clauses() -> Vec<Clause> {
    vec![
        Clause {
            case: "Bool true, as quoted text",
            condition: json!({"Bool": {"dynamodb:FullTableScan": "true"}}),
            selects_scans: true,
        },
        Clause {
            case: "Bool true, as a JSON boolean",
            condition: json!({"Bool": {"dynamodb:FullTableScan": true}}),
            selects_scans: true,
        },
        Clause {
            case: "Bool false, as quoted text",
            condition: json!({"Bool": {"dynamodb:FullTableScan": "false"}}),
            selects_scans: false,
        },
        Clause {
            case: "Bool false, as a JSON boolean",
            condition: json!({"Bool": {"dynamodb:FullTableScan": false}}),
            selects_scans: false,
        },
        Clause {
            case: "BoolIfExists true",
            condition: json!({"BoolIfExists": {"dynamodb:FullTableScan": "true"}}),
            selects_scans: true,
        },
    ]
}

#[tokio::test]
async fn a_deny_conditioned_on_the_full_table_scan_key_refuses_only_the_statements_it_selects() {
    let router = build_router();
    create_table(&router).await;
    seed(&router).await;
    let role_arn = create_role(&router).await;
    let (access_key_id, principal) = assume(&router, &role_arn).await;

    for clause in clauses() {
        // The documented shape: every read allowed, with the conditioned `Deny` taking back
        // exactly the statements the clause selects.
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Effect": "Allow",
                    "Action": "dynamodb:PartiQLSelect",
                    "Resource": every_source(),
                },
                {
                    "Effect": "Deny",
                    "Action": "dynamodb:PartiQLSelect",
                    "Resource": every_source(),
                    "Condition": clause.condition.clone(),
                },
            ],
        })
        .to_string();
        put_role_policy(&router, clause.case, &document).await;
        for statement in statements() {
            let case = format!("{clause:?} / {statement:?}");
            let (status, answered) = ddb(
                &router,
                &access_key_id,
                "ExecuteStatement",
                &json!({"Statement": statement.text}),
            )
            .await;
            if clause.selects_scans == statement.scans {
                let expected = denied_sentence(
                    &principal,
                    "dynamodb:PartiQLSelect",
                    &statement.resource(),
                );
                assert_eq!(
                    (status, error_code(&answered).as_str(), message_of(&answered)),
                    (400, "AccessDeniedException", expected.clone()),
                    "case {case:?}: the clause selects this statement, so the Deny refuses it \
                     with {expected:?}, got {status} with {answered}"
                );
            } else {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the clause does not select this statement, so the Allow \
                     serves it, got {status} with {answered}"
                );
                assert_eq!(
                    answered["Items"][0]["pk"],
                    json!({"S": PARTITION}),
                    "case {case:?}: a served statement reports the seeded item, got {answered}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_grant_conditioned_on_the_full_table_scan_key_admits_only_the_statements_it_selects() {
    let router = build_router();
    create_table(&router).await;
    seed(&router).await;
    let role_arn = create_role(&router).await;
    let (access_key_id, principal) = assume(&router, &role_arn).await;

    for clause in clauses() {
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "dynamodb:PartiQLSelect",
                "Resource": every_source(),
                "Condition": clause.condition.clone(),
            }],
        })
        .to_string();
        put_role_policy(&router, clause.case, &document).await;
        for statement in statements() {
            let case = format!("{clause:?} / {statement:?}");
            let (status, answered) = ddb(
                &router,
                &access_key_id,
                "ExecuteStatement",
                &json!({"Statement": statement.text}),
            )
            .await;
            if clause.selects_scans == statement.scans {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the clause selects this statement, so the grant admits it, \
                     got {status} with {answered}"
                );
                assert_eq!(
                    answered["Items"][0]["pk"],
                    json!({"S": PARTITION}),
                    "case {case:?}: a served statement reports the seeded item, got {answered}"
                );
            } else {
                let expected = ungranted_sentence(
                    &principal,
                    "dynamodb:PartiQLSelect",
                    &statement.resource(),
                );
                assert_eq!(
                    (status, error_code(&answered).as_str(), message_of(&answered)),
                    (400, "AccessDeniedException", expected.clone()),
                    "case {case:?}: the clause does not select this statement, so nothing \
                     grants it and it earns {expected:?}, got {status} with {answered}"
                );
            }
        }
    }
}
