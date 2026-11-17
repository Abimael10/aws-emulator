// SPDX-License-Identifier: CC0-1.0

//! An index-targeted or projection-carrying read is answered from the REQUEST and the
//! stored items: never with an error the model does not declare.
//!
//! The rule this suite pins, over both readers that take an `IndexName` and a projection:
//! a defect of the request — the index it names, the key schema its condition covers, the
//! type an operand carries against the key it binds to, a strongly consistent read of a
//! global secondary index, a `Select` its projection contradicts, a projection the
//! expression language rejects, a projection a global index cannot answer — is the
//! modelled `ValidationException` at HTTP 400 with the sentence AWS words it in, decided
//! before anything is read; and a projection the language accepts is SERVED, narrowing
//! each reported item to the paths it names.
//!
//! The catalog of refusals comes from the members themselves, not from one recorded
//! request: `smithy_query(query="doc",shape="Query",member="IndexName")` ("This index can
//! be any local secondary index or global secondary index"), `member="ConsistentRead"`
//! ("not supported on global secondary indexes"), `member="Select"` ("You cannot use both
//! Select and ProjectionExpression together in a single request, unless the value for
//! Select is SPECIFIC_ATTRIBUTES", and "you can only request attributes that are projected
//! into the index") and `member="ProjectionExpression"` ("To access an attribute whose name
//! conflicts with a DynamoDB reserved word" is what `ExpressionAttributeNames` is for).
//!
//! The rows differ along what the rule generalises over: the operation (`Query` and
//! `Scan`), the index kind (global, local, and a global index that projects only its keys),
//! the member the defect sits in, the declared type of the key an operand binds to, the
//! `Select` value, and — for the served reads — the FORM the projection takes: a whole
//! attribute, an alias, a path into a map, a path two levels deep, a list element, an
//! aliased attribute whose own name carries a dot, and several paths at once.
//!
//! The axis here is ADMISSION: that the service decides a read from the request rather
//! than refusing the capability. What an index read orders, pages and bills is
//! `query_indexes_and_projections.rs` and `scan_indexes_and_projections.rs`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every suite here reads. One table per emulator, and each suite builds its
/// own emulator, so the name is a constant.
const TABLE: &str = "ReadAdmission";

/// A global secondary index that projects every attribute.
const GLOBAL_ALL: &str = "ByTeam";

/// A global secondary index that projects only the keys, so a read of anything else has
/// to be refused rather than fetched from the parent table.
const GLOBAL_KEYS: &str = "ByBadge";

/// A local secondary index, which a strongly consistent read IS supported on.
const LOCAL_ALL: &str = "ByScore";

/// The binary badge of the first item, as the wire carries a `B` value.
const BADGE: &str = "YmFkZ2Ux";

/// The items every suite seeds. `score` is numeric and `badge` is binary, so a condition
/// supplying a string for either is a type mismatch against the schema; only the first
/// item carries a `badge` (so the keys-only index covers it alone), a nested document, a
/// list, and an attribute whose own name carries a dot.
fn stored_items() -> Vec<Value> {
    vec![
        json!({"pk": {"S": "a"}, "sk": {"N": "1"}, "team": {"S": "t1"}, "score": {"N": "5"},
               "badge": {"B": BADGE}, "label": {"S": "first"},
               "doc": {"M": {"child": {"S": "inner"},
                             "deep": {"M": {"leaf": {"S": "bottom"}}}}},
               "odd.tag": {"S": "flat"},
               "parts": {"L": [{"S": "zero"}, {"S": "one"}]}}),
        json!({"pk": {"S": "a"}, "sk": {"N": "2"}, "team": {"S": "t1"}, "score": {"N": "3"},
               "label": {"S": "second"}}),
        json!({"pk": {"S": "b"}, "sk": {"N": "1"}, "team": {"S": "t2"}, "score": {"N": "7"},
               "label": {"S": "third"}}),
    ]
}

/// Create [`TABLE`] with a global index of each projection type and one local index, wait
/// out the documented `CREATING` -> `ACTIVE` transition, and write every stored item.
async fn seeded(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"},
                {"AttributeName": "team", "AttributeType": "S"},
                {"AttributeName": "score", "AttributeType": "N"},
                {"AttributeName": "badge", "AttributeType": "B"},
            ],
            "BillingMode": "PAY_PER_REQUEST",
            "GlobalSecondaryIndexes": [
                {
                    "IndexName": GLOBAL_ALL,
                    "KeySchema": [
                        {"AttributeName": "team", "KeyType": "HASH"},
                        {"AttributeName": "score", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
                {
                    "IndexName": GLOBAL_KEYS,
                    "KeySchema": [{"AttributeName": "badge", "KeyType": "HASH"}],
                    "Projection": {"ProjectionType": "KEYS_ONLY"},
                },
            ],
            "LocalSecondaryIndexes": [
                {
                    "IndexName": LOCAL_ALL,
                    "KeySchema": [
                        {"AttributeName": "pk", "KeyType": "HASH"},
                        {"AttributeName": "score", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
            ],
        }),
    )
    .await;
    describe_when_active(router, "seed the read-admission table", TABLE).await;
    for item in stored_items() {
        ok(router, "PutItem", json!({"TableName": TABLE, "Item": item})).await;
    }
}

// ===========================================================================
// 1. Every read the service refuses is the modelled client error
// ===========================================================================

/// One refused read: what the request gets wrong, the operation it is sent to, the body,
/// and the sentence the service must answer with.
struct RefusalRow {
    case: &'static str,
    operation: &'static str,
    body: Value,
    message: String,
}

/// "You cannot use both Select and ProjectionExpression together in a single request,
/// unless the value for Select is SPECIFIC_ATTRIBUTES" (`Query$Select`): the clash is one
/// clause inside the validation envelope, naming the projection member the request carried
/// and what its `Select` asked for — the count itself for `COUNT`.
fn select_conflict(select: NonSpecificSelect) -> String {
    // `COUNT` "returns the number of matching items, rather than the matching items
    // themselves", which AWS's sentence words as the count itself; the other two name the
    // attributes they return, and are named as the request spelled them.
    let target = match select {
        NonSpecificSelect::Count => "only the Count",
        NonSpecificSelect::AllAttributes => "ALL_ATTRIBUTES",
        NonSpecificSelect::AllProjectedAttributes => "ALL_PROJECTED_ATTRIBUTES",
    };
    format!(
        "1 validation error detected: Cannot specify the ProjectionExpression when choosing \
         to get {target}"
    )
}

/// Every `Select` value but `SPECIFIC_ATTRIBUTES` — the ones a projection cannot be stated
/// beside — from `smithy_query(query="shape",shape="Select")`. An enumeration rather than a
/// string, so a value added to that set has to be given its wording above rather than
/// silently taking the raw name.
#[derive(Clone, Copy, Debug)]
enum NonSpecificSelect {
    Count,
    AllAttributes,
    AllProjectedAttributes,
}

impl NonSpecificSelect {
    /// The value as a request spells it on the wire.
    fn stated(self) -> &'static str {
        match self {
            Self::Count => "COUNT",
            Self::AllAttributes => "ALL_ATTRIBUTES",
            Self::AllProjectedAttributes => "ALL_PROJECTED_ATTRIBUTES",
        }
    }
}

/// The sentence a condition operand whose type does not match the key it binds to earns.
const TYPE_MISMATCH: &str = "One or more parameter values were invalid: Condition parameter \
                             type does not match schema type";

fn refusal_rows() -> Vec<RefusalRow> {
    vec![
        RefusalRow {
            case: "a query naming an index this table declares nowhere",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": "TaskStatus",
                         "KeyConditionExpression": "team = :t",
                         "ExpressionAttributeValues": {":t": {"S": "t1"}}}),
            message: "The table does not have the specified index: TaskStatus".to_string(),
        },
        RefusalRow {
            // An index name is matched as written: "IndexName - The name of an index"
            // (`Query$IndexName`) addresses the declared name, not a case-folding of it.
            case: "a query naming a declared index in the wrong case",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": "byteam",
                         "KeyConditionExpression": "team = :t",
                         "ExpressionAttributeValues": {":t": {"S": "t1"}}}),
            message: "The table does not have the specified index: byteam".to_string(),
        },
        RefusalRow {
            case: "a scan naming an index this table declares nowhere",
            operation: "Scan",
            body: json!({"TableName": TABLE, "IndexName": "no-such-index"}),
            message: "The table does not have the specified index: no-such-index".to_string(),
        },
        RefusalRow {
            case: "a global index query whose condition omits the index partition key",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": GLOBAL_ALL,
                         "KeyConditionExpression": "score = :s",
                         "ExpressionAttributeValues": {":s": {"N": "5"}}}),
            message: "Query condition missed key schema element: team".to_string(),
        },
        RefusalRow {
            case: "a global index query whose condition names the TABLE's partition key",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": GLOBAL_ALL,
                         "KeyConditionExpression": "pk = :p",
                         "ExpressionAttributeValues": {":p": {"S": "a"}}}),
            message: "Query condition missed key schema element: team".to_string(),
        },
        RefusalRow {
            case: "a local index query whose condition omits the partition key",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": LOCAL_ALL,
                         "KeyConditionExpression": "score = :s",
                         "ExpressionAttributeValues": {":s": {"N": "5"}}}),
            message: "Query condition missed key schema element: pk".to_string(),
        },
        RefusalRow {
            case: "a string supplied for the numeric index sort key",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": GLOBAL_ALL,
                         "KeyConditionExpression": "team = :t AND score = :s",
                         "ExpressionAttributeValues": {":t": {"S": "t1"},
                                                       ":s": {"S": "5"}}}),
            message: TYPE_MISMATCH.to_string(),
        },
        RefusalRow {
            case: "a string supplied for the binary index partition key",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": GLOBAL_KEYS,
                         "KeyConditionExpression": "badge = :b",
                         "ExpressionAttributeValues": {":b": {"S": BADGE}}}),
            message: TYPE_MISMATCH.to_string(),
        },
        RefusalRow {
            case: "a number supplied for the string index partition key",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": GLOBAL_ALL,
                         "KeyConditionExpression": "team = :t",
                         "ExpressionAttributeValues": {":t": {"N": "1"}}}),
            message: TYPE_MISMATCH.to_string(),
        },
        RefusalRow {
            case: "a strongly consistent query of a global secondary index",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": GLOBAL_ALL, "ConsistentRead": true,
                         "KeyConditionExpression": "team = :t",
                         "ExpressionAttributeValues": {":t": {"S": "t1"}}}),
            message: "Consistent reads are not supported on global secondary indexes"
                .to_string(),
        },
        RefusalRow {
            case: "a strongly consistent scan of a global secondary index",
            operation: "Scan",
            body: json!({"TableName": TABLE, "IndexName": GLOBAL_KEYS, "ConsistentRead": true}),
            message: "Consistent reads are not supported on global secondary indexes"
                .to_string(),
        },
        RefusalRow {
            case: "a counting query carrying a projection",
            operation: "Query",
            body: json!({"TableName": TABLE, "Select": NonSpecificSelect::Count.stated(),
                         "ProjectionExpression": "label",
                         "KeyConditionExpression": "pk = :p",
                         "ExpressionAttributeValues": {":p": {"S": "a"}}}),
            message: select_conflict(NonSpecificSelect::Count),
        },
        RefusalRow {
            case: "a query of every attribute carrying a projection",
            operation: "Query",
            body: json!({"TableName": TABLE,
                         "Select": NonSpecificSelect::AllAttributes.stated(),
                         "ProjectionExpression": "label",
                         "KeyConditionExpression": "pk = :p",
                         "ExpressionAttributeValues": {":p": {"S": "a"}}}),
            message: select_conflict(NonSpecificSelect::AllAttributes),
        },
        RefusalRow {
            case: "an index query of every projected attribute carrying a projection",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": GLOBAL_ALL,
                         "Select": NonSpecificSelect::AllProjectedAttributes.stated(),
                         "ProjectionExpression": "label",
                         "KeyConditionExpression": "team = :t",
                         "ExpressionAttributeValues": {":t": {"S": "t1"}}}),
            message: select_conflict(NonSpecificSelect::AllProjectedAttributes),
        },
        RefusalRow {
            case: "a counting scan carrying a projection",
            operation: "Scan",
            body: json!({"TableName": TABLE, "Select": NonSpecificSelect::Count.stated(),
                         "ProjectionExpression": "label"}),
            message: select_conflict(NonSpecificSelect::Count),
        },
        RefusalRow {
            // The projection is settled from the request, so a condition that matches
            // nothing is refused for its projection all the same.
            case: "a zero-match query projecting a path through an undefined alias",
            operation: "Query",
            body: json!({"TableName": TABLE, "ProjectionExpression": "doc.#u",
                         "KeyConditionExpression": "pk = :p",
                         "ExpressionAttributeValues": {":p": {"S": "no-such-partition"}}}),
            message: "Invalid ProjectionExpression: An expression attribute name used in the \
                      document path is not defined; attribute name: #u"
                .to_string(),
        },
        RefusalRow {
            case: "a scan projecting an undefined alias as the whole name",
            operation: "Scan",
            body: json!({"TableName": TABLE, "ProjectionExpression": "#gone"}),
            message: "Invalid ProjectionExpression: An expression attribute name used in the \
                      document path is not defined; attribute name: #gone"
                .to_string(),
        },
        RefusalRow {
            case: "a query projecting a bare reserved word",
            operation: "Query",
            body: json!({"TableName": TABLE, "ProjectionExpression": "status",
                         "KeyConditionExpression": "pk = :p",
                         "ExpressionAttributeValues": {":p": {"S": "a"}}}),
            message: "Invalid ProjectionExpression: Attribute name is a reserved keyword; \
                      reserved keyword: status"
                .to_string(),
        },
        RefusalRow {
            case: "a scan projecting a different bare reserved word",
            operation: "Scan",
            body: json!({"TableName": TABLE, "ProjectionExpression": "order"}),
            message: "Invalid ProjectionExpression: Attribute name is a reserved keyword; \
                      reserved keyword: order"
                .to_string(),
        },
        RefusalRow {
            case: "a query of every attribute of a keys-only global index",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": GLOBAL_KEYS,
                         "Select": "ALL_ATTRIBUTES",
                         "KeyConditionExpression": "badge = :b",
                         "ExpressionAttributeValues": {":b": {"B": BADGE}}}),
            message: format!(
                "One or more parameter values were invalid: Select type ALL_ATTRIBUTES is not \
                 supported for global secondary index {GLOBAL_KEYS} because its projection type \
                 is not ALL"
            ),
        },
        RefusalRow {
            case: "a scan of a keys-only global index projecting an unprojected attribute",
            operation: "Scan",
            body: json!({"TableName": TABLE, "IndexName": GLOBAL_KEYS,
                         "ProjectionExpression": "label"}),
            message: format!(
                "One or more parameter values were invalid: Global secondary index \
                 {GLOBAL_KEYS} does not project [label]"
            ),
        },
    ]
}

#[tokio::test]
async fn a_refused_indexed_or_projected_read_is_the_modelled_validation_error() {
    let router = emulator();
    seeded(&router).await;

    for row in refusal_rows() {
        let (status, refused) = call(&router, row.operation, row.body.clone()).await;
        assert_eq!(
            status, 400,
            "case {:?}: a request the service refuses is the caller's to fix, got HTTP \
             {status} with {refused}",
            row.case
        );
        assert_eq!(
            error_code(&refused),
            "ValidationException",
            "case {:?}: expected the modelled ValidationException, got {refused}",
            row.case
        );
        assert_eq!(
            message(&refused),
            row.message,
            "case {:?}: expected the message {:?}, got {refused}",
            row.case,
            row.message
        );
    }
}

// ===========================================================================
// 2. A read the service admits is served from the stored items
// ===========================================================================

/// One served read: the operation, the body, and the `Items` the response must carry, in
/// the order the read walks.
struct ServedRow {
    case: &'static str,
    operation: &'static str,
    body: Value,
    items: Value,
}

fn served_rows() -> Vec<ServedRow> {
    vec![
        ServedRow {
            case: "a scan projecting an aliased path into a map attribute",
            operation: "Scan",
            body: json!({"TableName": TABLE, "ProjectionExpression": "#p.#c",
                         "ExpressionAttributeNames": {"#p": "doc", "#c": "child"},
                         "FilterExpression": "label = :l",
                         "ExpressionAttributeValues": {":l": {"S": "first"}}}),
            items: json!([{"doc": {"M": {"child": {"S": "inner"}}}}]),
        },
        ServedRow {
            // An alias stands for the WHOLE name it maps to, dot and all: "#d" names the
            // top-level attribute `odd.tag` rather than a path through an attribute `odd`.
            case: "a scan projecting an aliased attribute whose name carries a dot",
            operation: "Scan",
            body: json!({"TableName": TABLE, "ProjectionExpression": "#d",
                         "ExpressionAttributeNames": {"#d": "odd.tag"},
                         "FilterExpression": "label = :l",
                         "ExpressionAttributeValues": {":l": {"S": "first"}}}),
            items: json!([{"odd.tag": {"S": "flat"}}]),
        },
        ServedRow {
            case: "a query projecting a path two levels into a map attribute",
            operation: "Query",
            body: json!({"TableName": TABLE, "ProjectionExpression": "doc.deep.leaf",
                         "KeyConditionExpression": "pk = :p AND sk = :s",
                         "ExpressionAttributeValues": {":p": {"S": "a"}, ":s": {"N": "1"}}}),
            items: json!([{"doc": {"M": {"deep": {"M": {"leaf": {"S": "bottom"}}}}}}]),
        },
        ServedRow {
            case: "a query projecting one element of a list attribute",
            operation: "Query",
            body: json!({"TableName": TABLE, "ProjectionExpression": "parts[1]",
                         "KeyConditionExpression": "pk = :p AND sk = :s",
                         "ExpressionAttributeValues": {":p": {"S": "a"}, ":s": {"N": "1"}}}),
            items: json!([{"parts": {"L": [{"S": "one"}]}}]),
        },
        ServedRow {
            case: "a global index query projecting one whole attribute",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": GLOBAL_ALL,
                         "ProjectionExpression": "label",
                         "KeyConditionExpression": "team = :t",
                         "ExpressionAttributeValues": {":t": {"S": "t1"}}}),
            // The index orders its entries by `score`: 3 before 5.
            items: json!([{"label": {"S": "second"}}, {"label": {"S": "first"}}]),
        },
        ServedRow {
            case: "a global index query naming SPECIFIC_ATTRIBUTES and several paths",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": GLOBAL_ALL,
                         "Select": "SPECIFIC_ATTRIBUTES",
                         "ProjectionExpression": "label, score",
                         "KeyConditionExpression": "team = :t",
                         "ExpressionAttributeValues": {":t": {"S": "t1"}}}),
            items: json!([{"label": {"S": "second"}, "score": {"N": "3"}},
                          {"label": {"S": "first"}, "score": {"N": "5"}}]),
        },
        ServedRow {
            // "Strongly consistent reads ... are supported on local secondary indexes"
            // is the other side of the global-index refusal above.
            case: "a strongly consistent local index query projecting one attribute",
            operation: "Query",
            body: json!({"TableName": TABLE, "IndexName": LOCAL_ALL, "ConsistentRead": true,
                         "ProjectionExpression": "label",
                         "KeyConditionExpression": "pk = :p",
                         "ExpressionAttributeValues": {":p": {"S": "a"}}}),
            items: json!([{"label": {"S": "second"}}, {"label": {"S": "first"}}]),
        },
        ServedRow {
            case: "a keys-only global index scan projecting the index key it does hold",
            operation: "Scan",
            body: json!({"TableName": TABLE, "IndexName": GLOBAL_KEYS,
                         "ProjectionExpression": "badge"}),
            items: json!([{"badge": {"B": BADGE}}]),
        },
    ]
}

#[tokio::test]
async fn a_legal_projection_narrows_what_each_read_reports() {
    let router = emulator();
    seeded(&router).await;

    for row in served_rows() {
        let served = ok(&router, row.operation, row.body.clone()).await;
        assert_eq!(
            served["Items"], row.items,
            "case {:?}: expected the items {}, got {served}",
            row.case, row.items
        );
        assert_eq!(
            served["Count"],
            json!(row.items.as_array().map_or(0, Vec::len)),
            "case {:?}: expected the Count of the items reported, got {served}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_counting_index_read_reports_the_count_and_no_items() {
    let router = emulator();
    seeded(&router).await;

    // The count each row expects is a column of the row, written out from the entries the
    // index holds for what the request addressed: the two `t1` entries of the global index
    // that covers every item, and the one entry of the keys-only index, which only the
    // badge-carrying item is in.
    for (case, operation, body, expected) in [
        (
            "a counting query of one global index partition",
            "Query",
            json!({"TableName": TABLE, "IndexName": GLOBAL_ALL,
                   "Select": NonSpecificSelect::Count.stated(),
                   "KeyConditionExpression": "team = :t",
                   "ExpressionAttributeValues": {":t": {"S": "t1"}}}),
            2,
        ),
        (
            "a counting scan of a keys-only global index",
            "Scan",
            json!({"TableName": TABLE, "IndexName": GLOBAL_KEYS,
                   "Select": NonSpecificSelect::Count.stated()}),
            1,
        ),
    ] {
        let counted = ok(&router, operation, body).await;
        assert_eq!(
            counted["Count"],
            json!(expected),
            "case {case:?}: expected a Count of {expected}, got {counted}"
        );
        assert_eq!(
            counted["Items"],
            Value::Null,
            "case {case:?}: COUNT returns the number of matching items rather than the items \
             themselves, got {counted}"
        );
    }
}
