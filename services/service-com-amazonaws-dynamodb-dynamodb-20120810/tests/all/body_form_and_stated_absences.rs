// SPDX-License-Identifier: CC0-1.0

//! What this service makes of a request's BODY, ahead of every value in it: whether the
//! bytes are a document an input can be read from, and what an explicit `null` in that
//! document states.
//!
//! Every suite here is a TABLE spanning what its rule generalises over rather than one
//! request, and every assertion names its case and the value it actually read.
//!
//! RULE 1 — a body that is not ONE complete JSON object is no body of this protocol: the
//! protocol carries an operation's input as a single JSON object, so bytes that do not parse,
//! bytes carrying anything after that object, and a document that is not an object are a
//! defect of the BODY rather than of one value in it. There is then no value whose class
//! could be named, so the refusal is the protocol's own `SerializationException` and NO prose
//! at all — in neither the capitalized slot a value-level refusal of this class states its
//! sentence in nor the lowercase one a modelled exception uses — and nothing of the request
//! is read, looked up or applied. The rows span how the bytes fail to be one object (not JSON
//! at all, truncated, content after the object, a second object, a document that is not an
//! object, no body at all) and the operation that carries them; the controls are the bodies
//! that ARE one object, including one whose every member is unknown, and a document that
//! nests deeper than the reader descends, which is a bound on a VALUE and keeps the
//! constraint refusal.
//!
//! RULE 2 — an explicit `null` states the ABSENCE of the member it stands for, so a request
//! carrying one is answered exactly as the same request without that member. The rows span
//! the modelled type of the member (a string, an integer, a boolean, an enumerated value, a
//! list, a map, a structure), the operation, the DEPTH the `null` stands at (a top-level
//! member, a member of a nested structure, a member of a structure inside a map), and the
//! unknown members beside it, which the protocol ignores the same way.
//!
//! RULE 3 — the one `null` that is not an absence a caller may send is one standing for a
//! member the input REQUIRES. On the VECTOR-SEARCH surface there is then no value to build the
//! input from, so the body fails to deserialize and is refused as that body-level failure with
//! the reader's own diagnosis of it — the class found, the value expected and the position in
//! the body the read stopped at, which is read from the bytes the caller sent rather than
//! fixed. The rows span the modelled type of the required member, the depth the `null` stands
//! at, what else is wrong with the same request, and the length of the body before the `null`,
//! which is what the position is read from. Every OTHER operation answers such a `null` as the
//! CONSTRAINT report it is, which is `required_null_member_reports`'s rule.
//!
//! RULE 4 — how many absences a body states is the caller's choice, so asking what each one
//! earns is bounded by how much JSON that costs to read, while APPLYING them is not: a body
//! dense with absences is still answered as the same body without them, and a request the
//! bound stops this layer asking about is never SERVED where it would have been refused.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, ok, REGION};
use crate::table_items::create_active_table;

/// The error class a body this service could not read an input from is refused with, as the
/// wire spells it: the absolute id of the protocol's own error shape, which is what an SDK
/// branches a non-retryable marshalling failure on.
const SERIALIZATION_EXCEPTION: &str = "com.amazon.coral.service#SerializationException";

/// The error class a value that WAS read and then broke a rule the model declares is refused
/// with — the other side of the boundary RULE 1 and RULE 3 are about.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The JSON literal a body states a member's absence with.
const NULL_TOKEN: &str = "null";

/// The table the rows address when they address a real one.
const TABLE: &str = "Absences";

/// Send one request with `body` as its bytes exactly — a suite about malformed bodies cannot
/// go through a JSON encoder, because an encoder only emits bodies that parse.
async fn send(router: &MultiServiceRouter, operation: &str, body: &str) -> (u16, Value) {
    let (status, _, parsed) = send_asking_in(router, operation, body, PROTOCOL_MEDIA_TYPE).await;
    (status, parsed)
}

/// The same, naming the API under `media_type` and reading back the media type the response
/// announces itself in.
async fn send_asking_in(
    router: &MultiServiceRouter,
    operation: &str,
    body: &str,
    media_type: &str,
) -> (u16, Option<String>, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", media_type)
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/dynamodb/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::copy_from_slice(body.as_bytes()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let announced = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, announced, parsed)
}

/// The media type this service's `awsJson1_0` protocol stamps on its responses.
const PROTOCOL_MEDIA_TYPE: &str = "application/x-amz-json-1.0";

/// The plain JSON media type the same endpoint also serves its API under.
const JSON_MEDIA_TYPE: &str = "application/json";

/// The class a refusal states, as the wire spells it.
fn class(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The prose a refusal of the serialization class states, read from the lowercase slot the
/// reader's own diagnosis of a body is carried in.
fn diagnosis(body: &Value) -> Option<&str> {
    body.get("message").and_then(Value::as_str)
}

// ===========================================================================
// RULE 1. A body that is not one complete JSON object
// ===========================================================================

#[tokio::test]
async fn a_body_that_is_not_one_json_object_is_refused_with_no_prose_at_all() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    // (case, operation, the bytes the request carries)
    let rows: Vec<(&str, &str, &str)> = vec![
        ("bytes that are not JSON at all", "ListTables", "not json"),
        ("a document of another format", "ListTables", "<ListTables/>"),
        ("no body at all", "ListTables", ""),
        ("whitespace alone", "ListTables", "   "),
        ("a truncated object", "ListTables", r#"{"Limit":"#),
        ("an object missing its closing brace", "DescribeTable", r#"{"TableName":"Absences""#),
        ("an unterminated string", "DescribeTable", r#"{"TableName":"Absen"#),
        ("a trailing comma", "ListTables", r#"{"Limit":1,}"#),
        ("a member with no value", "ListTables", r#"{"Limit":}"#),
        ("an unquoted member name", "ListTables", "{Limit:1}"),
        ("content after a complete object", "ListTables", r#"{"Limit":1} and more"#),
        ("a second object after the first", "ListTables", "{}{}"),
        ("a top-level array", "ListTables", "[]"),
        ("a top-level array of objects", "ListTables", r#"[{"Limit":1}]"#),
        ("a top-level string", "ListTables", r#""{}""#),
        ("a top-level number", "DescribeTable", "7"),
        ("a top-level boolean", "ListTables", "true"),
        ("a top-level null", "ListTables", NULL_TOKEN),
        (
            "content after a complete object, on a write",
            "PutItem",
            r#"{"TableName":"Absences","Item":{"pk":{"S":"written"}}} trailing"#,
        ),
    ];

    for (case, operation, body) in rows {
        let (status, answered) = send(&router, operation, body).await;
        assert_eq!(
            (status, class(&answered).as_str()),
            (400, SERIALIZATION_EXCEPTION),
            "case {case:?}: expected 400 {SERIALIZATION_EXCEPTION}, got {status} with {answered}"
        );
        assert_eq!(
            answered,
            json!({"__type": SERIALIZATION_EXCEPTION}),
            "case {case:?}: this refusal names the class and states no prose, got {answered}"
        );
    }

    // Nothing of a refused request was read, so nothing of it was applied: the write above
    // addressed a real table, which holds no item afterwards.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "written"}}}),
    )
    .await;
    assert_eq!(
        read,
        json!({}),
        "case \"after the refusals\": a refused write stored nothing, got {read}"
    );

    // This refusal is a document of this API like any other, so it announces the media type
    // the REQUEST named the API under — the one thing a client or a proxy keys the error
    // envelope on.
    for media_type in [JSON_MEDIA_TYPE, PROTOCOL_MEDIA_TYPE] {
        let (status, announced, answered) =
            send_asking_in(&router, "ListTables", "not json", media_type).await;
        assert_eq!(
            (status, announced.as_deref()),
            (400, Some(media_type)),
            "case \"a body-form refusal asked for in {media_type}\": the refusal is announced in \
             the media type the request named, got {status} announcing {announced:?} with \
             {answered}"
        );
    }

    // The CONTROLS, on the other side of the boundary: every one of these bodies IS one
    // complete JSON object, so what answers them is the operation or the rule they break.
    for (case, operation, body) in [
        ("the empty object", "ListTables", "{}"),
        ("an object with whitespace around it", "ListTables", " \n {} \t "),
        ("members the model does not declare", "ListTables", r#"{"Unknown":1,"Newer":"x"}"#),
        (
            "a member whose value is a nested document",
            "ListTables",
            r#"{"Unknown":{"nested":[1,2,{"deeper":true}]}}"#,
        ),
    ] {
        let (status, answered) = send(&router, operation, body).await;
        assert_eq!(
            (status, answered.clone()),
            (200, json!({"TableNames": [TABLE]})),
            "case {case:?}: expected the request to be served, got {status} with {answered}"
        );
    }

    // A document that nests deeper than the reader descends is a document: how deep a value a
    // request carries may nest is a bound on that VALUE, so it keeps the constraint class.
    let deep = format!(
        r#"{{"TableName":"{TABLE}","Item":{{"pk":{{"S":"deep"}},"v":{{"L":{}[]{}}}}}}}"#,
        r#"[{"L":"#.repeat(80),
        "}]".repeat(80)
    );
    let (status, answered) = send(&router, "PutItem", &deep).await;
    assert_eq!(
        (status, class(&answered).as_str()),
        (400, VALIDATION_EXCEPTION),
        "case \"a value nested past what the reader descends\": the nesting bound is a rule \
         about the value, got {status} with {answered}"
    );
}

// ===========================================================================
// RULE 2. An explicit null states the absence of its member
// ===========================================================================

#[tokio::test]
async fn an_explicit_null_states_the_absence_of_the_member_it_stands_for() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}, "note": {"S": "kept"}}}),
    )
    .await;

    // (case, operation, the body stating absences, the same body without those members)
    //
    // The modelled type of each member named here comes from
    // `smithy_query(query="operation", operation="<the operation>")`:
    // `ExclusiveStartTableName` targets `TableName` (string), `Limit` an integer,
    // `ConsistentRead` a boolean, `Select`/`ReturnConsumedCapacity` enumerated strings,
    // `AttributesToGet` a list, `ExpressionAttributeNames`/`ScanFilter`/`ExclusiveStartKey`
    // maps, and `Key`'s values are `AttributeValue` structures.
    let rows: Vec<(&str, &str, Value, Value)> = vec![
        (
            "a string and an integer, both absent",
            "ListTables",
            json!({"ExclusiveStartTableName": null, "Limit": null}),
            json!({}),
        ),
        (
            "an absence beside members the model does not declare",
            "ListTables",
            json!({"Limit": null, "Unknown": 1, "AlsoUnknown": null}),
            json!({}),
        ),
        (
            "an absence beside a value that is stated",
            "ListTables",
            json!({"ExclusiveStartTableName": null, "Limit": 10}),
            json!({"Limit": 10}),
        ),
        (
            "a boolean, an integer and an enumerated value, all absent",
            "Scan",
            json!({"TableName": TABLE, "ConsistentRead": null, "Limit": null, "Select": null}),
            json!({"TableName": TABLE}),
        ),
        (
            "a list and a map, both absent",
            "Scan",
            json!({"TableName": TABLE, "AttributesToGet": null, "ScanFilter": null,
                   "ExclusiveStartKey": null}),
            json!({"TableName": TABLE}),
        ),
        (
            "a map and a string, both absent, on a read",
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "a"}},
                   "ExpressionAttributeNames": null, "ProjectionExpression": null}),
            json!({"TableName": TABLE, "Key": {"pk": {"S": "a"}}}),
        ),
        (
            "an absence inside a structure the request carries",
            "Scan",
            json!({"TableName": TABLE, "Select": "COUNT",
                   "ExclusiveStartKey": {"pk": {"S": "a", "N": null}}}),
            json!({"TableName": TABLE, "Select": "COUNT",
                   "ExclusiveStartKey": {"pk": {"S": "a"}}}),
        ),
        (
            "an absence inside a structure in a map, on a write",
            "PutItem",
            json!({"TableName": TABLE,
                   "Item": {"pk": {"S": "b"}, "note": {"S": "stored", "B": null}},
                   "ReturnConsumedCapacity": null}),
            json!({"TableName": TABLE,
                   "Item": {"pk": {"S": "b"}, "note": {"S": "stored"}}}),
        ),
    ];

    for (case, operation, stating, without) in rows {
        let (stated_status, stated) = call(&router, operation, stating.clone()).await;
        let (plain_status, plain) = call(&router, operation, without.clone()).await;
        assert_eq!(
            (stated_status, &stated),
            (plain_status, &plain),
            "case {case:?}: {stating} must be answered exactly as {without}, got {stated_status} \
             with {stated} against {plain_status} with {plain}"
        );
        assert_eq!(
            stated_status, 200,
            "case {case:?}: the request states no member a rule refuses, got {stated_status} \
             with {stated}"
        );
    }

    // A whole optional STRUCTURE and a whole optional LIST stated absent, on a create: the
    // table is made, which is what proves the members were read as absent rather than as
    // values of the wrong class.
    let made = ok(
        &router,
        "CreateTable",
        json!({"TableName": "StatedAbsent", "BillingMode": "PAY_PER_REQUEST",
               "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
               "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
               "ProvisionedThroughput": null, "StreamSpecification": null,
               "Tags": null, "LocalSecondaryIndexes": null, "DeletionProtectionEnabled": null}),
    )
    .await;
    assert_eq!(
        made["TableDescription"]["TableName"],
        json!("StatedAbsent"),
        "case \"a structure and a list stated absent\": the create is served, got {made}"
    );
}

// ===========================================================================
// RULE 3. A null standing for a member the input requires
// ===========================================================================

/// The sentence the reader states for a `null` where it expected `expected`, in `body`.
///
/// The position is the reader's own count of the bytes the caller sent: it stops with the
/// token consumed, on the line it is on, so the column is the 1-based index of that token's
/// last byte. The bodies below are one line and carry the text `null` exactly once, so that
/// index is computed here rather than written down — which is what makes the case about the
/// REQUEST's own offset.
fn reader_diagnosis(body: &str, expected: &str) -> String {
    let at = body
        .find(NULL_TOKEN)
        .expect("the body states exactly one absence");
    assert!(
        !body[at + NULL_TOKEN.len()..].contains(NULL_TOKEN),
        "the body states exactly one absence, got {body}"
    );
    assert!(
        !body.contains('\n'),
        "the body is one line, got {body}"
    );
    let column = at + NULL_TOKEN.len();
    format!("invalid type: null, expected {expected} at line 1 column {column}")
}

/// A `SearchVectors` body carrying every member the input requires, with `member` stated as
/// absent and the table name as long as `pad` makes it — the one knob the position the
/// diagnosis names is read from.
///
/// The members and which of them are required come from
/// `smithy_query(query="operation", operation="SearchVectors")`: `TableName` and `IndexName`
/// are required strings, `SearchVector` a required list and `TopK` a required integer.
fn search_vectors(member: &str, pad: usize) -> String {
    let mut body = json!({
        "TableName": "T".repeat(pad),
        "IndexName": "by-vector",
        "SearchVector": [{"N": "1"}],
        "TopK": 3,
    });
    body[member] = Value::Null;
    serde_json::to_string(&body).expect("the body serializes")
}

#[tokio::test]
async fn a_null_a_required_member_stands_for_is_refused_with_the_readers_diagnosis() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    // (case, operation, the bytes the request carries, what the reader expected there)
    //
    // Every row is the one surface observed to answer such a `null` this way
    // (`src/service/request_layer.rs`'s reader-diagnosis operations). The expectation is the
    // reader's own for the Rust type the generated input reads that member into
    // (`src/ops/types`: `SearchVectorsInput.table_name: String`, `.search_vector:
    // Vec<AttributeValue>`, `.top_k: i32`) — named in words for a kind of value and by the
    // type itself for a numeric primitive, which is that reader's vocabulary rather than a
    // choice made here.
    let rows: Vec<(&str, &str, String, &str)> = vec![
        (
            "a required string, on a read of a vector index",
            "SearchVectors",
            search_vectors("IndexName", 4),
            "a string",
        ),
        (
            "the table name, which every operation requires",
            "SearchVectors",
            search_vectors("TableName", 4),
            "a string",
        ),
        (
            "a required list",
            "SearchVectors",
            search_vectors("SearchVector", 4),
            "a sequence",
        ),
        (
            "a required integer",
            "SearchVectors",
            search_vectors("TopK", 4),
            "i32",
        ),
        (
            // The same absence in a request that ALSO omits a required member: the read
            // stops at the `null` before any member's absence is noticed.
            "a required string, in a request that omits another required member",
            "SearchVectors",
            json!({"TableName": "T", "IndexName": null, "TopK": 3}).to_string(),
            "a string",
        ),
        (
            "a required list, in a request that omits every other required member",
            "SearchVectors",
            json!({"SearchVector": null}).to_string(),
            "a sequence",
        ),
        (
            // The position is read from the BODY, so the same member stated absent in a
            // longer body is named at a later column.
            "the same absence further into the body",
            "SearchVectors",
            search_vectors("IndexName", 40),
            "a string",
        ),
        (
            "the same absence further into the body again",
            "SearchVectors",
            search_vectors("IndexName", 120),
            "a string",
        ),
    ];

    for (case, operation, body, expected) in rows {
        let (status, answered) = send(&router, operation, &body).await;
        assert_eq!(
            (status, class(&answered).as_str()),
            (400, SERIALIZATION_EXCEPTION),
            "case {case:?}: expected 400 {SERIALIZATION_EXCEPTION}, got {status} with {answered}"
        );
        let stated = reader_diagnosis(&body, expected);
        assert_eq!(
            diagnosis(&answered),
            Some(stated.as_str()),
            "case {case:?}: expected the reader's diagnosis {stated:?} of {body}, got {answered}"
        );
        assert!(
            answered.get("Message").is_none(),
            "case {case:?}: this refusal states its prose in the lowercase slot alone, got \
             {answered}"
        );
    }

    // Nothing of a refused request was read, so nothing of it was applied: the table the rows
    // addressed through its name is still exactly as it was, and the vector read the rows
    // carried created no index on it.
    let described = ok(&router, "DescribeTable", json!({"TableName": TABLE})).await;
    assert!(
        described["Table"].get("VectorIndexes").is_none(),
        "case \"after the refusals\": a refused vector read created nothing, got {described}"
    );
    assert_eq!(
        described["Table"]["TableStatus"],
        json!("ACTIVE"),
        "case \"after the refusals\": a refused request left the table as it was, got {described}"
    );
}

// ===========================================================================
// RULE 4. How many absences a body states is the caller's choice
// ===========================================================================

/// One body stating `absences` absences on members the model does not declare, plus whatever
/// `carried` states — the shape a caller controls the COUNT of.
fn dense_with_absences(absences: usize, carried: Value) -> Value {
    let mut body = serde_json::Map::new();
    for which in 0..absences {
        body.insert(format!("Unknown{which}"), Value::Null);
    }
    if let Some(members) = carried.as_object() {
        for (name, held) in members {
            body.insert(name.clone(), held.clone());
        }
    }
    Value::Object(body)
}

#[tokio::test]
async fn a_body_dense_with_absences_is_applied_whether_or_not_it_is_asked_about() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    // (case, how many absences the body states, the operation, what else it carries, the
    // answer it must be served)
    //
    // The counts span both sides of the bound on how much of a body this service reads to ask
    // what its absences earn: a few absences are asked about one by one, and a body dense with
    // them is applied without being asked about. Either way the absences are APPLIED, which is
    // what each row reads back.
    let rows: Vec<(&str, usize, &str, Value, Value)> = vec![
        (
            "a handful of absences, well inside the bound",
            4,
            "ListTables",
            json!({}),
            json!({"TableNames": [TABLE]}),
        ),
        (
            "absences beside a value the request states",
            30,
            "ListTables",
            json!({"Limit": 10}),
            json!({"TableNames": [TABLE]}),
        ),
        (
            "a body dense with absences, past the bound",
            4_000,
            "ListTables",
            json!({}),
            json!({"TableNames": [TABLE]}),
        ),
        (
            "a body dense with absences beside a value the request states",
            4_000,
            "ListTables",
            json!({"Limit": 10}),
            json!({"TableNames": [TABLE]}),
        ),
        (
            "a body dense with absences, on a read of an item",
            4_000,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "absent"}}}),
            json!({}),
        ),
    ];

    for (case, absences, operation, carried, expected) in rows {
        let body = dense_with_absences(absences, carried);
        let (status, answered) = call(&router, operation, body).await;
        assert_eq!(
            (status, &answered),
            (200, &expected),
            "case {case:?}: {absences} stated absences are applied, so the request is answered \
             {expected}, got {status} with {answered}"
        );
    }

    // And the bound never turns a refusal into a success: a body dense enough that this layer
    // does not ask what its absences earn, stating a member the input REQUIRES as absent, is
    // still refused — by the reader's own account of the member the input did not get.
    let refused = dense_with_absences(4_000, json!({"TableName": TABLE, "Key": null}));
    let (status, answered) = call(&router, "GetItem", refused).await;
    assert_eq!(
        status, 400,
        "case \"a required member stated absent in a body dense with absences\": the request is \
         refused rather than served, got {status} with {answered}"
    );
}
