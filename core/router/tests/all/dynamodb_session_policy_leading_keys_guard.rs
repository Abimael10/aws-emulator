// SPDX-License-Identifier: CC0-1.0

//! A partition-scoped session policy on the DynamoDB data plane, and what its set operator
//! says about a call that addresses no partition at all.
//!
//! RULE: under IAM's set qualifiers a clause is decided against the SET of values the
//! request carries for the key, so a key the request models and carries NO value for is the
//! empty set: `ForAllValues:` holds over it ("returns true if there are no request values")
//! and `ForAnyValue:` does not, and the only operator that tests a key's PRESENCE is `Null`.
//! A DynamoDB table or index walk — a `Scan`, or a PartiQL `SELECT` whose `WHERE` clause
//! pins no partition — addresses no partition, so it carries no value for
//! `dynamodb:LeadingKeys`: a session policy whose only condition is
//! `ForAllValues:StringEquals` over that key therefore ADMITS the walk over every partition,
//! and it takes a `{"Null": {"dynamodb:LeadingKeys": "false"}}` guard beside the clause to
//! refuse it while the keyed reads keep working.
//!
//! Three services have to agree for that to hold (IAM holds the role and its grant, STS
//! mints the session and holds the document it was scoped with, DynamoDB derives the key and
//! makes the decision), so this is the suite that can pin it; a crate-local test reaches only
//! one of them.
//!
//! Each of the three keys a data-plane call answers for is derived from the request itself,
//! which is what lets a conditioned clause be DECIDED rather than left wanting a value:
//! `dynamodb:LeadingKeys` holds the partition-key values the call addresses,
//! `dynamodb:Attributes` the top-level attribute names it projects, and `dynamodb:Select`
//! the selection it states or implies — `SPECIFIC_ATTRIBUTES` for a call that projects
//! attributes through either the expression member or the legacy one it replaced. So the last
//! session below, scoped on those two keys and on no partition at all, admits a projected
//! walk and refuses the same walk asking for everything.
//!
//! The rows span what the rule generalises over: the SET QUALIFIER the clause carries
//! (`ForAllValues:`, `ForAnyValue:`), the presence guard beside it (none, `Null` false,
//! `Null` true), which condition key the clause tests (the partition, the attribute set, the
//! selection), whether the call addresses a partition (a keyed read, a walk), which partition
//! it addresses (one the clause lists, one it does not), the selection a walk asks for
//! (listed attributes, every attribute, a count), and the operation the walk or the keyed read
//! travels in (`GetItem`, `Query`, `Scan`, `ExecuteStatement`, whose statement carries its own
//! IAM action). A served call is read for the partitions it reports and, where the row is
//! about a projection, for the attribute names it reports of each item.

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

/// The table every row acts on.
const TABLE: &str = "GuardedPartitions";

/// The long-term key every emulator publishes, which is the account's own credential: the
/// setup and the read-backs are made with it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The role every session below is a session of.
const ROLE: &str = "ddb-leading-keys-guard-role";

/// The partition every conditioned document lists.
const ALLOWED_PARTITION: &str = "tenant-allowed";

/// A partition of the same table that none of them lists.
const OTHER_PARTITION: &str = "tenant-other";

/// The attribute names the projection-scoped document lists in `dynamodb:Attributes`: the two
/// key attributes and one non-key attribute, so a read of exactly these is inside its
/// envelope.
const VISIBLE_ATTRIBUTES: [&str; 3] = ["pk", "sk", "pub"];

/// The attribute that document withholds by leaving it out of that list.
const WITHHELD_ATTRIBUTE: &str = "secret";

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

/// Create the table the rows act on, wait until DynamoDB reports it `ACTIVE` ("you can
/// perform read and write operations only on an ACTIVE table"), and seed one item in each of
/// the two partitions.
async fn seed(router: &MultiServiceRouter) {
    let (status, created) = ddb(
        router,
        AMBIENT_KEY,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "S"},
            ],
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
        let (_, answered) =
            ddb(router, AMBIENT_KEY, "DescribeTable", &json!({"TableName": TABLE})).await;
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

    for partition in [ALLOWED_PARTITION, OTHER_PARTITION] {
        let (status, put) = ddb(
            router,
            AMBIENT_KEY,
            "PutItem",
            &json!({
                "TableName": TABLE,
                "Item": {
                    "pk": {"S": partition},
                    "sk": {"S": "1"},
                    "pub": {"S": "public-value"},
                    WITHHELD_ATTRIBUTE: {"S": "hidden-value"},
                },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: PutItem({partition:?}) must succeed, got {status} with {put}"
        );
    }
}

/// Create the role every session is a session of: trusted by the account root, and granted
/// every action of this service on the table — so what scopes a session below is its session
/// policy and never the role's own document.
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
    let role_arn = element(&created, "Arn");
    let grant = json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": "dynamodb:*", "Resource": table_arn()}],
    })
    .to_string();
    let (status, embedded) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", "table-wide"),
            ("PolicyDocument", &grant),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({ROLE:?}) must succeed, got {status} with {embedded}"
    );
    role_arn
}

/// One session of that role: the access key id a call signs with, and the `AssumedRoleUser`
/// ARN STS reported for it, which is the principal a refusal names.
struct Session {
    label: &'static str,
    access_key_id: String,
    principal_arn: String,
}

/// Assume the role under `session_name`, passing `policy` as the inline session policy.
async fn assume(
    router: &MultiServiceRouter,
    role_arn: &str,
    label: &'static str,
    session_name: &str,
    policy: &str,
) -> Session {
    let members = vec![
        ("RoleArn", role_arn),
        ("RoleSessionName", session_name),
        ("Policy", policy),
    ];
    let (status, body) = query(router, "sts", "2011-06-15", "AssumeRole", &members).await;
    assert_eq!(
        status, 200,
        "setup: AssumeRole for {label:?} must succeed, got {status} with {body}"
    );
    Session {
        label,
        access_key_id: element(&body, "AccessKeyId"),
        // The first `<Arn>` of the answer is the `AssumedRoleUser`'s, which is the identity
        // `sts:GetCallerIdentity` reports for this credential.
        principal_arn: element(&body, "Arn"),
    }
}

/// A session policy granting the four read actions on this table, conditioned on the
/// partitions the call addresses: `clause` is the clause written over
/// `dynamodb:LeadingKeys`, and `guard` the presence clause written beside it, if any.
fn scoped_policy(clause: &str, guard: Option<&str>) -> String {
    let mut condition = serde_json::Map::new();
    condition.insert(
        clause.to_string(),
        json!({"dynamodb:LeadingKeys": [ALLOWED_PARTITION]}),
    );
    if let Some(guard) = guard {
        condition.insert(
            "Null".to_string(),
            json!({"dynamodb:LeadingKeys": guard}),
        );
    }
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "dynamodb:GetItem",
                "dynamodb:Query",
                "dynamodb:Scan",
                "dynamodb:PartiQLSelect",
            ],
            "Resource": table_arn(),
            "Condition": Value::Object(condition),
        }],
    })
    .to_string()
}

/// A session policy granting the same four read actions on this table, conditioned on the
/// ATTRIBUTES a call projects and the SELECTION it asks for and on no partition at all: the
/// two keys a call derives from its projection, whichever member the projection travels in.
fn projection_scoped_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "dynamodb:GetItem",
                "dynamodb:Query",
                "dynamodb:Scan",
                "dynamodb:PartiQLSelect",
            ],
            "Resource": table_arn(),
            "Condition": {
                "ForAllValues:StringEquals": {"dynamodb:Attributes": VISIBLE_ATTRIBUTES},
                "StringEquals": {"dynamodb:Select": "SPECIFIC_ATTRIBUTES"},
            },
        }],
    })
    .to_string()
}

/// One row: the session it is made with, the operation, its body, the IAM action a refusal
/// would name, and the partitions the answer must report — `None` for a call the policies in
/// force refuse.
struct Row {
    case: &'static str,
    session: usize,
    operation: &'static str,
    request: Value,
    action: &'static str,
    reported: Option<&'static [&'static str]>,
}

/// One row of the second table: a call the policies in force SERVE, read for the attribute
/// names it reports of each item rather than for the partitions it reached.
struct Projection {
    case: &'static str,
    session: usize,
    operation: &'static str,
    request: Value,
    attributes: &'static [&'static str],
}

/// A `GetItem` key addressing one partition's seeded item.
fn key_of(partition: &str) -> Value {
    json!({"pk": {"S": partition}, "sk": {"S": "1"}})
}

/// An `ExecuteStatement` body reading this table: every item of it, or the items of the one
/// partition `partition` names.
fn statement(partition: Option<&str>) -> Value {
    let text = match partition {
        Some(partition) => format!("SELECT * FROM \"{TABLE}\" WHERE pk = '{partition}'"),
        None => format!("SELECT * FROM \"{TABLE}\""),
    };
    json!({"Statement": text})
}

/// The items an answer reports: the one a keyed read found, or the ones a walk listed.
fn reported_items(operation: &str, answer: &Value) -> Vec<Value> {
    match operation {
        "GetItem" => answer.get("Item").cloned().into_iter().collect(),
        _ => answer
            .get("Items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    }
}

/// The partitions an answer reports, in partition order and each once.
fn reported_partitions(operation: &str, answer: &Value) -> Vec<String> {
    let mut partitions: Vec<String> = reported_items(operation, answer)
        .iter()
        .filter_map(|item| item.pointer("/pk/S").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    partitions.sort();
    partitions.dedup();
    partitions
}

/// The attribute names the items an answer reports carry, in name order and each once: what a
/// served read reported OF the stored items, which is where an attribute a document withholds
/// would show up.
fn reported_attributes(operation: &str, answer: &Value) -> Vec<String> {
    let mut names: Vec<String> = reported_items(operation, answer)
        .iter()
        .filter_map(Value::as_object)
        .flat_map(|item| item.keys().cloned())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// RULE: see the module docs. The table's rows differ along the set qualifier the clause
/// carries, the presence guard beside it, whether the call addresses a partition, which
/// partition it addresses, and the operation it travels in.
#[tokio::test]
async fn a_set_qualified_leading_keys_clause_is_decided_by_the_set_the_request_carries() {
    let router = build_router();
    seed(&router).await;
    let role_arn = create_role(&router).await;

    let sessions = [
        assume(
            &router,
            &role_arn,
            "the clause alone, over every value the call addresses",
            "forall",
            &scoped_policy("ForAllValues:StringEquals", None),
        )
        .await,
        assume(
            &router,
            &role_arn,
            "the same clause with the presence guard beside it",
            "guarded",
            &scoped_policy("ForAllValues:StringEquals", Some("false")),
        )
        .await,
        assume(
            &router,
            &role_arn,
            "the same clause guarded the other way, so the key must be ABSENT",
            "absent",
            &scoped_policy("ForAllValues:StringEquals", Some("true")),
        )
        .await,
        assume(
            &router,
            &role_arn,
            "the clause under the other set qualifier",
            "forany",
            &scoped_policy("ForAnyValue:StringEquals", None),
        )
        .await,
        assume(
            &router,
            &role_arn,
            "a document scoped on the projection instead of the partition",
            "projected",
            &projection_scoped_policy(),
        )
        .await,
    ];
    /// The indexes into the sessions above, named so a row reads as what it is about.
    const FOR_ALL: usize = 0;
    const GUARDED: usize = 1;
    const ABSENT: usize = 2;
    const FOR_ANY: usize = 3;
    const PROJECTED: usize = 4;

    /// The partition the clause lists, which a keyed read inside the envelope reports.
    const LISTED: &[&str] = &[ALLOWED_PARTITION];
    /// Every partition of the table, which is what an admitted walk reports.
    const EVERY: &[&str] = &[ALLOWED_PARTITION, OTHER_PARTITION];

    let rows: Vec<Row> = vec![
        Row {
            case: "the keyed read of the partition the clause lists",
            session: FOR_ALL,
            operation: "GetItem",
            request: json!({
                "TableName": TABLE,
                "Key": key_of(ALLOWED_PARTITION),
                "ConsistentRead": true,
            }),
            action: "dynamodb:GetItem",
            reported: Some(LISTED),
        },
        Row {
            case: "the keyed read of a partition it does not list",
            session: FOR_ALL,
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key_of(OTHER_PARTITION)}),
            action: "dynamodb:GetItem",
            reported: None,
        },
        Row {
            case: "the query of the partition the clause lists",
            session: FOR_ALL,
            operation: "Query",
            request: json!({
                "TableName": TABLE,
                "KeyConditionExpression": "pk = :p",
                "ExpressionAttributeValues": {":p": {"S": ALLOWED_PARTITION}},
            }),
            action: "dynamodb:Query",
            reported: Some(LISTED),
        },
        Row {
            case: "the query of a partition it does not list",
            session: FOR_ALL,
            operation: "Query",
            request: json!({
                "TableName": TABLE,
                "KeyConditionExpression": "pk = :p",
                "ExpressionAttributeValues": {":p": {"S": OTHER_PARTITION}},
            }),
            action: "dynamodb:Query",
            reported: None,
        },
        Row {
            // The walk addresses no partition, so it carries no value for the key: that is
            // IAM's empty set, which the set qualifier holds over, and the walk is served
            // over every partition — the hole the presence guard below exists to close.
            case: "the table walk, which addresses no partition, under the clause alone",
            session: FOR_ALL,
            operation: "Scan",
            request: json!({"TableName": TABLE, "ConsistentRead": true}),
            action: "dynamodb:Scan",
            reported: Some(EVERY),
        },
        Row {
            case: "the key-less statement walk under the clause alone",
            session: FOR_ALL,
            operation: "ExecuteStatement",
            request: statement(None),
            action: "dynamodb:PartiQLSelect",
            reported: Some(EVERY),
        },
        Row {
            case: "a statement pinning the partition the clause lists",
            session: FOR_ALL,
            operation: "ExecuteStatement",
            request: statement(Some(ALLOWED_PARTITION)),
            action: "dynamodb:PartiQLSelect",
            reported: Some(LISTED),
        },
        Row {
            case: "a statement pinning a partition it does not list",
            session: FOR_ALL,
            operation: "ExecuteStatement",
            request: statement(Some(OTHER_PARTITION)),
            action: "dynamodb:PartiQLSelect",
            reported: None,
        },
        Row {
            // The guard asks whether the key is PRESENT, which the walk answers no to, so
            // the statement no longer applies to it.
            case: "the table walk once the presence guard is beside the clause",
            session: GUARDED,
            operation: "Scan",
            request: json!({"TableName": TABLE, "ConsistentRead": true}),
            action: "dynamodb:Scan",
            reported: None,
        },
        Row {
            case: "the key-less statement walk once the presence guard is beside it",
            session: GUARDED,
            operation: "ExecuteStatement",
            request: statement(None),
            action: "dynamodb:PartiQLSelect",
            reported: None,
        },
        Row {
            // And the keyed reads the guard was added to keep go on working.
            case: "the keyed read under the presence guard",
            session: GUARDED,
            operation: "GetItem",
            request: json!({
                "TableName": TABLE,
                "Key": key_of(ALLOWED_PARTITION),
                "ConsistentRead": true,
            }),
            action: "dynamodb:GetItem",
            reported: Some(LISTED),
        },
        Row {
            case: "the keyed query under the presence guard",
            session: GUARDED,
            operation: "Query",
            request: json!({
                "TableName": TABLE,
                "KeyConditionExpression": "pk = :p",
                "ExpressionAttributeValues": {":p": {"S": ALLOWED_PARTITION}},
            }),
            action: "dynamodb:Query",
            reported: Some(LISTED),
        },
        Row {
            case: "the keyed read of a partition the clause does not list, under the guard",
            session: GUARDED,
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key_of(OTHER_PARTITION)}),
            action: "dynamodb:GetItem",
            reported: None,
        },
        Row {
            // The guard spelled the other way admits exactly the calls the first one
            // refused: `Null` is the operator that tests presence, in both directions.
            case: "the table walk under a guard asking that the key be absent",
            session: ABSENT,
            operation: "Scan",
            request: json!({"TableName": TABLE, "ConsistentRead": true}),
            action: "dynamodb:Scan",
            reported: Some(EVERY),
        },
        Row {
            case: "the keyed read under a guard asking that the key be absent",
            session: ABSENT,
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key_of(ALLOWED_PARTITION)}),
            action: "dynamodb:GetItem",
            reported: None,
        },
        Row {
            // The other set qualifier needs ONE value of the set to satisfy it, which the
            // empty set carries none of: the walk is refused with no guard at all.
            case: "the keyed read under the other set qualifier",
            session: FOR_ANY,
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key_of(ALLOWED_PARTITION)}),
            action: "dynamodb:GetItem",
            reported: Some(LISTED),
        },
        Row {
            case: "the table walk under the other set qualifier",
            session: FOR_ANY,
            operation: "Scan",
            request: json!({"TableName": TABLE, "ConsistentRead": true}),
            action: "dynamodb:Scan",
            reported: None,
        },
        Row {
            // Under that qualifier a value OUTSIDE the list fails the clause too, so the
            // refusal of the walk above is the empty set and not the qualifier refusing
            // everything.
            case: "the keyed read of a partition the other set qualifier does not list",
            session: FOR_ANY,
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key_of(OTHER_PARTITION)}),
            action: "dynamodb:GetItem",
            reported: None,
        },
        Row {
            case: "the key-less statement walk under the other set qualifier",
            session: FOR_ANY,
            operation: "ExecuteStatement",
            request: statement(None),
            action: "dynamodb:PartiQLSelect",
            reported: None,
        },
        Row {
            // The projection-scoped document conditions no partition, so the walk's empty
            // set never comes into it: what decides the walk is the selection it states
            // and the attribute names it projects, both derived from the request.
            case: "the projected walk under a document scoped on the projection",
            session: PROJECTED,
            operation: "Scan",
            request: json!({
                "TableName": TABLE,
                "ProjectionExpression": VISIBLE_ATTRIBUTES.join(", "),
                "ConsistentRead": true,
            }),
            action: "dynamodb:Scan",
            reported: Some(EVERY),
        },
        Row {
            case: "the same walk asking for every attribute of each item",
            session: PROJECTED,
            operation: "Scan",
            request: json!({"TableName": TABLE, "ConsistentRead": true}),
            action: "dynamodb:Scan",
            reported: None,
        },
        Row {
            case: "the same walk reporting only how many items matched",
            session: PROJECTED,
            operation: "Scan",
            request: json!({"TableName": TABLE, "Select": "COUNT"}),
            action: "dynamodb:Scan",
            reported: None,
        },
        Row {
            case: "a walk projecting an attribute the document does not list",
            session: PROJECTED,
            operation: "Scan",
            request: json!({
                "TableName": TABLE,
                "ProjectionExpression": format!("pk, {WITHHELD_ATTRIBUTE}"),
            }),
            action: "dynamodb:Scan",
            reported: None,
        },
        Row {
            // The legacy member states the same ask, so the selection it implies is the
            // same one.
            case: "the listed names stated through the legacy member",
            session: PROJECTED,
            operation: "GetItem",
            request: json!({
                "TableName": TABLE,
                "Key": key_of(ALLOWED_PARTITION),
                "AttributesToGet": VISIBLE_ATTRIBUTES,
            }),
            action: "dynamodb:GetItem",
            reported: Some(LISTED),
        },
        Row {
            case: "the keyed read of the partition no clause of that document lists",
            session: PROJECTED,
            operation: "GetItem",
            request: json!({
                "TableName": TABLE,
                "Key": key_of(OTHER_PARTITION),
                "ProjectionExpression": VISIBLE_ATTRIBUTES.join(", "),
            }),
            action: "dynamodb:GetItem",
            reported: Some(&[OTHER_PARTITION]),
        },
    ];

    for row in &rows {
        let session = &sessions[row.session];
        let case = row.case;
        let (status, answer) = ddb(&router, &session.access_key_id, row.operation, &row.request)
            .await;
        match row.reported {
            Some(expected) => {
                assert_eq!(
                    status, 200,
                    "case {case:?} ({}): {} expected HTTP 200, got {status} {answer}",
                    session.label, row.operation
                );
                let partitions = reported_partitions(row.operation, &answer);
                assert_eq!(
                    partitions, expected,
                    "case {case:?} ({}): the answer must report exactly {expected:?}, got \
                     {partitions:?} in {answer}",
                    session.label
                );
            }
            None => {
                assert_eq!(
                    status, 400,
                    "case {case:?} ({}): {} expected HTTP 400, got {status} {answer}",
                    session.label, row.operation
                );
                assert_eq!(
                    error_code(&answer),
                    "AccessDeniedException",
                    "case {case:?} ({}): expected AccessDeniedException, got {answer}",
                    session.label
                );
                let action = row.action;
                let expected = format!(
                    "User: {} is not authorized to perform: {action} on resource: {} \
                     because no session policy allows the {action} action",
                    session.principal_arn,
                    table_arn()
                );
                let message = message_of(&answer);
                assert_eq!(
                    message, expected,
                    "case {case:?} ({}): expected the session-policy refusal {expected:?}, \
                     got {message:?}",
                    session.label
                );
            }
        }
    }

    // What a SERVED read reports of each item it reports, which is the other half of the
    // keys a projection-scoped document is decided on: an admitted projected read reports
    // exactly the names it projected, and the document that conditions no projection at all
    // withholds nothing — so a row above answering 200 cannot be passing while the emulator
    // serves an attribute a document meant to withhold.
    let projections = vec![
        Projection {
            case: "the projected walk reports exactly the names it projected",
            session: PROJECTED,
            operation: "Scan",
            request: json!({
                "TableName": TABLE,
                "ProjectionExpression": VISIBLE_ATTRIBUTES.join(", "),
                "ConsistentRead": true,
            }),
            attributes: &["pk", "pub", "sk"],
        },
        Projection {
            case: "the legacy member reports exactly the names it listed",
            session: PROJECTED,
            operation: "GetItem",
            request: json!({
                "TableName": TABLE,
                "Key": key_of(ALLOWED_PARTITION),
                "AttributesToGet": VISIBLE_ATTRIBUTES,
            }),
            attributes: &["pk", "pub", "sk"],
        },
        Projection {
            case: "a document conditioning no projection withholds no attribute",
            session: FOR_ALL,
            operation: "GetItem",
            request: json!({
                "TableName": TABLE,
                "Key": key_of(ALLOWED_PARTITION),
                "ConsistentRead": true,
            }),
            attributes: &["pk", "pub", WITHHELD_ATTRIBUTE, "sk"],
        },
    ];
    for row in &projections {
        let session = &sessions[row.session];
        let case = row.case;
        let operation = row.operation;
        let expected = row.attributes;
        let (status, answer) = ddb(&router, &session.access_key_id, operation, &row.request).await;
        assert_eq!(
            status, 200,
            "case {case:?} ({}): {operation} expected HTTP 200, got {status} {answer}",
            session.label
        );
        let names = reported_attributes(operation, &answer);
        assert_eq!(
            names, expected,
            "case {case:?} ({}): the answer must carry exactly {expected:?}, got {names:?} \
             in {answer}",
            session.label
        );
    }

    // Every refused call had no effect on the table: the account's own credential still
    // walks both partitions.
    let (status, stored) = ddb(
        &router,
        AMBIENT_KEY,
        "Scan",
        &json!({"TableName": TABLE, "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the table after the refused reads\": the account walks its own table, got \
         {status} {stored}"
    );
    let partitions = reported_partitions("Scan", &stored);
    assert_eq!(
        partitions, EVERY,
        "case \"the table after the refused reads\": every partition must still be stored, \
         got {partitions:?} in {stored}"
    );
}
