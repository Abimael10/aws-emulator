// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the PartiQL SEMANTIC layer: which SOURCE a statement may address,
//! what an index-qualified `SELECT` reports, which operands a `SET` clause may join, and
//! what one slot of a `BatchExecuteStatement` reports when its statement never ran.
//!
//! The rules asserted here, each over the class it generalises over rather than one
//! statement:
//!
//! * a statement addresses items by the equalities its clause pins on ONE partition key
//!   and ONE sort key, so a source keyed by several `HASH` or several `RANGE` attributes
//!   cannot be addressed at all and is refused for the SOURCE, before any clause of the
//!   statement is judged;
//! * what an index-qualified read REPORTS follows the index KIND: a local secondary index
//!   fetches from the parent table what it does not project, a global one cannot and
//!   refuses a projection naming such an attribute, and `SELECT *` through either reports
//!   the index's own entry;
//! * a source qualifier naming no index of the table is refused for that name;
//! * a filter on an attribute the index does not project is refused where the clause states
//!   a condition on the index's partition key — the query the read is served as — and
//!   evaluated against the entry, matching nothing, where the read walks the whole index;
//! * a `SET` clause joins two operands with `+` or `-` and applies no other arithmetic; a
//!   `[index]` step addresses a position of a list; and a bag denotes a set whose members
//!   are all of one type;
//! * a batch slot carries the table its statement REACHED, so a statement refused on its
//!   own merits carries the error alone while one that ran — or that failed its condition
//!   against a stored item — names the table.
//!
//! Every statement goes over the wire (`POST /` with `X-Amz-Target:
//! DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter` the
//! emulator's front doors use, and every positioned refusal is COMPUTED from the statement
//! text so a row asserts the rule rather than a transcribed number.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table the index rows address: a partition and a sort key, with one index of every
/// kind this surface has to tell apart.
const TABLE: &str = "PartiqlIndexSources";

/// The global secondary index that projects ONE non-key attribute (`proj_attr`) and not
/// the other (`not_proj_attr`).
const GSI: &str = "gsi1";

/// The KEYS_ONLY local secondary index: it projects the table's keys and its own, so an
/// attribute named through it is fetched from the parent table.
const LSI: &str = "lsi1";

/// The global secondary index whose key schema holds TWO `HASH` elements — "Each global
/// secondary index supports up to 4 partition keys and up to 4 sort keys"
/// (`CreateTable$GlobalSecondaryIndexes`).
const MULTI_HASH_GSI: &str = "multi-hash-gsi";

/// The vector index beside them, which is searched rather than read by a statement.
const VECTOR_INDEX: &str = "vec-idx";

/// The sentence a statement addressed at a source keyed over several attributes is refused
/// with.
const MULTI_ATTRIBUTE_MESSAGE: &str = "PartiQL statements cannot be executed on tables with \
                                       multi-attribute HASH keys or multi-attribute RANGE keys.";

/// Create the table every index row addresses and seed two items that reach every index.
async fn indexed(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "S"},
                {"AttributeName": "gpk", "AttributeType": "S"},
                {"AttributeName": "gsk", "AttributeType": "S"},
                {"AttributeName": "lsk", "AttributeType": "S"},
                {"AttributeName": "mpk", "AttributeType": "S"},
            ],
            "GlobalSecondaryIndexes": [
                {
                    "IndexName": GSI,
                    "KeySchema": [
                        {"AttributeName": "gpk", "KeyType": "HASH"},
                        {"AttributeName": "gsk", "KeyType": "RANGE"},
                    ],
                    "Projection": {
                        "ProjectionType": "INCLUDE",
                        "NonKeyAttributes": ["proj_attr"],
                    },
                },
                {
                    "IndexName": MULTI_HASH_GSI,
                    "KeySchema": [
                        {"AttributeName": "gpk", "KeyType": "HASH"},
                        {"AttributeName": "mpk", "KeyType": "HASH"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
            ],
            "LocalSecondaryIndexes": [{
                "IndexName": LSI,
                "KeySchema": [
                    {"AttributeName": "pk", "KeyType": "HASH"},
                    {"AttributeName": "lsk", "KeyType": "RANGE"},
                ],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            }],
            "VectorIndexes": [{
                "IndexName": VECTOR_INDEX,
                "VectorAttribute": {"AttributeName": "emb"},
                "Dimensions": 2,
                "DistanceFunction": "COSINE",
                "Projection": {"ProjectionType": "ALL"},
            }],
        }),
    )
    .await;
    describe_when_active(router, "the indexed table", TABLE).await;
    for (pk, gsk, proj, extra) in [("Pa", "a", "match", "first"), ("Pb", "b", "other", "second")] {
        ok(
            router,
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {
                    "pk": {"S": pk},
                    "sk": {"S": "only"},
                    "gpk": {"S": "g1"},
                    "gsk": {"S": gsk},
                    "lsk": {"S": gsk},
                    "mpk": {"S": "m1"},
                    "proj_attr": {"S": proj},
                    "not_proj_attr": {"S": "hidden"},
                    "extra": {"S": extra},
                    "emb": {"L": [{"N": "1"}, {"N": "0"}]},
                },
            }),
        )
        .await;
    }
}

/// Run one statement that must be SERVED, answering the response.
async fn served(router: &MultiServiceRouter, case: &str, statement: &str) -> Value {
    let (status, answered) = call(
        router,
        "ExecuteStatement",
        json!({"Statement": statement}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: expected the statement to be served, got {status} with {answered}"
    );
    answered
}

/// Run one statement that must be REFUSED, answering the message it was refused with.
async fn refused(router: &MultiServiceRouter, case: &str, statement: &str) -> String {
    let (status, answered) = call(
        router,
        "ExecuteStatement",
        json!({"Statement": statement}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case}: expected the statement to be refused, got {status} with {answered}"
    );
    assert_eq!(
        error_code(&answered),
        "ValidationException",
        "case {case}: a statement this surface will not run is the request's own defect, got \
         {answered}"
    );
    message(&answered)
}

/// The items one served statement reported.
fn items(answered: &Value) -> Vec<Value> {
    answered["Items"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn a_source_keyed_over_several_attributes_is_refused_before_any_clause() {
    let router = emulator();
    indexed(&router).await;

    // The rows differ in the CLAUSE the statement states — one that pins the index's whole
    // key, one that pins part of it, one that pins none of it, and one that states none at
    // all — because the refusal is about the SOURCE and therefore the same for every one of
    // them. The sibling index of the same table, keyed over one attribute each way, is
    // served, so the refusal is about the key schema and not about the qualifier.
    for (case, statement) in [
        (
            "both HASH attributes pinned",
            format!(
                "SELECT * FROM \"{TABLE}\".\"{MULTI_HASH_GSI}\" WHERE gpk = 'g1' AND mpk = 'm1'"
            ),
        ),
        (
            "one HASH attribute pinned",
            format!("SELECT * FROM \"{TABLE}\".\"{MULTI_HASH_GSI}\" WHERE gpk = 'g1'"),
        ),
        (
            "a clause pinning no key attribute",
            format!("SELECT * FROM \"{TABLE}\".\"{MULTI_HASH_GSI}\" WHERE proj_attr = 'match'"),
        ),
        (
            "no clause at all",
            format!("SELECT * FROM \"{TABLE}\".\"{MULTI_HASH_GSI}\""),
        ),
        (
            "a projection naming an attribute",
            format!("SELECT proj_attr FROM \"{TABLE}\".\"{MULTI_HASH_GSI}\" WHERE gpk = 'g1'"),
        ),
    ] {
        let answered = refused(&router, case, &statement).await;
        assert_eq!(
            answered, MULTI_ATTRIBUTE_MESSAGE,
            "case {case}: expected the multi-attribute-key sentence, got {answered:?}"
        );
    }

    // ...and the single-attribute index of the same table answers the statement.
    let served = served(
        &router,
        "the single-attribute index beside it",
        &format!("SELECT * FROM \"{TABLE}\".\"{GSI}\" WHERE gpk = 'g1' AND gsk = 'a'"),
    )
    .await;
    assert_eq!(
        items(&served).len(),
        1,
        "case the single-attribute index beside it: expected the one addressed entry, got \
         {served}"
    );
}

#[tokio::test]
async fn a_batch_slot_addressed_at_a_multi_attribute_key_reports_the_source_refusal() {
    let router = emulator();
    indexed(&router).await;

    let (status, answered) = call(
        &router,
        "BatchExecuteStatement",
        json!({"Statements": [
            {"Statement": format!(
                "SELECT * FROM \"{TABLE}\".\"{MULTI_HASH_GSI}\" WHERE gpk = 'g1' AND mpk = 'm1'"
            )},
            {"Statement": format!("SELECT * FROM \"{TABLE}\" WHERE proj_attr = 'match'")},
            {"Statement": format!(
                "SELECT * FROM \"{TABLE}\" WHERE pk = 'Pa' AND (sk = 'only' OR sk = 'other')"
            )},
            {"Statement": format!(
                "INSERT INTO \"{TABLE}\" VALUE {{'pk': 'Pz', 'sk': 'only'}} RETURNING ALL OLD *"
            )},
        ]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "a batch of statements that each fail on their own merits still answers 200, got \
         {status} with {answered}"
    );

    // Each slot states the rule ITS statement broke, and none of them reached a table, so
    // none carries `TableName`.
    for (at, (case, expected)) in [
        (
            "the multi-attribute-key source",
            MULTI_ATTRIBUTE_MESSAGE.to_string(),
        ),
        (
            "a read that pins no key",
            "Select statements within BatchExecuteStatement must specify the primary key in the \
             where clause."
                .to_string(),
        ),
        (
            "a clause whose OR branches overlap on the sort key",
            "Overlapping conditions with range keys are not supported in where clause".to_string(),
        ),
        (
            "an INSERT asking to see an image",
            "RETURNING is not supported in INSERT".to_string(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let slot = &answered["Responses"][at];
        assert_eq!(
            slot["Error"]["Code"].as_str(),
            Some("ValidationError"),
            "case {case}: expected the per-slot validation code, got {slot}"
        );
        assert_eq!(
            slot["Error"]["Message"].as_str(),
            Some(expected.as_str()),
            "case {case}: expected the sentence for that rule, got {slot}"
        );
        assert!(
            slot.get("TableName").is_none(),
            "case {case}: a statement refused before it ran reached no table, got {slot}"
        );
    }

    // The INSERT the last slot refused wrote nothing: the slot reported a refusal, so the
    // item it carried is not stored.
    let (status, read) = call(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "Pz"}, "sk": {"S": "only"}}}),
    )
    .await;
    assert_eq!(
        (status, read.get("Item").cloned()),
        (200, None),
        "a refused INSERT stores nothing, got {read}"
    );
}

#[tokio::test]
async fn a_batch_slot_that_reached_its_table_names_it() {
    let router = emulator();
    indexed(&router).await;

    let (status, answered) = call(
        &router,
        "BatchExecuteStatement",
        json!({"Statements": [
            {"Statement": format!(
                "UPDATE \"{TABLE}\" SET proj_attr = 'written' WHERE pk = 'Pa' AND sk = 'only'"
            )},
            {"Statement": format!(
                "UPDATE \"{TABLE}\" SET proj_attr = 'nowhere' WHERE pk = 'absent' AND sk = 'only'"
            )},
        ]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 for a batch of writes, got {status} with {answered}"
    );
    for (at, (case, code)) in [
        ("a write that ran", None),
        ("a write whose condition no stored item met", Some("ConditionalCheckFailed")),
    ]
    .into_iter()
    .enumerate()
    {
        let slot = &answered["Responses"][at];
        assert_eq!(
            slot["Error"]["Code"].as_str(),
            code,
            "case {case}: expected Error.Code {code:?}, got {slot}"
        );
        assert_eq!(
            slot["TableName"].as_str(),
            Some(TABLE),
            "case {case}: a slot whose statement reached the table names it, got {slot}"
        );
    }
}

#[tokio::test]
async fn what_an_index_qualified_select_reports_follows_the_index_kind() {
    let router = emulator();
    indexed(&router).await;

    // A LOCAL index projects its keys only, so an attribute named through it is FETCHED
    // from the parent table; the rows differ in which unprojected attribute is named and
    // in how many of them the list holds.
    for (case, statement, expected) in [
        (
            "one unprojected attribute through the local index",
            format!("SELECT extra FROM \"{TABLE}\".\"{LSI}\" WHERE pk = 'Pa' AND lsk = 'a'"),
            json!([{"extra": {"S": "first"}}]),
        ),
        (
            "two unprojected attributes through the local index",
            format!(
                "SELECT extra, not_proj_attr FROM \"{TABLE}\".\"{LSI}\" WHERE pk = 'Pa' AND \
                 lsk = 'a'"
            ),
            json!([{"extra": {"S": "first"}, "not_proj_attr": {"S": "hidden"}}]),
        ),
        (
            "a projected key attribute and an unprojected one together",
            format!("SELECT lsk, extra FROM \"{TABLE}\".\"{LSI}\" WHERE pk = 'Pb'"),
            json!([{"lsk": {"S": "b"}, "extra": {"S": "second"}}]),
        ),
        (
            "an attribute the local index does project",
            format!("SELECT lsk FROM \"{TABLE}\".\"{LSI}\" WHERE pk = 'Pa' AND lsk = 'a'"),
            json!([{"lsk": {"S": "a"}}]),
        ),
    ] {
        let answered = served(&router, case, &statement).await;
        assert_eq!(
            answered["Items"], expected,
            "case {case}: expected {expected}, got {answered}"
        );
    }

    // `SELECT *` names no attribute in particular, so either kind of index reports the
    // ENTRY it holds and nothing it does not project.
    for (case, statement, expected) in [
        (
            "every attribute through the local index",
            format!("SELECT * FROM \"{TABLE}\".\"{LSI}\" WHERE pk = 'Pa' AND lsk = 'a'"),
            json!([{"pk": {"S": "Pa"}, "sk": {"S": "only"}, "lsk": {"S": "a"}}]),
        ),
        (
            "every attribute through the global index",
            format!("SELECT * FROM \"{TABLE}\".\"{GSI}\" WHERE gpk = 'g1' AND gsk = 'a'"),
            json!([{
                "pk": {"S": "Pa"},
                "sk": {"S": "only"},
                "gpk": {"S": "g1"},
                "gsk": {"S": "a"},
                "proj_attr": {"S": "match"},
            }]),
        ),
    ] {
        let answered = served(&router, case, &statement).await;
        assert_eq!(
            answered["Items"], expected,
            "case {case}: an index reports what it projects, got {answered}"
        );
    }

    // A GLOBAL index "cannot fetch attributes from the parent table" (`Query$Select`), so
    // a projection naming an attribute it does not project is refused instead, whichever
    // unprojected attribute it names and whatever else it names beside it.
    for (case, statement, named) in [
        (
            "one unprojected attribute through the global index",
            format!("SELECT extra FROM \"{TABLE}\".\"{GSI}\" WHERE gpk = 'g1'"),
            "extra",
        ),
        (
            "a projected attribute and an unprojected one",
            format!("SELECT proj_attr, not_proj_attr FROM \"{TABLE}\".\"{GSI}\" WHERE gpk = 'g1'"),
            "not_proj_attr",
        ),
        (
            "a path reaching inside an unprojected attribute",
            format!("SELECT extra.deep FROM \"{TABLE}\".\"{GSI}\""),
            "extra",
        ),
    ] {
        let answered = refused(&router, case, &statement).await;
        assert_eq!(
            answered,
            format!(
                "One or more parameter values were invalid: Global secondary index {GSI} does not \
                 project [{named}]"
            ),
            "case {case}: expected the unprojected-projection sentence, got {answered:?}"
        );
    }
}

#[tokio::test]
async fn an_unprojected_filter_attribute_is_refused_only_where_the_clause_pins_the_index_key() {
    let router = emulator();
    indexed(&router).await;

    // The clause states a condition on the index's partition key, so the read is served as
    // a QUERY of that index — the range of its keys the clause constrains — and a
    // predicate on an attribute the index does not project could never be decided from the
    // entries that range holds. The rows differ in the predicate form the unprojected
    // attribute appears in, and in the operator that constrains the index's key.
    for (case, statement) in [
        (
            "an equality on an unprojected attribute",
            format!(
                "SELECT * FROM \"{TABLE}\".\"{GSI}\" WHERE gpk = 'g1' AND not_proj_attr = 'hidden'"
            ),
        ),
        (
            "a BEGINS_WITH on an unprojected attribute",
            format!(
                "SELECT * FROM \"{TABLE}\".\"{GSI}\" WHERE gpk = 'g1' AND \
                 begins_with(not_proj_attr, 'h')"
            ),
        ),
        (
            "an unprojected attribute inside a disjunction",
            format!(
                "SELECT * FROM \"{TABLE}\".\"{GSI}\" WHERE gpk = 'g1' AND (not_proj_attr = 'x' \
                 OR proj_attr = 'match')"
            ),
        ),
        (
            "the index's partition key constrained by an inequality",
            format!(
                "SELECT * FROM \"{TABLE}\".\"{GSI}\" WHERE gpk > 'g0' AND not_proj_attr = 'hidden'"
            ),
        ),
        (
            "the index's partition key named inside a disjunction",
            format!(
                "SELECT * FROM \"{TABLE}\".\"{GSI}\" WHERE (gpk = 'g1' OR gpk = 'g2') AND \
                 not_proj_attr = 'hidden'"
            ),
        ),
    ] {
        let answered = refused(&router, case, &statement).await;
        assert_eq!(
            answered,
            format!(
                "One or more parameter values were invalid: Secondary index {GSI} does not \
                 project one or more filter attributes: [not_proj_attr]"
            ),
            "case {case}: expected the unprojected-filter sentence, got {answered:?}"
        );
    }

    // A clause that states no condition on the index's partition key walks the whole index
    // instead, and that walk judges its predicate against each ENTRY: an attribute the
    // index does not project is absent from every entry, so the statement is served and
    // matches nothing.
    for (case, statement) in [
        (
            "an unprojected attribute with no key named at all",
            format!("SELECT * FROM \"{TABLE}\".\"{GSI}\" WHERE not_proj_attr = 'hidden'"),
        ),
        (
            "an unprojected attribute beside the index's sort key",
            format!(
                "SELECT * FROM \"{TABLE}\".\"{GSI}\" WHERE gsk = 'a' AND not_proj_attr = 'hidden'"
            ),
        ),
        (
            "an unprojected attribute beside the table's own key",
            format!(
                "SELECT * FROM \"{TABLE}\".\"{GSI}\" WHERE pk = 'Pa' AND not_proj_attr = 'hidden'"
            ),
        ),
    ] {
        let answered = served(&router, case, &statement).await;
        assert_eq!(
            items(&answered),
            Vec::<Value>::new(),
            "case {case}: the entries carry no such attribute, so the walk matches nothing, got \
             {answered}"
        );
    }

    // The same walk over an attribute the index DOES project reports the entries that
    // satisfy it, so the rule above narrows nothing it should not.
    let answered = served(
        &router,
        "a projected attribute with no key named",
        &format!("SELECT * FROM \"{TABLE}\".\"{GSI}\" WHERE proj_attr = 'other'"),
    )
    .await;
    assert_eq!(
        items(&answered).len(),
        1,
        "case a projected attribute with no key named: expected the one entry that satisfies it, \
         got {answered}"
    );
}

#[tokio::test]
async fn a_source_qualifier_naming_no_index_of_the_table_is_a_bad_statement_not_a_missing_resource()
{
    let router = emulator();
    indexed(&router).await;

    // A name the table carries no index of resolved to nothing of the table: the table was
    // found and nothing it carries answers to that name, which is a defect of the
    // STATEMENT. The rows differ in the spelling of the name.
    for (case, index) in [
        ("a name the table carries no index of", "nowhere-idx"),
        ("another name the table carries no index of", "gsi9"),
    ] {
        let answered = refused(
            &router,
            case,
            &format!("SELECT * FROM \"{TABLE}\".\"{index}\" WHERE pk = 'Pa'"),
        )
        .await;
        assert_eq!(
            answered, "The table does not have the specified index",
            "case {case}: expected the index-unknown sentence, got {answered:?}"
        );
    }

    // The same name sent as the `IndexName` MEMBER of a classic read is answered that name
    // as well: what each refusal carries is what its request gave it to echo — a member
    // holds the name on its own, a source qualifier is part of the statement text — so the
    // sentence above is the rule for a qualifier rather than this surface dropping a value
    // it had.
    let (status, answered) = call(
        &router,
        "Query",
        json!({
            "TableName": TABLE,
            "IndexName": "nowhere-idx",
            "KeyConditionExpression": "gpk = :g",
            "ExpressionAttributeValues": {":g": {"S": "g1"}},
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case the same name as an IndexName member: expected the read to be refused, got \
         {status} with {answered}"
    );
    assert_eq!(
        message(&answered),
        "The table does not have the specified index: nowhere-idx",
        "case the same name as an IndexName member: expected the sentence with the name the \
         member carried, got {answered}"
    );
}

/// Where `run` sits in `statement`, as a positioned refusal reports it:
/// `<line>:<column>:<length>`, counted in characters from 1 over the ONE TOKEN the refused
/// construct is spelled at.
fn position_of(statement: &str, run: &str) -> String {
    let offset = statement
        .find(run)
        .unwrap_or_else(|| panic!("the run {run:?} is not in the statement {statement:?}"));
    let line = 1 + statement[..offset].matches('\n').count();
    let line_start = statement[..offset]
        .rfind('\n')
        .map_or(0, |break_at| break_at + 1);
    let column = 1 + statement[line_start..offset].chars().count();
    format!("{line}:{column}:{}", run.chars().count())
}

/// A one-item table the `SET`-clause rows update, keyed by a partition key alone.
const WRITES: &str = "PartiqlSetOperands";

/// Create that table and seed the item every row below updates.
async fn writable(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": WRITES,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        }),
    )
    .await;
    describe_when_active(router, "the writable table", WRITES).await;
    ok(
        router,
        "PutItem",
        json!({
            "TableName": WRITES,
            "Item": {
                "pk": {"S": "w"},
                "n": {"N": "10"},
                "tags": {"L": [{"S": "one"}, {"S": "two"}]},
                "ns": {"NS": ["1", "2"]},
            },
        }),
    )
    .await;
}

/// The item the write table holds now.
async fn stored(router: &MultiServiceRouter) -> Value {
    ok(
        router,
        "GetItem",
        json!({"TableName": WRITES, "Key": {"pk": {"S": "w"}}}),
    )
    .await["Item"]
        .clone()
}

#[tokio::test]
async fn a_set_clause_joins_two_operands_with_plus_or_minus_and_no_other_arithmetic() {
    let router = emulator();
    writable(&router).await;

    // Addition and subtraction APPLY, in both operand orders and against a literal or the
    // stored attribute; the rows differ in which operand the clause reads from the item.
    for (case, clause, expected) in [
        ("subtracting a literal", "n = n - 4", "6"),
        ("adding a literal", "n = n + 5", "15"),
        ("subtracting from a literal", "n = 30 - n", "20"),
        ("adding two literals", "n = 2 + 3", "5"),
        ("subtracting an attribute from itself", "n = n - n", "0"),
    ] {
        let statement = format!("UPDATE \"{WRITES}\" SET {clause} WHERE pk = 'w'");
        let (status, answered) = call(
            &router,
            "ExecuteStatement",
            json!({"Statement": statement}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the arithmetic to apply, got {status} with {answered}"
        );
        let item = stored(&router).await;
        assert_eq!(
            item["n"], json!({"N": expected}),
            "case {case}: expected n to be {expected}, got {item}"
        );
        // Put the item back the way the next row expects it.
        ok(
            &router,
            "ExecuteStatement",
            json!({"Statement": format!("UPDATE \"{WRITES}\" SET n = 10 WHERE pk = 'w'")}),
        )
        .await;
    }

    // Every other arithmetic operator is refused, and the refusal names the operator the
    // statement spelled. The rows span both operators and both operand positions.
    for (case, clause, operator) in [
        ("multiplying an attribute by a literal", "n = n * 2", "*"),
        ("dividing an attribute by a literal", "n = n / 2", "/"),
        ("multiplying a literal by an attribute", "n = 2 * n", "*"),
        ("dividing a literal by an attribute", "n = 2 / n", "/"),
        ("an operator past a sum", "n = n + 1 * 2", "*"),
    ] {
        let answered = refused(
            &router,
            case,
            &format!("UPDATE \"{WRITES}\" SET {clause} WHERE pk = 'w'"),
        )
        .await;
        assert_eq!(
            answered,
            format!("Unsupported operator in Update Expression. Operator: {operator}"),
            "case {case}: expected the unsupported-operator sentence, got {answered:?}"
        );
        let item = stored(&router).await;
        assert_eq!(
            item["n"], json!({"N": "10"}),
            "case {case}: a refused statement writes nothing, got {item}"
        );
    }
}

#[tokio::test]
async fn a_list_index_step_addresses_a_position_of_the_list() {
    let router = emulator();
    writable(&router).await;

    // A position of the list is addressed and written; a position past its end appends, by
    // the rule the classic update path already applies.
    for (case, clause, expected) in [
        (
            "the first position",
            "tags[0] = 'first'",
            json!({"L": [{"S": "first"}, {"S": "two"}]}),
        ),
        (
            "the last position",
            "tags[1] = 'last'",
            json!({"L": [{"S": "first"}, {"S": "last"}]}),
        ),
        (
            "a position past the end",
            "tags[9] = 'appended'",
            json!({"L": [{"S": "first"}, {"S": "last"}, {"S": "appended"}]}),
        ),
    ] {
        let statement = format!("UPDATE \"{WRITES}\" SET {clause} WHERE pk = 'w'");
        let (status, answered) = call(
            &router,
            "ExecuteStatement",
            json!({"Statement": statement}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the step to address the list, got {status} with {answered}"
        );
        let item = stored(&router).await;
        assert_eq!(
            item["tags"], expected,
            "case {case}: expected {expected}, got {item}"
        );
    }

    // An index below the first position addresses no member of any list, and the refusal
    // echoes the index the statement wrote, at the position the statement spells it. The
    // rows differ in the index's width and in where the statement spells it.
    for (case, path, index) in [
        ("the position before the first", "tags[-1]", "-1"),
        ("a position far below the first", "tags[-120]", "-120"),
        ("a nested list's position", "tags[0].deep[-7]", "-7"),
    ] {
        let statement = format!("UPDATE \"{WRITES}\" SET {path} = 'x' WHERE pk = 'w'");
        let answered = refused(&router, case, &statement).await;
        // The position reported is the token the index itself is spelled at: the digits,
        // whose own width the sentence carries.
        let spelled = index.trim_start_matches('-');
        let at = position_of(&statement, &format!("{spelled}]"));
        let at = at
            .rsplit_once(':')
            .map(|(head, _)| format!("{head}:{}", spelled.chars().count()))
            .unwrap_or(at);
        assert_eq!(
            answered,
            format!("List index is not within the allowable range; index: [{index}] at {at}"),
            "case {case}: expected the list-index sentence for that position, got {answered:?}"
        );
    }
}

#[tokio::test]
async fn a_bag_denotes_a_set_whose_members_are_all_of_one_type() {
    let router = emulator();
    writable(&router).await;

    // A bag of one type IS a set, and the statement stores it.
    for (case, clause, expected) in [
        (
            "a bag of strings",
            "ss = <<'x', 'y'>>",
            json!({"SS": ["x", "y"]}),
        ),
        ("a bag of numbers", "ns2 = <<1, 2>>", json!({"NS": ["1", "2"]})),
        (
            "a set the clause ADDS to",
            "ns = set_add(ns, <<3>>)",
            json!({"NS": ["1", "2", "3"]}),
        ),
    ] {
        let statement = format!("UPDATE \"{WRITES}\" SET {clause} WHERE pk = 'w'");
        let (status, answered) = call(
            &router,
            "ExecuteStatement",
            json!({"Statement": statement}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the set to be stored, got {status} with {answered}"
        );
        let item = stored(&router).await;
        let attribute = clause.split(' ').next().unwrap_or_default();
        let mut got = item[attribute].clone();
        for member in got
            .as_object_mut()
            .map(|entries| entries.values_mut().collect::<Vec<_>>())
            .unwrap_or_default()
        {
            if let Some(members) = member.as_array_mut() {
                members.sort_by_key(|value| value.as_str().unwrap_or_default().to_string());
            }
        }
        assert_eq!(
            got, expected,
            "case {case}: expected {expected}, got {item}"
        );
    }

    // A bag whose members are not all of one type denotes no stored set at all, whichever
    // types it mixes and whichever clause carries it.
    for (case, statement) in [
        (
            "a bag mixing a number and a string",
            format!("UPDATE \"{WRITES}\" SET s = <<1, 'two'>> WHERE pk = 'w'"),
        ),
        (
            "a bag mixing a string and a number",
            format!("UPDATE \"{WRITES}\" SET s = <<'two', 1>> WHERE pk = 'w'"),
        ),
        (
            "a bag mixing a number and a boolean",
            format!("UPDATE \"{WRITES}\" SET s = <<1, true>> WHERE pk = 'w'"),
        ),
        (
            "a mixed bag a SET ADD carries",
            format!("UPDATE \"{WRITES}\" SET ns = set_add(ns, <<1, 'two'>>) WHERE pk = 'w'"),
        ),
        (
            "a mixed bag a SET DELETE carries",
            format!("UPDATE \"{WRITES}\" SET ns = set_delete(ns, <<1, 'two'>>) WHERE pk = 'w'"),
        ),
        (
            "a mixed bag an INSERT carries",
            format!("INSERT INTO \"{WRITES}\" VALUE {{'pk': 'mixed', 's': <<1, 'two'>>}}"),
        ),
    ] {
        let answered = refused(&router, case, &statement).await;
        assert_eq!(
            answered,
            "Unsupported data type in Bag. DynamoDB only supports either numbers or strings in \
             bags",
            "case {case}: expected the bag sentence, got {answered:?}"
        );
    }
}

#[tokio::test]
async fn an_insert_cannot_ask_to_see_an_image_of_what_it_added() {
    let router = emulator();
    writable(&router).await;

    // Every form of the clause is refused for the VERB, whichever image it asks for, and
    // the item is not stored.
    for (case, clause) in [
        ("the item as it stands after the write", "RETURNING ALL NEW *"),
        ("the item it replaced", "RETURNING ALL OLD *"),
        ("the attributes it modified", "RETURNING MODIFIED NEW *"),
    ] {
        let answered = refused(
            &router,
            case,
            &format!(
                "INSERT INTO \"{WRITES}\" VALUE {{'pk': 'returning', 'n': 1}} {clause}"
            ),
        )
        .await;
        assert_eq!(
            answered, "RETURNING is not supported in INSERT",
            "case {case}: expected the INSERT sentence, got {answered:?}"
        );
    }
    let (status, read) = call(
        &router,
        "GetItem",
        json!({"TableName": WRITES, "Key": {"pk": {"S": "returning"}}}),
    )
    .await;
    assert_eq!(
        (status, read.get("Item").cloned()),
        (200, None),
        "a refused INSERT stores nothing, got {read}"
    );

    // An `INSERT` that states no such clause is served, so the refusal is about the clause
    // and not about the verb.
    ok(
        &router,
        "ExecuteStatement",
        json!({"Statement": format!(
            "INSERT INTO \"{WRITES}\" VALUE {{'pk': 'plain', 'n': 1}}"
        )}),
    )
    .await;
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": WRITES, "Key": {"pk": {"S": "plain"}}}),
    )
    .await;
    assert_eq!(
        read["Item"],
        json!({"pk": {"S": "plain"}, "n": {"N": "1"}}),
        "an INSERT with no RETURNING clause stores its item, got {read}"
    );
}
