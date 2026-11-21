// SPDX-License-Identifier: CC0-1.0

//! What the SEARCHED operand of `contains` may be, driven end to end.
//!
//! RULE under test: in the condition-expression language, `contains`'s searched operand is
//! an OPERAND like its second one — either a document path the item is read at, or a value
//! the request carried — so a membership test over a collection the request supplied
//! (`contains(:candidates, #attribute)`) is PARSED and evaluated per item rather than
//! refused. The call holds when the first operand contains the second, whichever side came
//! from the request; the one rule beyond that is the reference's own, that two operands
//! naming ONE location are a mistake.
//!
//! The rows differ along what the rule generalises over: the TYPE of the searched
//! collection the request carried (a list, a string set, a number set, a text searched for a
//! substring), the type of the member it is tested for (text, a number, binary, a value the
//! request carried too), how many of the scanned items the test selects (all of them, some,
//! none), where the tested member is read from (a top-level attribute, a map member, an
//! alias), and the surrounding grammar (`NOT`, `AND`). The last two suites drive the same
//! operand form through a `Query` filter and through a write's `ConditionExpression`, which
//! is the same parser and evaluator, and pin the calls whose signature still needs a path.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row reads: one partition every seeded item shares and a sort key that
/// names the item, so a `Query` can narrow to the same five items a `Scan` walks.
const TABLE: &str = "SetMembershipOperands";

/// The partition every seeded item sits in.
const TENANT: &str = "tenant-1";

/// One seeded item: the sort key that names it, its `status`, its `tier`, the `fragment` of
/// text it carries, the `label` inside its map, the byte its binary `code` repeats, the
/// members of its `tags` set and the members of its `role_list` list.
struct Seeded {
    sort_key: &'static str,
    status: &'static str,
    tier: u32,
    fragment: &'static str,
    label: &'static str,
    byte: u8,
    tags: &'static [&'static str],
    role_list: &'static [&'static str],
}

/// The five seeded items, in the order they are written.
const SEEDED: [Seeded; 5] = [
    Seeded {
        sort_key: "u1",
        status: "active",
        tier: 1,
        fragment: "act",
        label: "alpha",
        byte: 1,
        tags: &["red", "blue"],
        role_list: &["admin", "ops"],
    },
    Seeded {
        sort_key: "u2",
        status: "active",
        tier: 2,
        fragment: "act",
        label: "beta",
        byte: 2,
        tags: &["red"],
        role_list: &["ops"],
    },
    Seeded {
        sort_key: "u3",
        status: "pending",
        tier: 3,
        fragment: "pen",
        label: "alpha",
        byte: 3,
        tags: &["green"],
        role_list: &[],
    },
    Seeded {
        sort_key: "u4",
        status: "inactive",
        tier: 4,
        fragment: "ina",
        label: "gamma",
        byte: 4,
        tags: &["blue"],
        role_list: &["viewer"],
    },
    Seeded {
        sort_key: "u5",
        status: "suspended",
        tier: 5,
        fragment: "sus",
        label: "delta",
        byte: 5,
        tags: &["green", "red"],
        role_list: &["admin"],
    },
];

/// How many items a read of [`TABLE`] scans, which every served row must report.
const SCANNED: usize = SEEDED.len();

/// The emulator with [`TABLE`] and the five seeded items in place.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S"), ("sk", "RANGE", "S")]).await;
    for item in SEEDED {
        let role_list: Vec<Value> = item
            .role_list
            .iter()
            .map(|role| json!({"S": role}))
            .collect();
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": {
                "pk": {"S": TENANT},
                "sk": {"S": item.sort_key},
                "status": {"S": item.status},
                "tier": {"N": item.tier.to_string()},
                "fragment": {"S": item.fragment},
                "meta": {"M": {"label": {"S": item.label}}},
                "code": {"B": code_of(item.byte)},
                "tags": {"SS": item.tags},
                "role_list": {"L": role_list},
            }}),
        )
        .await;
    }
    router
}

/// The binary `code` of the item whose byte is `byte`, as a request spells one.
fn code_of(byte: u8) -> String {
    ::aws_smithy_types::base64::encode([byte, byte])
}

/// The sort keys a served read selected, in ascending order — the SET of items the filter
/// selects is the rule here, not the order a table walk reports them in.
fn selected(answered: &Value) -> Vec<String> {
    let mut keys: Vec<String> = answered["Items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item["sk"]["S"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    keys.sort();
    keys
}

/// One row: the case, the filter, its substitution tables, and the sort keys it selects.
struct Row {
    case: &'static str,
    filter: &'static str,
    values: Value,
    names: Value,
    selects: Vec<&'static str>,
}

/// Every membership form a filter may state, with the items each one selects out of
/// [`SEEDED`].
fn rows() -> Vec<Row> {
    let every_status = json!({":candidates": {"L": [
        {"S": "active"}, {"S": "pending"}, {"S": "inactive"}, {"S": "suspended"}]}});
    vec![
        Row {
            case: "a list carrying every stored status",
            filter: "contains(:candidates, #st)",
            values: every_status.clone(),
            names: json!({"#st": "status"}),
            selects: vec!["u1", "u2", "u3", "u4", "u5"],
        },
        Row {
            case: "a list carrying two of the stored statuses",
            filter: "contains(:candidates, #st)",
            values: json!({":candidates": {"L": [{"S": "active"}, {"S": "pending"}]}}),
            names: json!({"#st": "status"}),
            selects: vec!["u1", "u2", "u3"],
        },
        Row {
            case: "a list carrying one status no item holds",
            filter: "contains(:candidates, #st)",
            values: json!({":candidates": {"L": [{"S": "retired"}]}}),
            names: json!({"#st": "status"}),
            selects: vec![],
        },
        Row {
            case: "an empty list",
            filter: "contains(:candidates, #st)",
            values: json!({":candidates": {"L": []}}),
            names: json!({"#st": "status"}),
            selects: vec![],
        },
        Row {
            case: "a string set of statuses",
            filter: "contains(:candidates, #st)",
            values: json!({":candidates": {"SS": ["pending", "suspended"]}}),
            names: json!({"#st": "status"}),
            selects: vec!["u3", "u5"],
        },
        Row {
            case: "a number set of tiers",
            filter: "contains(:candidates, tier)",
            values: json!({":candidates": {"NS": ["2", "4"]}}),
            names: json!({}),
            selects: vec!["u2", "u4"],
        },
        Row {
            case: "a list of binary codes",
            filter: "contains(:candidates, code)",
            values: json!({":candidates": {"L": [{"B": code_of(2)}, {"B": code_of(5)}]}}),
            names: json!({}),
            selects: vec!["u2", "u5"],
        },
        Row {
            case: "a text searched for the fragment each item holds",
            filter: "contains(:candidates, fragment)",
            values: json!({":candidates": {"S": "act and pen only"}}),
            names: json!({}),
            selects: vec!["u1", "u2", "u3"],
        },
        Row {
            case: "a list tested for a member read inside a map",
            filter: "contains(:candidates, meta.label)",
            values: json!({":candidates": {"L": [{"S": "alpha"}, {"S": "gamma"}]}}),
            names: json!({}),
            selects: vec!["u1", "u3", "u4"],
        },
        Row {
            case: "a list tested for a member read through an aliased path",
            filter: "contains(:candidates, #m.#l)",
            values: json!({":candidates": {"L": [{"S": "delta"}]}}),
            names: json!({"#m": "meta", "#l": "label"}),
            selects: vec!["u5"],
        },
        Row {
            case: "a membership test the surrounding grammar negates",
            filter: "NOT contains(:candidates, #st)",
            values: json!({":candidates": {"L": [{"S": "active"}, {"S": "pending"}]}}),
            names: json!({"#st": "status"}),
            selects: vec!["u4", "u5"],
        },
        Row {
            case: "a membership test combined with another condition",
            filter: "contains(:candidates, #st) AND tier > :floor",
            values: json!({":candidates": {"L": [{"S": "active"}, {"S": "inactive"}]},
                           ":floor": {"N": "1"}}),
            names: json!({"#st": "status"}),
            selects: vec!["u2", "u4"],
        },
        Row {
            case: "a membership test whose operands cannot match any stored value",
            filter: "contains(:candidates, tier)",
            values: json!({":candidates": {"SS": ["2", "4"]}}),
            names: json!({}),
            selects: vec![],
        },
        // Both operands carried by the REQUEST: the call is then the same fact for every
        // item, which is still a served scan rather than a refusal.
        Row {
            case: "both operands carried by the request, the member among them",
            filter: "contains(:candidates, :one)",
            values: json!({":candidates": {"L": [{"S": "active"}, {"S": "pending"}]},
                           ":one": {"S": "pending"}}),
            names: json!({}),
            selects: vec!["u1", "u2", "u3", "u4", "u5"],
        },
        Row {
            case: "both operands carried by the request, the member not among them",
            filter: "contains(:candidates, :one)",
            values: json!({":candidates": {"L": [{"S": "active"}]},
                           ":one": {"S": "retired"}}),
            names: json!({}),
            selects: vec![],
        },
        // The searched operand read from the ITEM, which this rule leaves exactly as it
        // was: a set and a list the item holds, searched for a value the request carried.
        Row {
            case: "a stored string set searched for a value the request carried",
            filter: "contains(tags, :one)",
            values: json!({":one": {"S": "red"}}),
            names: json!({}),
            selects: vec!["u1", "u2", "u5"],
        },
        Row {
            case: "a stored list searched for a value the request carried",
            filter: "contains(role_list, :one)",
            values: json!({":one": {"S": "admin"}}),
            names: json!({}),
            selects: vec!["u1", "u5"],
        },
    ]
}

#[tokio::test]
async fn a_scan_evaluates_a_membership_test_over_the_values_the_request_carried() {
    let router = seeded().await;

    for Row {
        case,
        filter,
        values,
        names,
        selects,
    } in rows()
    {
        let mut request = json!({"TableName": TABLE, "FilterExpression": filter});
        if !values.as_object().is_none_or(serde_json::Map::is_empty) {
            request["ExpressionAttributeValues"] = values;
        }
        if !names.as_object().is_none_or(serde_json::Map::is_empty) {
            request["ExpressionAttributeNames"] = names;
        }
        let (status, answered) = call(&router, "Scan", request).await;
        assert_eq!(
            status,
            200,
            "case {case:?} ({filter:?}): expected a served scan, got {status} with {}",
            message(&answered)
        );
        assert_eq!(
            selected(&answered),
            selects,
            "case {case:?} ({filter:?}): expected the items {selects:?}, got {answered}"
        );
        assert_eq!(
            answered["Count"],
            json!(selects.len()),
            "case {case:?} ({filter:?}): expected {} selected, got {answered}",
            selects.len()
        );
        assert_eq!(
            answered["ScannedCount"],
            json!(SCANNED),
            "case {case:?} ({filter:?}): expected all {SCANNED} items scanned, got {answered}"
        );
    }
}

#[tokio::test]
async fn a_query_filters_by_the_same_membership_test() {
    let router = seeded().await;

    for Row {
        case,
        filter,
        values,
        names,
        selects,
    } in rows()
    {
        let mut carried = values.as_object().cloned().unwrap_or_default();
        carried.insert(":tenant".to_string(), json!({"S": TENANT}));
        let mut request = json!({"TableName": TABLE,
                                 "KeyConditionExpression": "pk = :tenant",
                                 "FilterExpression": filter,
                                 "ExpressionAttributeValues": Value::Object(carried)});
        if !names.as_object().is_none_or(serde_json::Map::is_empty) {
            request["ExpressionAttributeNames"] = names;
        }
        let (status, answered) = call(&router, "Query", request).await;
        assert_eq!(
            status,
            200,
            "case {case:?} ({filter:?}): expected a served query, got {status} with {}",
            message(&answered)
        );
        assert_eq!(
            selected(&answered),
            selects,
            "case {case:?} ({filter:?}): expected the items {selects:?}, got {answered}"
        );
        assert_eq!(
            answered["ScannedCount"],
            json!(SCANNED),
            "case {case:?} ({filter:?}): expected all {SCANNED} items read, got {answered}"
        );
    }
}

#[tokio::test]
async fn a_write_s_condition_is_held_to_the_same_membership_test() {
    let router = seeded().await;
    let key = json!({"pk": {"S": TENANT}, "sk": {"S": "u3"}});

    // The stored status is `pending`, so a list that does not carry it leaves the write
    // unmade: the condition is evaluated against the item, not assumed to hold.
    let (status, answered) = call(
        &router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": TENANT}, "sk": {"S": "u3"}, "status": {"S": "archived"}},
               "ConditionExpression": "contains(:candidates, #st)",
               "ExpressionAttributeNames": {"#st": "status"},
               "ExpressionAttributeValues": {":candidates": {"L": [{"S": "active"}]}}}),
    )
    .await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "ConditionalCheckFailedException"),
        "case a condition whose candidates exclude the stored status: expected the \
         conditional refusal, got {status} with {answered}"
    );
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": key.clone(), "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        read["Item"]["status"],
        json!({"S": "pending"}),
        "case a condition whose candidates exclude the stored status: the item is \
         untouched, got {read}"
    );

    // And a list that carries it admits the write, which the read back reports.
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": TENANT}, "sk": {"S": "u3"}, "status": {"S": "archived"}},
               "ConditionExpression": "contains(:candidates, #st)",
               "ExpressionAttributeNames": {"#st": "status"},
               "ExpressionAttributeValues": {":candidates": {"SS": ["pending", "active"]}}}),
    )
    .await;
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": key, "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        read["Item"]["status"],
        json!({"S": "archived"}),
        "case a condition whose candidates carry the stored status: the write landed, got \
         {read}"
    );
}

#[tokio::test]
async fn the_calls_whose_signature_declares_a_path_still_require_one() {
    let router = seeded().await;

    // (case, the filter, the whole message it must earn). The functions whose reference
    // signature declares a document path keep requiring one, and `contains` keeps the one
    // rule the reference states about its operands: "The path and the operand must be
    // distinct; for example, contains (a, a) returns an error".
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "attribute_exists given a value",
            "attribute_exists(:one)",
            needs_a_path("attribute_exists"),
        ),
        (
            "attribute_not_exists given a value",
            "attribute_not_exists(:one)",
            needs_a_path("attribute_not_exists"),
        ),
        (
            "attribute_type given a value in its path position",
            "attribute_type(:one, :type)",
            needs_a_path("attribute_type"),
        ),
        (
            "begins_with given a value in its path position",
            "begins_with(:one, :one)",
            needs_a_path("begins_with"),
        ),
        (
            "size given a value",
            "size(:one) = :tier",
            needs_a_path("size"),
        ),
        (
            "contains given one location twice",
            "contains(status, status)",
            "Invalid FilterExpression: The first operand must be distinct from the \
             remaining operands for this operator or function; operator: contains, first \
             operand: [status]"
                .to_string(),
        ),
    ];

    for (case, filter, expected) in rows {
        let (status, answered) = call(
            &router,
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": filter,
                   "ExpressionAttributeValues": {":one": {"S": "active"},
                                                 ":type": {"S": "S"},
                                                 ":tier": {"N": "1"}}}),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.clone()),
            "case {case:?} ({filter:?}): expected {expected:?}, got {status} with {answered}"
        );
    }
}

/// The whole message a value written where the signature declares a document path earns on
/// a `Scan`'s filter.
fn needs_a_path(function: &str) -> String {
    format!(
        "Invalid FilterExpression: Operator or function requires a document path; operator \
         or function: {function}"
    )
}
