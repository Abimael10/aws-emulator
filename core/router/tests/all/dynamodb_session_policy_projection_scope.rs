// SPDX-License-Identifier: CC0-1.0

//! A projection-scoped session policy on the DynamoDB data plane: three services, one
//! decision.
//!
//! RULE: a data-plane call signed with an assumed-role session credential is authorized
//! against the role's identity-based documents INTERSECTED with the inline session policy
//! the mint passed ("the resulting session's permissions are the intersection of the role's
//! identity-based policies and the session policies", `AssumeRoleRequest$Policy`), and the
//! session policy's `Condition` is decided against the condition keys the REQUEST itself
//! carries values for: `dynamodb:Select` (the selection the call states, or
//! `SPECIFIC_ATTRIBUTES` when it projects attributes through either the expression member or
//! the legacy one it replaced), `dynamodb:Attributes` (the top-level attribute names it
//! projects) and `dynamodb:LeadingKeys` (the partition-key values it addresses). A call
//! outside the envelope such a document draws is refused `AccessDeniedException` naming the
//! document that withheld the grant — the session policy — and the stored item is untouched
//! by a refused read, so the withheld attribute stays withheld rather than being removed.
//!
//! Three services have to agree for that to hold (IAM holds the role and its grant, STS
//! mints the session and holds the document it was scoped with, DynamoDB derives the keys
//! and makes the decision), so this is the suite that can pin it; a crate-local test reaches
//! only one of them.
//!
//! The rows span what the rule generalises over: which of the three keys the call steps
//! outside (the selection, the attribute set, the partition), the member the projection
//! travels in (`ProjectionExpression`, `AttributesToGet`, `Query`'s `Select` beside one), the
//! operation (`GetItem`, `Query`, `BatchGetItem`, and `Scan`/`PutItem`, which the document
//! names no grant for at all), and what the credential was scoped with (the conditioned
//! document, the same grant with no `Condition`, and no session policy at all — the two
//! controls that show it is the clause doing the scoping and not the role's own grant or the
//! read path).

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
const TABLE: &str = "ScopedReads";

/// The long-term key every emulator publishes, which is the account's own credential: the
/// setup and the read-backs are made with it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The role every session below is a session of.
const ROLE: &str = "ddb-projection-scoped-role";

/// The partition the conditioned document admits.
const ALLOWED_PARTITION: &str = "tenant-allowed";

/// A partition of the same table that it does not.
const OTHER_PARTITION: &str = "tenant-other";

/// The attribute names the conditioned document lists in `dynamodb:Attributes`: the two key
/// attributes and one non-key attribute, so a read of exactly these is inside the envelope.
const VISIBLE_ATTRIBUTES: [&str; 3] = ["pk", "sk", "pub"];

/// The attribute the conditioned document withholds by leaving it out of that list.
const WITHHELD_ATTRIBUTE: &str = "secret";

/// The value the withheld attribute holds, which must still be there after every refused
/// read: a refusal withholds an attribute, it does not remove it.
const WITHHELD_VALUE: &str = "hidden-value";

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
/// the two partitions — each carrying the withheld attribute beside the listed ones.
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
        let (_, answered) = ddb(router, AMBIENT_KEY, "DescribeTable", &json!({"TableName": TABLE}))
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

    for (partition, public) in [(ALLOWED_PARTITION, "public-value"), (OTHER_PARTITION, "p")] {
        let (status, put) = ddb(
            router,
            AMBIENT_KEY,
            "PutItem",
            &json!({
                "TableName": TABLE,
                "Item": {
                    "pk": {"S": partition},
                    "sk": {"S": "1"},
                    "pub": {"S": public},
                    WITHHELD_ATTRIBUTE: {"S": WITHHELD_VALUE},
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

/// Assume the role under `session_name`, passing `policy` as the inline session policy when
/// one is given.
async fn assume(
    router: &MultiServiceRouter,
    role_arn: &str,
    label: &'static str,
    session_name: &str,
    policy: Option<&str>,
) -> Session {
    let mut members = vec![("RoleArn", role_arn), ("RoleSessionName", session_name)];
    if let Some(policy) = policy {
        members.push(("Policy", policy));
    }
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

/// The conditioned session policy the rule is about: the three read actions on this table,
/// admitted only where the call's own `dynamodb:Select`, `dynamodb:Attributes` and
/// `dynamodb:LeadingKeys` stay inside the envelope.
fn conditioned_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["dynamodb:GetItem", "dynamodb:Query", "dynamodb:BatchGetItem"],
            "Resource": table_arn(),
            "Condition": {
                "ForAllValues:StringEquals": {
                    "dynamodb:LeadingKeys": [ALLOWED_PARTITION],
                    "dynamodb:Attributes": VISIBLE_ATTRIBUTES,
                },
                "StringEquals": {"dynamodb:Select": "SPECIFIC_ATTRIBUTES"},
            },
        }],
    })
    .to_string()
}

/// The SAME grant with no `Condition` at all: the control that shows the clause is what
/// scoped the rows above.
fn unconditioned_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["dynamodb:GetItem", "dynamodb:Query", "dynamodb:BatchGetItem"],
            "Resource": table_arn(),
        }],
    })
    .to_string()
}

/// The projection one row asks for, as the text the expression member carries.
fn projection(names: &[&str]) -> String {
    names.join(", ")
}

/// One row: the session it is made with, the operation, its body, and the attribute names the
/// answer must carry — `None` for a call the policies in force refuse.
struct Row {
    case: &'static str,
    session: usize,
    operation: &'static str,
    request: Value,
    served: Option<&'static [&'static str]>,
}

/// A `GetItem` of one partition's seeded item.
fn key_of(partition: &str) -> Value {
    json!({"pk": {"S": partition}, "sk": {"S": "1"}})
}

/// The attribute names an answer carries for `operation`, in name order.
fn served_attributes(operation: &str, answer: &Value) -> Vec<String> {
    let item = match operation {
        "GetItem" => answer.get("Item").cloned().unwrap_or(Value::Null),
        "Query" => answer
            .get("Items")
            .and_then(|items| items.get(0))
            .cloned()
            .unwrap_or(Value::Null),
        "BatchGetItem" => answer
            .pointer(&format!("/Responses/{TABLE}/0"))
            .cloned()
            .unwrap_or(Value::Null),
        // A walk reports every partition in no stated order, so the row is read by the
        // partition it is about rather than by position.
        "Scan" => answer
            .get("Items")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item.pointer("/pk/S").and_then(Value::as_str) == Some(ALLOWED_PARTITION))
            })
            .cloned()
            .unwrap_or(Value::Null),
        other => panic!("no reader for a served {other} answer"),
    };
    let mut names: Vec<String> = item
        .as_object()
        .map(|members| members.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// RULE: see the module docs. The table's rows differ along which condition key the call
/// steps outside, the member its projection travels in, the operation, and what the
/// credential was scoped with.
#[tokio::test]
async fn a_projection_scoped_session_policy_decides_each_read_on_the_keys_the_request_carries() {
    let router = build_router();
    seed(&router).await;
    let role_arn = create_role(&router).await;

    let sessions = [
        assume(
            &router,
            &role_arn,
            "the projection-scoped session",
            "scoped",
            Some(&conditioned_policy()),
        )
        .await,
        assume(
            &router,
            &role_arn,
            "the same grant with no condition",
            "unconditioned",
            Some(&unconditioned_policy()),
        )
        .await,
        assume(&router, &role_arn, "no session policy at all", "capless", None).await,
    ];
    /// The indexes into the sessions above, named so a row reads as what it is about.
    const SCOPED: usize = 0;
    const UNCONDITIONED: usize = 1;
    const CAPLESS: usize = 2;

    /// Every attribute of the seeded item, which is what an unscoped read reports.
    const WHOLE_ITEM: &[&str] = &["pk", "pub", "secret", "sk"];
    /// The listed attributes, in the order a name-ordered read reports them.
    const LISTED: &[&str] = &["pk", "pub", "sk"];

    let rows: Vec<Row> = vec![
        Row {
            case: "the projected read the document admits",
            session: SCOPED,
            operation: "GetItem",
            request: json!({
                "TableName": TABLE,
                "Key": key_of(ALLOWED_PARTITION),
                "ProjectionExpression": projection(&VISIBLE_ATTRIBUTES),
                "ConsistentRead": true,
            }),
            served: Some(LISTED),
        },
        Row {
            // The selection a read with no projection implies is ALL_ATTRIBUTES, which the
            // clause over `dynamodb:Select` does not admit.
            case: "the same read with no projection, so its selection is every attribute",
            session: SCOPED,
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key_of(ALLOWED_PARTITION)}),
            served: None,
        },
        Row {
            case: "a projection naming an attribute the document does not list",
            session: SCOPED,
            operation: "GetItem",
            request: json!({
                "TableName": TABLE,
                "Key": key_of(ALLOWED_PARTITION),
                "ProjectionExpression": projection(&["pk", WITHHELD_ATTRIBUTE]),
            }),
            served: None,
        },
        Row {
            case: "the admitted projection on a partition the document does not list",
            session: SCOPED,
            operation: "GetItem",
            request: json!({
                "TableName": TABLE,
                "Key": key_of(OTHER_PARTITION),
                "ProjectionExpression": projection(&VISIBLE_ATTRIBUTES),
            }),
            served: None,
        },
        Row {
            // The legacy member the projection expression replaced states the same ask, so
            // the selection it implies is the same one.
            case: "the admitted names stated through AttributesToGet",
            session: SCOPED,
            operation: "GetItem",
            request: json!({
                "TableName": TABLE,
                "Key": key_of(ALLOWED_PARTITION),
                "AttributesToGet": VISIBLE_ATTRIBUTES,
            }),
            served: Some(LISTED),
        },
        Row {
            case: "a withheld name stated through AttributesToGet",
            session: SCOPED,
            operation: "GetItem",
            request: json!({
                "TableName": TABLE,
                "Key": key_of(ALLOWED_PARTITION),
                "AttributesToGet": ["pk", WITHHELD_ATTRIBUTE],
            }),
            served: None,
        },
        Row {
            case: "a query stating the admitted selection beside its projection",
            session: SCOPED,
            operation: "Query",
            request: json!({
                "TableName": TABLE,
                "KeyConditionExpression": "pk = :p",
                "ExpressionAttributeValues": {":p": {"S": ALLOWED_PARTITION}},
                "Select": "SPECIFIC_ATTRIBUTES",
                "ProjectionExpression": projection(&["pk", "pub"]),
            }),
            served: Some(&["pk", "pub"]),
        },
        Row {
            case: "a query stating the selection the document does not admit",
            session: SCOPED,
            operation: "Query",
            request: json!({
                "TableName": TABLE,
                "KeyConditionExpression": "pk = :p",
                "ExpressionAttributeValues": {":p": {"S": ALLOWED_PARTITION}},
                "Select": "ALL_ATTRIBUTES",
            }),
            served: None,
        },
        Row {
            case: "a query stating no selection and no projection",
            session: SCOPED,
            operation: "Query",
            request: json!({
                "TableName": TABLE,
                "KeyConditionExpression": "pk = :p",
                "ExpressionAttributeValues": {":p": {"S": ALLOWED_PARTITION}},
            }),
            served: None,
        },
        Row {
            case: "an admitted query against a partition the document does not list",
            session: SCOPED,
            operation: "Query",
            request: json!({
                "TableName": TABLE,
                "KeyConditionExpression": "pk = :p",
                "ExpressionAttributeValues": {":p": {"S": OTHER_PARTITION}},
                "Select": "SPECIFIC_ATTRIBUTES",
                "ProjectionExpression": projection(&["pk", "pub"]),
            }),
            served: None,
        },
        Row {
            case: "a batch read carrying the admitted projection",
            session: SCOPED,
            operation: "BatchGetItem",
            request: json!({
                "RequestItems": {
                    TABLE: {
                        "Keys": [key_of(ALLOWED_PARTITION)],
                        "ProjectionExpression": projection(&VISIBLE_ATTRIBUTES),
                    },
                },
            }),
            served: Some(LISTED),
        },
        Row {
            case: "a batch read carrying no projection",
            session: SCOPED,
            operation: "BatchGetItem",
            request: json!({
                "RequestItems": {TABLE: {"Keys": [key_of(ALLOWED_PARTITION)]}},
            }),
            served: None,
        },
        Row {
            // An action the document names no grant for is outside the scope whatever the
            // request carries: the condition keys never come into it.
            case: "a table walk, an action the document names no grant for",
            session: SCOPED,
            operation: "Scan",
            request: json!({"TableName": TABLE}),
            served: None,
        },
        Row {
            case: "a write, an action the document names no grant for",
            session: SCOPED,
            operation: "PutItem",
            request: json!({
                "TableName": TABLE,
                "Item": {"pk": {"S": ALLOWED_PARTITION}, "sk": {"S": "written"}},
            }),
            served: None,
        },
        Row {
            // The control: the same grant without the clause serves the very read the
            // conditioned document refused, whole item and all.
            case: "the unprojected read under the same grant with no condition",
            session: UNCONDITIONED,
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key_of(ALLOWED_PARTITION)}),
            served: Some(WHOLE_ITEM),
        },
        Row {
            case: "the other partition under the same grant with no condition",
            session: UNCONDITIONED,
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key_of(OTHER_PARTITION)}),
            served: Some(WHOLE_ITEM),
        },
        Row {
            // The second control: a session no mint scoped is capped by nothing, so the
            // role's own grant stands.
            case: "the unprojected read under a session no mint scoped",
            session: CAPLESS,
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key_of(ALLOWED_PARTITION)}),
            served: Some(WHOLE_ITEM),
        },
        Row {
            case: "the table walk under the same grant with no condition, which names no \
                   walk",
            session: UNCONDITIONED,
            operation: "Scan",
            request: json!({"TableName": TABLE}),
            served: None,
        },
        Row {
            // Nothing caps this session, so the role's own grant of every action of this
            // service reaches the walk the two scoped sessions were refused.
            case: "the table walk under a session no mint scoped",
            session: CAPLESS,
            operation: "Scan",
            request: json!({"TableName": TABLE}),
            served: Some(WHOLE_ITEM),
        },
    ];

    for row in &rows {
        let session = &sessions[row.session];
        let case = row.case;
        let (status, answer) = ddb(
            &router,
            &session.access_key_id,
            row.operation,
            &row.request,
        )
        .await;
        match row.served {
            Some(expected) => {
                assert_eq!(
                    status, 200,
                    "case {case:?} ({}): {} expected HTTP 200, got {status} {answer}",
                    session.label, row.operation
                );
                let names = served_attributes(row.operation, &answer);
                assert_eq!(
                    names, expected,
                    "case {case:?} ({}): the answer must carry exactly {expected:?}, got \
                     {names:?} in {answer}",
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
                let action = format!("dynamodb:{}", row.operation);
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

    // A refused read withheld the attribute; it did not remove it. The account's own
    // credential reads the whole item back, the withheld attribute included.
    let (status, stored) = ddb(
        &router,
        AMBIENT_KEY,
        "GetItem",
        &json!({"TableName": TABLE, "Key": key_of(ALLOWED_PARTITION)}),
    )
    .await;
    let names = served_attributes("GetItem", &stored);
    assert_eq!(
        status, 200,
        "case \"the stored item after the refused reads\": the account reads its own table, \
         got {status} {stored}"
    );
    assert_eq!(
        names, WHOLE_ITEM,
        "case \"the stored item after the refused reads\": the item must still carry \
         {WHOLE_ITEM:?}, got {names:?} in {stored}"
    );
    assert_eq!(
        stored["Item"][WITHHELD_ATTRIBUTE],
        json!({"S": WITHHELD_VALUE}),
        "case \"the stored item after the refused reads\": the withheld attribute is still \
         stored, got {stored}"
    );

    // And the refused write stored nothing: the refusal had no effect on the table.
    let (status, absent) = ddb(
        &router,
        AMBIENT_KEY,
        "GetItem",
        &json!({
            "TableName": TABLE,
            "Key": {"pk": {"S": ALLOWED_PARTITION}, "sk": {"S": "written"}},
        }),
    )
    .await;
    assert_eq!(
        (status, absent.get("Item").cloned()),
        (200, None),
        "case \"the refused write\": nothing was stored under the key it addressed, got \
         {status} {absent}"
    );
}
