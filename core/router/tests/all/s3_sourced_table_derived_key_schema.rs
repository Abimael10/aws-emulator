// SPDX-License-Identifier: CC0-1.0

//! A table keyed per uploaded object, over the ASSEMBLED router: the schema is read out of an
//! Amazon S3 object's header row and the write is then held to it by DynamoDB.
//!
//! RULE: a single-item write that carries an `Item` and gives no value to a key attribute the
//! table's `KeySchema` declares is refused HTTP 400 `ValidationException` whose message NAMES
//! that attribute — "One or more parameter values were invalid: Missing the key <name> in the
//! item" — with the one variable part taken from the table's own key schema and the submitted
//! item; when the item gives no value to either key of a composite schema the name stated is
//! the first element the schema declares, which `CreateTable` holds to be the `HASH` one.
//!
//! What this suite adds over the service crate's own
//! (`service-com-amazonaws-dynamodb-dynamodb-20120810/tests/all/s3_sourced_derived_key_schema.rs`,
//! which spans the class the rule generalises over — names, scalar types, arity, role, and the
//! secondary-index half) is the WIRE PATH: here the object is stored and read back over Amazon
//! S3's own REST surface and the table is driven over DynamoDB's JSON surface on ONE assembled
//! `MultiServiceRouter`, each request claimed by the signing name it is signed for. A client
//! that keys a table per uploaded object takes exactly that path, and only a suite with both
//! services mounted can walk it end to end.
//!
//! The rows vary WHICH declared key the item leaves out (the sort key, the partition key, both)
//! and the schema the object implies (two columns named in words; two spelled with a dash, a dot
//! and digits, the second numeric), and every row then writes the complete item the same object
//! implies and reads it back, so a refusal is known to be about the key the item omitted.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call of this suite is signed for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which every call here signs with. The signature
/// itself is never checked: this router runs with request authentication off.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The bucket the CSV objects are uploaded to (no underscore: `CreateBucket` refuses one).
const BUCKET: &str = "router-derived-schema-landing";

/// The clause every refusal of a parameter value in DynamoDB opens with.
const INVALID_PARAMETER: &str = "One or more parameter values were invalid";

/// How often to look while waiting for a created table to reach `ACTIVE`, and how many looks to
/// take. The span is DynamoDB's own, so nothing sleeps for a fixed stretch, and the deadline is
/// generous enough that a loaded host cannot decide the outcome.
const LOOK: std::time::Duration = std::time::Duration::from_millis(50);
const LOOKS: usize = 1_200;

/// The sentence a write states for a key attribute its `Item` gave no value for.
fn missing_the_key(attribute: &str) -> String {
    format!("{INVALID_PARAMETER}: Missing the key {attribute} in the item")
}

/// The two services this rule spans, over ONE engine, as `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(engine),
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

/// One call against Amazon S3's REST surface, path-style: the status and the body as text.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "text/csv")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// `PUT /<bucket>` — S3's `CreateBucket`.
async fn create_bucket(router: &MultiServiceRouter) {
    let (status, answered) = s3(router, "PUT", &format!("/{BUCKET}"), "").await;
    assert_eq!(
        status, 200,
        "setup: CreateBucket({BUCKET:?}) must succeed, got {status} with {answered}"
    );
}

/// Upload one CSV object and read it back over the same surface: the schema is derived from
/// what Amazon S3 serves, not from what this suite holds.
async fn stored_csv(router: &MultiServiceRouter, case: &str, key: &str, csv: &str) -> String {
    let (status, answered) = s3(router, "PUT", &format!("/{BUCKET}/{key}"), csv).await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutObject({key:?}) must succeed, got {status} with {answered}"
    );
    let (status, served) = s3(router, "GET", &format!("/{BUCKET}/{key}"), "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetObject({key:?}) must serve the object, got {status} with {served}"
    );
    served
}

/// One DynamoDB operation over the JSON 1.0 surface: the status and the parsed body.
async fn ddb(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("authorization", authorization("dynamodb"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the test body serializes"),
        ))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One DynamoDB operation that has to be served, answering its body.
async fn ddb_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, answered) = ddb(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must be served, got {status} with {answered}"
    );
    answered
}

/// The wire error code (`__type`) of a refusal, without the shape's namespace.
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

/// One column as a CSV object's header row and first data row declare it: the name, the scalar
/// type its datum is expressible as (a run of ASCII digits is `N`, anything else `S`), and the
/// datum.
fn columns(csv: &str) -> Vec<(String, String, String)> {
    let mut lines = csv.lines();
    let header = lines.next().unwrap_or_default();
    let data = lines.next().unwrap_or_default();
    let mut cells = data.split(',');
    header
        .split(',')
        .map(|name| {
            let value = cells.next().unwrap_or("0").trim().to_string();
            let numeric = !value.is_empty() && value.chars().all(|cell| cell.is_ascii_digit());
            (
                name.trim().to_string(),
                if numeric { "N" } else { "S" }.to_string(),
                value,
            )
        })
        .collect()
}

/// The item the object's data row is, with the columns named in `omitted` left out.
fn item_without(columns: &[(String, String, String)], omitted: &[&str]) -> Value {
    let mut item = Map::new();
    for (name, declared, value) in columns {
        if !omitted.contains(&name.as_str()) {
            let mut attribute = Map::new();
            attribute.insert(declared.clone(), Value::String(value.clone()));
            item.insert(name.clone(), Value::Object(attribute));
        }
    }
    Value::Object(item)
}

/// Create the table the header row implies — the first column its partition key, the second its
/// sort key — and wait out the documented `CREATING` -> `ACTIVE` transition by POLLING.
async fn create_derived_table(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    columns: &[(String, String, String)],
) {
    let key_schema: Vec<Value> = columns
        .iter()
        .zip(["HASH", "RANGE"])
        .map(|((name, _, _), role)| json!({"AttributeName": name, "KeyType": role}))
        .collect();
    let definitions: Vec<Value> = columns
        .iter()
        .take(key_schema.len())
        .map(|(name, declared, _)| json!({"AttributeName": name, "AttributeType": declared}))
        .collect();
    ddb_ok(
        router,
        case,
        "CreateTable",
        json!({
            "TableName": table,
            "KeySchema": key_schema,
            "AttributeDefinitions": definitions,
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let mut described = Value::Null;
    for _ in 0..LOOKS {
        let (_, answered) = ddb(router, "DescribeTable", json!({"TableName": table})).await;
        described = answered;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: DynamoDB must report {table:?} ACTIVE, got {described}");
}

#[tokio::test]
async fn a_write_is_held_to_the_schema_derived_from_its_s3_object() {
    let router = emulator();
    create_bucket(&router).await;

    // Each row: the object a client uploaded, the key columns its item gives no value for, and
    // the attribute the refusal must name.
    let rows: &[(&str, &str, &str, &[&str], &str)] = &[
        (
            "a two-column header, no value for the sort column",
            "inventory.csv",
            "item_id,warehouse_id\nsku-1,w-1\n",
            &["warehouse_id"],
            "warehouse_id",
        ),
        (
            "the same object, no value for the partition column",
            "inventory.csv",
            "item_id,warehouse_id\nsku-1,w-1\n",
            &["item_id"],
            "item_id",
        ),
        (
            "the same object, no value for either column",
            "inventory.csv",
            "item_id,warehouse_id\nsku-1,w-1\n",
            &["item_id", "warehouse_id"],
            "item_id",
        ),
        (
            "a dashed partition column and a numeric dotted sort column",
            "shipments.csv",
            "order-id,Line.Item_2\no-1,7\n",
            &["Line.Item_2"],
            "Line.Item_2",
        ),
    ];

    let mut derived: Vec<Vec<(String, String, String)>> = Vec::new();
    let mut created: Vec<String> = Vec::new();
    for (case, key, csv, _, _) in rows {
        let table = format!("derived-{}", key.strip_suffix(".csv").unwrap_or(key));
        let served = stored_csv(&router, case, key, csv).await;
        assert_eq!(
            served, *csv,
            "case {case:?}: expected S3 to serve the uploaded CSV back, got {served:?}"
        );
        let columns = columns(&served);
        if !created.contains(&table) {
            create_derived_table(&router, case, &table, &columns).await;
            created.push(table);
        }
        derived.push(columns);
    }

    for ((case, key, _, omitted, named), columns) in rows.iter().zip(&derived) {
        let table = format!("derived-{}", key.strip_suffix(".csv").unwrap_or(key));
        let (status, answered) = ddb(
            &router,
            "PutItem",
            json!({"TableName": table, "Item": item_without(columns, omitted)}),
        )
        .await;
        let expected = missing_the_key(named);
        let stated = (status, error_code(&answered), message_of(&answered));
        assert_eq!(
            stated,
            (400, "ValidationException".to_string(), expected.clone()),
            "case {case:?}: expected (400, ValidationException, {expected:?}), got {stated:?} \
             from {answered}"
        );
    }

    // Every derived schema accepts the complete item the same object implies, and reads it back:
    // the refusals above are about the key the item omitted, not about the write.
    for ((case, key, _, _, _), columns) in rows.iter().zip(&derived) {
        let table = format!("derived-{}", key.strip_suffix(".csv").unwrap_or(key));
        let item = item_without(columns, &[]);
        ddb_ok(
            &router,
            case,
            "PutItem",
            json!({"TableName": table, "Item": item.clone()}),
        )
        .await;
        let read = ddb_ok(
            &router,
            case,
            "GetItem",
            // Every column these objects declare is a key of the table, so the complete item IS
            // its own `Key`.
            json!({"TableName": table, "Key": item.clone()}),
        )
        .await;
        assert_eq!(
            read["Item"], item,
            "case {case:?}: expected the complete derived item back, got {read}"
        );
    }
}
