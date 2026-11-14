// SPDX-License-Identifier: CC0-1.0

//! The condition keys a DATA-PLANE call of this service answers for, and what a statement
//! conditioned on one of them decides.
//!
//! RULE: a data-plane request publishes every condition key of this service it can answer
//! for — the partitions it addresses (`dynamodb:LeadingKeys`), the attribute names it names
//! (`dynamodb:Attributes`) and the selection a read states or implies (`dynamodb:Select`) —
//! carrying the values it names and NO value where it names none. A clause over such a key
//! is therefore decided by IAM's own rules for a key the request carries no value for: a
//! `ForAllValues:` clause holds over the empty set, a `ForAnyValue:` one does not, an
//! unqualified comparison holds only where it is negated or spelled `…IfExists`, and
//! `Null` asks about presence alone. Nothing is left undecided because the request named
//! none of something.
//!
//! The rows span what the rule generalises over: the condition key, the set qualifier
//! (`ForAllValues:`, `ForAnyValue:`, none), the operator (equality, inequality, pattern,
//! existence) and its `…IfExists` spelling, the operation (`GetItem`, `Query`, `Scan`,
//! `PutItem`), how many values the request carries for the key (none, one, several), and —
//! for the attributes — whether a path names a whole attribute or reaches inside one.
//!
//! Driven over the wire through the same `MultiServiceRouter` the emulator's front doors
//! use, reusing the harness in [`super::table_items`] and the policy helpers in
//! [`super::resource_policy_conditions_and_read_lag`]. The decision is exercised through
//! the TABLE's own resource-based policy, which is the half of the rule this crate can
//! state on its own: the identity and session halves of the same decision are driven
//! across services in `core/router/tests/all/dynamodb_data_plane_authorization.rs`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::resource_policy_conditions_and_read_lag::{
    attach_and_apply, conditioned_deny, message_of, table_arn,
};
use super::table_items::{call, create_active_table, emulator, ok};
use crate::emulator::error_code;

/// The principal every request of this harness speaks for, which is who a refusal names.
const CALLER: &str = "arn:aws:iam::000000000000:user/emulator";

/// The partition whose item carries a nested attribute, so a projection can reach inside
/// one.
const NESTED_PARTITION: &str = "secret";

/// One probe against the policy in force: the request, and whether the conditioned deny
/// refuses it.
struct Probe {
    case: &'static str,
    operation: &'static str,
    body: Value,
    denied: bool,
    /// The IAM action a refusal names, where it is not the one the operation is named
    /// after: a transactional `Put` performs `dynamodb:PutItem`.
    action: Option<&'static str>,
}

/// One condition row: the clause the deny carries, and the probes taken against it.
struct ConditionCase {
    case: &'static str,
    condition: Value,
    probes: Vec<Probe>,
}

/// A table holding three items: two plain ones, and one carrying a nested map attribute.
async fn seeded_table(router: &MultiServiceRouter, name: &str) -> String {
    create_active_table(router, name, &[("pk", "HASH", "S")]).await;
    for partition in ["secret", "public"] {
        ok(
            router,
            "PutItem",
            json!({
                "TableName": name,
                "Item": {"pk": {"S": partition}, "v": {"S": format!("{partition}-1")}},
            }),
        )
        .await;
    }
    ok(
        router,
        "PutItem",
        json!({
            "TableName": name,
            "Item": {
                "pk": {"S": NESTED_PARTITION},
                "v": {"S": "secret-1"},
                "m": {"M": {"nestedvalue": {"S": "nested"}}},
            },
        }),
    )
    .await;
    table_arn(name)
}

/// Attach each row's clause as a deny of every data-plane action on the table, then take
/// its probes: a served probe is a 200, and a refused one is the `AccessDeniedException`
/// this service words an explicit resource-policy deny with.
async fn drive(router: &MultiServiceRouter, arn: &str, cases: Vec<ConditionCase>) {
    for ConditionCase {
        case,
        condition,
        probes,
    } in cases
    {
        attach_and_apply(router, arn, &conditioned_deny(arn, json!("dynamodb:*"), condition)).await;
        for probe in probes {
            let (status, body) = call(router, probe.operation, &probe.body).await;
            if probe.denied {
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (400, "AccessDeniedException"),
                    "case {case} / {}: expected the clause to select {} and refuse it, got \
                     {status} {body}",
                    probe.case,
                    probe.operation
                );
                assert_eq!(
                    message_of(&body),
                    format!(
                        "User: {CALLER} is not authorized to perform: {} on resource: \
                         {arn} with an explicit deny in a resource-based policy",
                        probe
                            .action
                            .map_or_else(
                                || format!("dynamodb:{}", probe.operation),
                                str::to_string
                            )
                    ),
                    "case {case} / {}: expected the explicit-deny sentence, got {:?}",
                    probe.case,
                    message_of(&body)
                );
            } else {
                assert_eq!(
                    status, 200,
                    "case {case} / {}: the clause does not select this {}, so it is served, got \
                     {status} {body}",
                    probe.case, probe.operation
                );
            }
        }
    }
}

/// A `GetItem` of one partition, with the projection `projection` when one is given.
fn read_of(table: &str, partition: &str, projection: Option<&str>) -> Value {
    let mut body = json!({"TableName": table, "Key": {"pk": {"S": partition}}});
    if let Some(projection) = projection {
        body["ProjectionExpression"] = json!(projection);
    }
    body
}

/// A `Query` of one partition, with the `Select` `select` when one is given.
fn query_of(table: &str, partition: &str, select: Option<&str>) -> Value {
    let mut body = json!({
        "TableName": table,
        "KeyConditionExpression": "pk = :p",
        "ExpressionAttributeValues": {":p": {"S": partition}},
    });
    if let Some(select) = select {
        body["Select"] = json!(select);
    }
    body
}

/// A `PutItem` of an item carrying `attributes` beside its key.
fn write_of(table: &str, partition: &str, attributes: &[&str]) -> Value {
    json!({"TableName": table, "Item": item_of(partition, attributes)})
}

/// An item at `partition` carrying `attributes` beside its key.
fn item_of(partition: &str, attributes: &[&str]) -> Value {
    let mut item = json!({"pk": {"S": partition}});
    for attribute in attributes {
        item[*attribute] = json!({"S": "written"});
    }
    item
}

/// An `UpdateItem` whose expression writes one attribute: the attributes an update names
/// are those of its expression, which the authorizer does not enumerate, so such a call
/// answers for `dynamodb:Attributes` with nothing at all.
fn update_of(table: &str, partition: &str) -> Value {
    json!({
        "TableName": table,
        "Key": {"pk": {"S": partition}},
        "UpdateExpression": "SET v = :x",
        "ExpressionAttributeValues": {":x": {"S": "updated"}},
    })
}

/// A `BatchWriteItem` putting one item, which names the attributes that item carries.
fn batch_write_of(table: &str, partition: &str, attributes: &[&str]) -> Value {
    json!({
        "RequestItems": {
            table: [{"PutRequest": {"Item": item_of(partition, attributes)}}],
        },
    })
}

/// A `TransactWriteItems` putting one item, which names the attributes that item carries.
fn transact_write_of(table: &str, partition: &str, attributes: &[&str]) -> Value {
    json!({
        "TransactItems": [{
            "Put": {"TableName": table, "Item": item_of(partition, attributes)},
        }],
    })
}

/// A `PutItem` whose item carries `beside_the_key` attributes of its own, so the number of
/// values one call publishes for `dynamodb:Attributes` can be driven to its ceiling and
/// past it.
fn wide_write_of(table: &str, partition: &str, beside_the_key: usize) -> Value {
    let mut item = json!({"pk": {"S": partition}});
    for index in 0..beside_the_key {
        item[format!("a{index}")] = json!({"S": "written"});
    }
    json!({"TableName": table, "Item": item})
}

/// The most attribute names one call publishes for `dynamodb:Attributes`: the ceiling this
/// service holds a projection to, which the item a write stores is read against too.
const MAX_PUBLISHED_ATTRIBUTES: usize =
    service_com_amazonaws_dynamodb_dynamodb_20120810::state::MAX_PROJECTED_PATHS;

/// RULE: a call that addresses no partition of its own — a table walk — carries NO value
/// for `dynamodb:LeadingKeys` and answers that it carries none, which is IAM's empty set:
/// `ForAllValues:` and the negated and `…IfExists` spellings hold over it, `ForAnyValue:`
/// and a plain equality do not, and `Null` reports the key as absent. A call that addresses
/// a partition is decided against the value it addresses.
#[tokio::test]
async fn a_table_walk_carries_the_empty_set_of_leading_keys() {
    let router = emulator();
    let table = "LeadingKeySets";
    let arn = seeded_table(&router, table).await;

    let walk = |denied| Probe {
        case: "a table walk, which addresses no partition",
        operation: "Scan",
        body: json!({"TableName": table}),
        denied,
        action: None,
    };
    let selected = |denied| Probe {
        case: "a read of the partition the clause lists",
        operation: "GetItem",
        body: read_of(table, "secret", None),
        denied,
        action: None,
    };
    let other = |denied| Probe {
        case: "a read of a partition the clause does not list",
        operation: "GetItem",
        body: read_of(table, "public", None),
        denied,
        action: None,
    };

    let cases = vec![
        ConditionCase {
            // "ForAllValues … returns true if there are no request values" (the IAM
            // policy reference's set-operator rule).
            case: "ForAllValues:StringEquals over one leading key",
            condition: json!({"ForAllValues:StringEquals": {"dynamodb:LeadingKeys": ["secret"]}}),
            probes: vec![walk(true), selected(true), other(false)],
        },
        ConditionCase {
            // "ForAnyValue … returns false if there are no request values" — the other
            // half of that same rule.
            case: "ForAnyValue:StringEquals over one leading key",
            condition: json!({"ForAnyValue:StringEquals": {"dynamodb:LeadingKeys": ["secret"]}}),
            probes: vec![walk(false), selected(true), other(false)],
        },
        ConditionCase {
            case: "an unqualified StringEquals on the leading key",
            condition: json!({"StringEquals": {"dynamodb:LeadingKeys": "secret"}}),
            probes: vec![walk(false), selected(true), other(false)],
        },
        ConditionCase {
            // "If the key is not present … a negated condition operator returns true".
            case: "a negated StringNotEquals on the leading key",
            condition: json!({"StringNotEquals": {"dynamodb:LeadingKeys": "secret"}}),
            probes: vec![walk(true), selected(false), other(true)],
        },
        ConditionCase {
            // The `…IfExists` spelling holds on a key the request carries no value for.
            case: "StringEqualsIfExists on the leading key",
            condition: json!({"StringEqualsIfExists": {"dynamodb:LeadingKeys": "secret"}}),
            probes: vec![walk(true), selected(true), other(false)],
        },
        ConditionCase {
            case: "Null asserting the leading key is absent",
            condition: json!({"Null": {"dynamodb:LeadingKeys": "true"}}),
            probes: vec![walk(true), selected(false), other(false)],
        },
    ];
    drive(&router, &arn, cases).await;
}

/// RULE: a read answers for the selection it asks for — the attributes it listed
/// (`SPECIFIC_ATTRIBUTES`), every attribute of the item (`ALL_ATTRIBUTES`) or the count a
/// `Select` asked for — and a call that selects nothing, which every write is, carries no
/// value for `dynamodb:Select` at all.
#[tokio::test]
async fn the_selection_a_read_states_decides_a_conditioned_statement() {
    let router = emulator();
    let table = "ReadSelections";
    let arn = seeded_table(&router, table).await;

    let whole_item = |denied| Probe {
        case: "a read that lists no attribute, which selects them all",
        operation: "GetItem",
        body: read_of(table, "secret", None),
        denied,
        action: None,
    };
    let projected = |denied| Probe {
        case: "a read that lists one attribute",
        operation: "GetItem",
        body: read_of(table, "secret", Some("v")),
        denied,
        action: None,
    };
    let projected_scan = |denied| Probe {
        case: "a table walk that lists one attribute",
        operation: "Scan",
        body: json!({"TableName": table, "ProjectionExpression": "v"}),
        denied,
        action: None,
    };
    let counted = |denied| Probe {
        case: "a query asking only for the count",
        operation: "Query",
        body: query_of(table, "secret", Some("COUNT")),
        denied,
        action: None,
    };
    let write = |denied| Probe {
        case: "a write, which selects nothing",
        operation: "PutItem",
        body: write_of(table, "written", &["v"]),
        denied,
        action: None,
    };
    let update = |denied| Probe {
        case: "an update, which selects nothing",
        operation: "UpdateItem",
        body: update_of(table, "written"),
        denied,
        action: None,
    };

    // The four values the member admits
    // (`smithy_query(query="shape",shape="Select")` -> ALL_ATTRIBUTES |
    // ALL_PROJECTED_ATTRIBUTES | SPECIFIC_ATTRIBUTES | COUNT), each as the selection a
    // request states or implies.
    let cases = vec![
        ConditionCase {
            case: "StringEquals on the selection a read of every attribute implies",
            condition: json!({"StringEquals": {"dynamodb:Select": "ALL_ATTRIBUTES"}}),
            probes: vec![
                whole_item(true),
                projected(false),
                projected_scan(false),
                counted(false),
                write(false),
                update(false),
            ],
        },
        ConditionCase {
            case: "StringEquals on the selection a listed projection implies",
            condition: json!({"StringEquals": {"dynamodb:Select": "SPECIFIC_ATTRIBUTES"}}),
            probes: vec![
                whole_item(false),
                projected(true),
                projected_scan(true),
                counted(false),
                write(false),
            ],
        },
        ConditionCase {
            case: "StringEquals on the selection a counting query states",
            condition: json!({"StringEquals": {"dynamodb:Select": "COUNT"}}),
            probes: vec![
                whole_item(false),
                projected(false),
                projected_scan(false),
                counted(true),
                write(false),
            ],
        },
        ConditionCase {
            case: "StringEqualsIfExists, which holds for a call that selects nothing",
            condition: json!({"StringEqualsIfExists": {"dynamodb:Select": "COUNT"}}),
            probes: vec![
                whole_item(false),
                projected(false),
                projected_scan(false),
                counted(true),
                write(true),
                update(true),
            ],
        },
        ConditionCase {
            case: "Null asserting the selection is absent",
            condition: json!({"Null": {"dynamodb:Select": "true"}}),
            probes: vec![
                whole_item(false),
                projected(false),
                projected_scan(false),
                counted(false),
                write(true),
                update(true),
            ],
        },
    ];
    drive(&router, &arn, cases).await;
}

/// RULE: a call answers for the attributes it names — the top-level attribute name each
/// projected path starts at, and every attribute name the item a write stores carries —
/// and a call that names none carries no value for `dynamodb:Attributes`. A call whose
/// named set is not enumerated — the attributes an UPDATE's expression writes — answers
/// for the key with nothing, so a clause over it cannot be decided and the deny carrying it
/// keeps refusing (the fail-closed reading of an undecided statement) instead of holding
/// over an empty set the request did not have.
#[tokio::test]
async fn the_attributes_a_call_names_decide_a_conditioned_statement() {
    let router = emulator();
    let table = "NamedAttributes";
    let arn = seeded_table(&router, table).await;

    let none_named = |denied| Probe {
        case: "a read that names no attribute",
        operation: "GetItem",
        body: read_of(table, NESTED_PARTITION, None),
        denied,
        action: None,
    };
    let one_named = |denied| Probe {
        case: "a read naming one attribute",
        operation: "GetItem",
        body: read_of(table, NESTED_PARTITION, Some("v")),
        denied,
        action: None,
    };
    let two_named = |denied| Probe {
        case: "a read naming two attributes",
        operation: "GetItem",
        body: read_of(table, NESTED_PARTITION, Some("pk, v")),
        denied,
        action: None,
    };
    let inside = |denied| Probe {
        case: "a read whose path reaches inside an attribute, naming that attribute",
        operation: "GetItem",
        body: read_of(table, NESTED_PARTITION, Some("m.nestedvalue")),
        denied,
        action: None,
    };
    let legacy = |denied| Probe {
        case: "a read naming one attribute through the legacy member",
        operation: "GetItem",
        body: json!({
            "TableName": table,
            "Key": {"pk": {"S": NESTED_PARTITION}},
            "AttributesToGet": ["v"],
        }),
        denied,
        action: None,
    };
    let stores_two = |denied| Probe {
        case: "a write whose item carries the key and one attribute",
        operation: "PutItem",
        body: write_of(table, "written", &["v"]),
        denied,
        action: None,
    };
    let stores_three = |denied| Probe {
        case: "a write whose item carries an attribute outside the clause",
        operation: "PutItem",
        body: write_of(table, "written", &["v", "w"]),
        denied,
        action: None,
    };
    let batched_two = |denied| Probe {
        case: "a batched write whose item carries the key and one attribute",
        operation: "BatchWriteItem",
        body: batch_write_of(table, "batched", &["v"]),
        denied,
        action: None,
    };
    let batched_three = |denied| Probe {
        case: "a batched write whose item carries an attribute outside the clause",
        operation: "BatchWriteItem",
        body: batch_write_of(table, "batched", &["v", "w"]),
        denied,
        action: None,
    };
    let transacted_two = |denied| Probe {
        case: "a transactional put whose item carries the key and one attribute",
        operation: "TransactWriteItems",
        body: transact_write_of(table, "transacted", &["v"]),
        denied,
        // A transactional `Put` performs `dynamodb:PutItem`, which is the action its
        // refusal names.
        action: Some("dynamodb:PutItem"),
    };
    let transacted_three = |denied| Probe {
        case: "a transactional put whose item carries an attribute outside the clause",
        operation: "TransactWriteItems",
        body: transact_write_of(table, "transacted", &["v", "w"]),
        denied,
        action: Some("dynamodb:PutItem"),
    };
    let update = |denied| Probe {
        case: "an update, whose named attributes are not enumerated",
        operation: "UpdateItem",
        body: update_of(table, "written"),
        denied,
        action: None,
    };
    // The ceiling itself and one past it: a call that names at most that many attributes
    // answers for the key, and one that names more answers for it with nothing, which is
    // the same undecided — and so refused — position as the update above.
    let at_the_ceiling = |denied| Probe {
        case: "a write naming as many attributes as one call publishes",
        operation: "PutItem",
        body: wide_write_of(table, "wide", MAX_PUBLISHED_ATTRIBUTES - 1),
        denied,
        action: None,
    };
    let past_the_ceiling = |denied| Probe {
        case: "a write naming one attribute more than that",
        operation: "PutItem",
        body: wide_write_of(table, "wide", MAX_PUBLISHED_ATTRIBUTES),
        denied,
        action: None,
    };

    let cases = vec![
        ConditionCase {
            case: "ForAllValues:StringEquals over the key and one attribute",
            condition: json!({
                "ForAllValues:StringEquals": {"dynamodb:Attributes": ["pk", "v"]},
            }),
            probes: vec![
                none_named(true),
                one_named(true),
                two_named(true),
                inside(false),
                legacy(true),
                stores_two(true),
                stores_three(false),
                batched_two(true),
                batched_three(false),
                transacted_two(true),
                transacted_three(false),
                at_the_ceiling(false),
                past_the_ceiling(true),
                // Undecided, so the deny is never weakened into permission.
                update(true),
            ],
        },
        ConditionCase {
            case: "ForAnyValue:StringEquals over one attribute",
            condition: json!({"ForAnyValue:StringEquals": {"dynamodb:Attributes": ["w"]}}),
            probes: vec![
                none_named(false),
                one_named(false),
                inside(false),
                stores_two(false),
                stores_three(true),
                batched_three(true),
                transacted_three(true),
                update(true),
            ],
        },
        ConditionCase {
            case: "ForAllValues:StringEquals over the attribute a nested path names",
            condition: json!({"ForAllValues:StringEquals": {"dynamodb:Attributes": ["m"]}}),
            probes: vec![one_named(false), inside(true), stores_two(false)],
        },
        ConditionCase {
            case: "ForAllValues:StringLike over a pattern of attribute names",
            condition: json!({"ForAllValues:StringLike": {"dynamodb:Attributes": ["v*"]}}),
            probes: vec![
                none_named(true),
                one_named(true),
                two_named(false),
                legacy(true),
                stores_three(false),
            ],
        },
        ConditionCase {
            case: "Null asserting the request names no attribute",
            condition: json!({"Null": {"dynamodb:Attributes": "true"}}),
            probes: vec![
                none_named(true),
                one_named(false),
                inside(false),
                stores_two(false),
            ],
        },
    ];
    drive(&router, &arn, cases).await;
}

/// One projection row: what the request asks of the item, and what the read reports.
struct ProjectionCase {
    case: &'static str,
    /// The projection as the request states it, in the member it states it through.
    request: Value,
    /// The `Item` the read reports.
    reported: Value,
}

/// RULE: a key-addressed read reports what its projection names of the item it found — the
/// value at each path, wrapped in the containers the path stepped through — and the whole
/// item when it names no projection at all; a path that addresses nothing in the item
/// contributes nothing.
#[tokio::test]
async fn a_projected_read_reports_only_the_paths_it_names() {
    let router = emulator();
    let table = "Projections";
    seeded_table(&router, table).await;

    let cases = vec![
        ProjectionCase {
            case: "no projection at all, which reports the whole item",
            request: read_of(table, NESTED_PARTITION, None),
            reported: json!({
                "pk": {"S": NESTED_PARTITION},
                "v": {"S": "secret-1"},
                "m": {"M": {"nestedvalue": {"S": "nested"}}},
            }),
        },
        ProjectionCase {
            case: "one attribute",
            request: read_of(table, NESTED_PARTITION, Some("v")),
            reported: json!({"v": {"S": "secret-1"}}),
        },
        ProjectionCase {
            case: "two attributes, one of them the key",
            request: read_of(table, NESTED_PARTITION, Some("pk, v")),
            reported: json!({"pk": {"S": NESTED_PARTITION}, "v": {"S": "secret-1"}}),
        },
        ProjectionCase {
            case: "a path reaching inside an attribute",
            request: read_of(table, NESTED_PARTITION, Some("m.nestedvalue")),
            reported: json!({"m": {"M": {"nestedvalue": {"S": "nested"}}}}),
        },
        ProjectionCase {
            case: "an aliased name, substituted from ExpressionAttributeNames",
            request: json!({
                "TableName": table,
                "Key": {"pk": {"S": NESTED_PARTITION}},
                "ProjectionExpression": "#alias",
                "ExpressionAttributeNames": {"#alias": "v"},
            }),
            reported: json!({"v": {"S": "secret-1"}}),
        },
        ProjectionCase {
            case: "the legacy member naming one attribute",
            request: json!({
                "TableName": table,
                "Key": {"pk": {"S": NESTED_PARTITION}},
                "AttributesToGet": ["v"],
            }),
            reported: json!({"v": {"S": "secret-1"}}),
        },
        ProjectionCase {
            case: "a path the item holds nothing at, which contributes nothing",
            request: read_of(table, NESTED_PARTITION, Some("absent")),
            reported: json!({}),
        },
    ];

    for ProjectionCase {
        case,
        request,
        reported,
    } in cases
    {
        let answer = ok(&router, "GetItem", request).await;
        assert_eq!(
            answer["Item"], reported,
            "case {case}: expected the projection to report {reported}, got {answer}"
        );
    }
}
