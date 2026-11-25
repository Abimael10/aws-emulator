// SPDX-License-Identifier: CC0-1.0

//! What a `PutInventory` of a CUSTOM type is held to, end to end: the attribute set the type
//! is registered with, and the count of custom schemas the endpoint enforces.
//!
//! RULE under test: **a custom inventory type's attribute set is registered by the first write
//! of an item under that type at a schema version — "the request adds an inventory item, if it
//! doesn't already exist, or updates an inventory item, if it does exist"
//! (`smithy_query(query="operation", operation="PutInventory")`), and `GetInventorySchema`
//! "return[s] … a list of attribute names for a specific Inventory item type" — so a later
//! write of that same type and version whose content carries another attribute set is content
//! that does not match the type's schema: HTTP 400 `InvalidItemContentException` with the
//! attributes the item carries and the ones the schema requires, writing nothing. A write that
//! carries exactly the registered set is served whatever its values, and a write at another
//! schema version registers that version's own set. Past the custom schema count the endpoint
//! enforces, a write that would register one more type is HTTP 400
//! `CustomSchemaCountLimitExceededException` naming the count it was refused at. How large one
//! item is — its entries, and each entry's attributes — is bounded before anything is stored:
//! one past either ceiling is HTTP 400 `ItemSizeLimitExceededException`.**
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the router. The ONE thing seeded through the engine is a registered managed node,
//! because no call of this service registers one; every schema here is registered by a real
//! `PutInventory` and read back with a real `GetInventorySchema` or `ListInventoryEntries`.
//!
//! The rows differ along what the rule generalises over: how many attributes a registration
//! carries, whether the second write drops one, adds one, swaps one for another, carries none
//! at all, or carries the same set in another key order and with other values; how many entries
//! one item holds; which node of the Region the second write is about; which schema version
//! it names; and how large one item is, at each ceiling and one past it.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::custom_inventory;
use smithy_server_runtime::MultiServiceRouter;

use crate::custom_inventory::{register, NODE};
use crate::parameter_store::{call, error_code, error_message, router_with_engine, REGION};

/// A second registered node of the same Region, so a case can pin that the registered schema
/// is the TYPE's rather than one node's item.
const OTHER_NODE: &str = "i-0abc1234";

/// A capture time in the form the modelled `InventoryItemCaptureTime` spells (pattern
/// `^(20)[0-9][0-9]-(0[1-9]|1[012])-([12][0-9]|3[01]|0[1-9])(T)…(Z)$`).
const CAPTURED: &str = "2024-03-04T05:06:07Z";

/// The schema version a registration is written at (`InventoryItemSchemaVersion`, pattern
/// `^([0-9]{1,6})(\.[0-9]{1,6})$`).
const VERSION: &str = "2.0";

/// A second schema version, for the row that registers a set of its own.
const OTHER_VERSION: &str = "3.0";

/// The custom type the writes here use, well formed under the custom-name pattern
/// `^(Custom:)([a-zA-Z0-9]{3,93})$`.
const CUSTOM_TYPE: &str = "Custom:RackInfo";

/// A router with just this service mounted, and the engine behind it — with the two nodes the
/// cases write about already registered.
async fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = router_with_engine();
    register(&engine, REGION, NODE);
    register(&engine, REGION, OTHER_NODE);
    (router, engine)
}

/// `PutInventory` of one item of `CUSTOM_TYPE` at `version` about `node_id`, carrying `entries`.
async fn put_version(
    router: &MultiServiceRouter,
    node_id: &str,
    version: &str,
    entries: &Value,
) -> (u16, Value) {
    call(
        router,
        "PutInventory",
        &json!({
            "InstanceId": node_id,
            "Items": [{
                "TypeName": CUSTOM_TYPE,
                "SchemaVersion": version,
                "CaptureTime": CAPTURED,
                "Content": entries,
            }],
        }),
    )
    .await
}

/// `PutInventory` of one item of `CUSTOM_TYPE` at [`VERSION`] about `node_id`.
async fn put(router: &MultiServiceRouter, node_id: &str, entries: &Value) -> (u16, Value) {
    put_version(router, node_id, VERSION, entries).await
}

/// The write that REGISTERS the type's attribute set, which every case starts from.
async fn register_schema(router: &MultiServiceRouter, case: &str, entries: &Value) {
    let (status, body) = put(router, NODE, entries).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the write that registers the schema must be served, got {status} with \
         {body}"
    );
}

/// The fixed text of a content-mismatch refusal, with the two attribute lists it names split
/// out — written here independently of the service: `input item [` and `] but required [` and
/// `].`, every attribute of a list followed by a space. The names come back in the order the
/// refusal stated them.
fn mismatch_lists(case: &str, message: &str) -> (Vec<String>, Vec<String>) {
    let opened = message
        .strip_prefix("Custom inventory item doesn't match schema: input item [")
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the mismatch sentence, got {message:?}")
        });
    let (present, rest) = opened.split_once("] but required [").unwrap_or_else(|| {
        panic!("case {case:?}: expected the mismatch sentence, got {message:?}")
    });
    let required = rest.strip_suffix("].").unwrap_or_else(|| {
        panic!("case {case:?}: expected the mismatch sentence, got {message:?}")
    });
    (names(case, present), names(case, required))
}

/// The names one bracketed list of the refusal above carries, in the order it states them.
fn names(case: &str, list: &str) -> Vec<String> {
    assert!(
        list.is_empty() || list.ends_with(' '),
        "case {case:?}: every attribute of a list is followed by a space, got {list:?}"
    );
    list.split_terminator(' ').map(str::to_string).collect()
}

/// `names`, sorted — the form two attribute lists are compared as SETS in, which is what the
/// rule is about: the order a list states its attributes in is the order the request's own body
/// stated them, and the serializer behind this harness promises no key order.
fn sorted(names: &[String]) -> Vec<String> {
    let mut sorted = names.to_vec();
    sorted.sort_unstable();
    sorted
}

/// The attribute names `row.present`/`row.required` name, sorted.
fn sorted_set(names: &[&str]) -> Vec<String> {
    sorted(
        &names
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>(),
    )
}

/// The attribute names `GetInventorySchema` reports for `CUSTOM_TYPE`, in the order it reports
/// them — which is the order the write that registered the type stated them.
async fn reported_attributes(router: &MultiServiceRouter) -> Vec<String> {
    let (status, reported) = call(
        router,
        "GetInventorySchema",
        &json!({ "TypeName": CUSTOM_TYPE }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a schema read of {CUSTOM_TYPE} must be served, got {status} with {reported}"
    );
    reported["Schemas"]
        .as_array()
        .unwrap_or_else(|| panic!("a schema read reports Schemas, got {reported}"))
        .iter()
        .flat_map(|schema| {
            schema["Attributes"]
                .as_array()
                .unwrap_or_else(|| panic!("a reported schema carries Attributes, got {schema}"))
                .iter()
                .map(|attribute| attribute["Name"].as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// RULE: a write of a type and schema version the Region already holds an item of must carry
/// that registration's attribute set; one carrying another set is refused
/// `InvalidItemContentException`, naming the attributes it carried and the ones required.
#[tokio::test]
async fn a_write_that_does_not_carry_the_registered_attribute_set_is_refused() {
    /// One row: the content that registers the schema, the content the second write carries,
    /// the node it is about, and the two attribute lists the refusal must name.
    struct Row {
        case: &'static str,
        registered: Value,
        written: Value,
        node: &'static str,
        present: &'static [&'static str],
        required: &'static [&'static str],
    }

    let rows = [
        Row {
            case: "one of two registered attributes dropped",
            registered: json!([{ "Widget": "gizmo", "Stage": "PreProd" }]),
            written: json!([{ "Widget": "gizmo" }]),
            node: NODE,
            present: &["Widget"],
            required: &["Widget", "Stage"],
        },
        Row {
            case: "two of three registered attributes dropped",
            registered: json!([{ "Widget": "gizmo", "Stage": "PreProd", "Units": "42" }]),
            written: json!([{ "Stage": "Prod" }]),
            node: NODE,
            present: &["Stage"],
            required: &["Widget", "Stage", "Units"],
        },
        Row {
            case: "an attribute the schema does not stand for added",
            registered: json!([{ "Widget": "gizmo" }]),
            written: json!([{ "Widget": "gizmo", "Stage": "PreProd" }]),
            node: NODE,
            present: &["Widget", "Stage"],
            required: &["Widget"],
        },
        Row {
            case: "one registered attribute swapped for another",
            registered: json!([{ "Widget": "gizmo", "Stage": "PreProd" }]),
            written: json!([{ "Widget": "gizmo", "Tier": "gold" }]),
            node: NODE,
            present: &["Widget", "Tier"],
            required: &["Widget", "Stage"],
        },
        Row {
            case: "an entry carrying no attribute at all",
            registered: json!([{ "Widget": "gizmo" }]),
            written: json!([{}]),
            node: NODE,
            present: &[],
            required: &["Widget"],
        },
        Row {
            case: "the second of two entries of one item breaks the set",
            registered: json!([{ "Widget": "gizmo" }]),
            written: json!([{ "Widget": "gizmo" }, { "Widget": "doodad", "Stage": "Prod" }]),
            node: NODE,
            present: &["Widget", "Stage"],
            required: &["Widget"],
        },
        Row {
            case: "another node of the Region writes another set under the same type",
            registered: json!([{ "Widget": "gizmo", "Stage": "PreProd" }]),
            written: json!([{ "Widget": "gizmo" }]),
            node: OTHER_NODE,
            present: &["Widget"],
            required: &["Widget", "Stage"],
        },
    ];

    for row in rows {
        let case = row.case;
        let (router, _engine) = service().await;
        register_schema(&router, case, &row.registered).await;

        let (status, body) = put(&router, row.node, &row.written).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidItemContentException"),
            "case {case:?}: expected the content-mismatch refusal, got {status} with {body}"
        );
        let (present, required) = mismatch_lists(case, &error_message(&body));
        assert_eq!(
            (sorted(&present), sorted(&required)),
            (sorted_set(row.present), sorted_set(row.required)),
            "case {case:?}: expected the refusal to name both attribute lists, got {:?}",
            error_message(&body)
        );
        assert_eq!(
            body["TypeName"].as_str(),
            Some(CUSTOM_TYPE),
            "case {case:?}: the refusal names the type it was taken for, got {body}"
        );
    }
}

/// RULE: how large one inventory item is — how many entries it carries, and how many attributes
/// each entry carries — is bounded before anything is stored: an item at a ceiling is written,
/// one past it is refused `ItemSizeLimitExceededException` and writes nothing.
#[tokio::test]
async fn an_item_past_a_size_ceiling_is_refused() {
    /// One entry of `attributes` attributes, named so each is distinct.
    fn entry(attributes: usize) -> Value {
        Value::Object(
            (0..attributes)
                .map(|index| (format!("Attribute{index}"), json!("value")))
                .collect(),
        )
    }

    /// `entries` entries, each of `attributes` attributes.
    fn content(entries: usize, attributes: usize) -> Value {
        Value::Array((0..entries).map(|_| entry(attributes)).collect())
    }

    let attributes = custom_inventory::MAX_ITEM_ATTRIBUTES;
    let entries = custom_inventory::MAX_ITEM_ENTRIES;
    // (case, the content the write carries, the status and code it must answer)
    let rows: [(&str, Value, u16, &str); 5] = [
        ("one entry of one attribute", content(1, 1), 200, ""),
        (
            "the attribute ceiling itself",
            content(1, attributes),
            200,
            "",
        ),
        (
            "one attribute past the ceiling",
            content(1, attributes + 1),
            400,
            "ItemSizeLimitExceededException",
        ),
        ("the entry ceiling itself", content(entries, 1), 200, ""),
        (
            "one entry past the ceiling",
            content(entries + 1, 1),
            400,
            "ItemSizeLimitExceededException",
        ),
    ];

    for (case, written, expected_status, expected_code) in rows {
        let (router, _engine) = service().await;
        let (status, body) = put(&router, NODE, &written).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {expected_status}, got {status} with {body}"
        );
        if expected_code.is_empty() {
            continue;
        }
        assert_eq!(
            (error_code(&body).as_str(), body["TypeName"].as_str()),
            (expected_code, Some(CUSTOM_TYPE)),
            "case {case:?}: expected {expected_code} for {CUSTOM_TYPE}, got {body}"
        );
        // The refused write stored nothing, so the type it named stands for nothing.
        let (status, read) = call(
            &router,
            "ListInventoryEntries",
            &json!({ "InstanceId": NODE, "TypeName": CUSTOM_TYPE }),
        )
        .await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (400, "InvalidTypeNameException"),
            "case {case:?}: the refused write defined nothing, got {status} with {read}"
        );
    }
}

/// RULE: a refused write leaves the type as it was — the item the registration wrote is still
/// the item the read reports, and the schema still stands for the registered attribute set.
#[tokio::test]
async fn a_refused_write_changes_neither_the_item_nor_the_schema() {
    let case = "a write dropping a registered attribute";
    let (router, _engine) = service().await;
    let registered = json!([{ "Widget": "gizmo", "Stage": "PreProd" }]);
    register_schema(&router, case, &registered).await;

    let (status, body) = put(&router, NODE, &json!([{ "Widget": "doodad" }])).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "InvalidItemContentException"),
        "case {case:?}: expected the content-mismatch refusal, got {status} with {body}"
    );

    // The attributes the refusal says are required are the ones the schema read reports, in
    // the same order: the registration is ONE statement of what the type stands for, so the
    // refusal and the discovery call cannot disagree about it.
    let (_, required) = mismatch_lists(case, &error_message(&body));
    assert_eq!(
        required,
        reported_attributes(&router).await,
        "case {case:?}: the refusal names the attributes the schema read reports"
    );

    let (status, read) = call(
        &router,
        "ListInventoryEntries",
        &json!({ "InstanceId": NODE, "TypeName": CUSTOM_TYPE }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read must be served, got {status} with {read}"
    );
    assert_eq!(
        read["Entries"], registered,
        "case {case:?}: the refused write persisted nothing, got {read}"
    );
    assert_eq!(
        sorted(&reported_attributes(&router).await),
        vec!["Stage".to_string(), "Widget".to_string()],
        "case {case:?}: the registered schema is unchanged"
    );
}

/// RULE: a write carrying exactly the registered attribute set is served whatever its values,
/// its key order and however many entries it holds, and a write at another schema version
/// registers that version's own set.
#[tokio::test]
async fn a_write_carrying_the_registered_set_or_another_version_is_served() {
    /// One row: the content that registers the schema, the content the second write carries,
    /// the schema version it names and the node it is about.
    struct Row {
        case: &'static str,
        registered: Value,
        written: Value,
        version: &'static str,
        node: &'static str,
    }

    let rows = [
        Row {
            case: "the same set with other values",
            registered: json!([{ "Widget": "gizmo", "Stage": "PreProd" }]),
            written: json!([{ "Widget": "doodad", "Stage": "Prod" }]),
            version: VERSION,
            node: NODE,
        },
        Row {
            case: "the same set in another key order",
            registered: json!([{ "Widget": "gizmo", "Stage": "PreProd" }]),
            written: json!([{ "Stage": "Prod", "Widget": "doodad" }]),
            version: VERSION,
            node: NODE,
        },
        Row {
            case: "several entries, each carrying the registered set",
            registered: json!([{ "Widget": "gizmo", "Units": "42" }]),
            written: json!([
                { "Widget": "doodad", "Units": "7" },
                { "Widget": "sprocket", "Units": "8" },
            ]),
            version: VERSION,
            node: NODE,
        },
        Row {
            case: "another node of the Region writing the registered set",
            registered: json!([{ "Widget": "gizmo", "Stage": "PreProd" }]),
            written: json!([{ "Widget": "doodad", "Stage": "Prod" }]),
            version: VERSION,
            node: OTHER_NODE,
        },
        Row {
            case: "another schema version registering a set of its own",
            registered: json!([{ "Widget": "gizmo", "Stage": "PreProd" }]),
            written: json!([{ "Widget": "gizmo", "Tier": "gold" }]),
            version: OTHER_VERSION,
            node: NODE,
        },
        Row {
            case: "an item carrying no entry at all",
            registered: json!([{ "Widget": "gizmo", "Stage": "PreProd" }]),
            written: json!([]),
            version: VERSION,
            node: NODE,
        },
    ];

    for row in rows {
        let case = row.case;
        let (router, _engine) = service().await;
        register_schema(&router, case, &row.registered).await;

        let (status, body) = put_version(&router, row.node, row.version, &row.written).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the write must be served, got {status} with {body}"
        );

        let (status, read) = call(
            &router,
            "ListInventoryEntries",
            &json!({ "InstanceId": row.node, "TypeName": CUSTOM_TYPE }),
        )
        .await;
        assert_eq!(
            (status, &read["Entries"]),
            (200, &row.written),
            "case {case:?}: the read reports what the write persisted, got {status} with {read}"
        );
    }
}

/// RULE: a schema states ONE version's attribute set — the version the last write wrote — and
/// each version's registration stands on its own: a write of a version the Region still holds
/// an item of is held to THAT version's set, whatever another version registered.
#[tokio::test]
async fn each_schema_version_carries_its_own_registered_set() {
    let case = "two schema versions of one type, held by two nodes";
    let (router, _engine) = service().await;
    let first = json!([{ "Widget": "gizmo", "Stage": "PreProd" }]);
    let second = json!([{ "Widget": "gizmo", "Tier": "gold" }]);
    register_schema(&router, case, &first).await;
    let (status, body) = put_version(&router, OTHER_NODE, OTHER_VERSION, &second).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a write at another schema version registers that version, got {status} \
         with {body}"
    );

    // The schema read reports the version the last write wrote, with that version's own
    // attributes — one schema states one version's set.
    let (status, reported) = call(
        &router,
        "GetInventorySchema",
        &json!({ "TypeName": CUSTOM_TYPE }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the schema read must be served, got {status} with {reported}"
    );
    assert_eq!(
        reported["Schemas"][0]["Version"].as_str(),
        Some(OTHER_VERSION),
        "case {case:?}: the schema carries the version the last write wrote, got {reported}"
    );
    assert_eq!(
        sorted(&reported_attributes(&router).await),
        vec!["Tier".to_string(), "Widget".to_string()],
        "case {case:?}: the schema carries that version's attributes, got {reported}"
    );

    // The first version's registration still stands for the node that holds it: its own set is
    // served, and the other version's set is refused naming the first version's attributes.
    let (status, body) = put(&router, NODE, &first).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the first version's own set is still served, got {status} with {body}"
    );
    let (status, body) = put(&router, NODE, &second).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "InvalidItemContentException"),
        "case {case:?}: the other version's set is not this version's, got {status} with {body}"
    );
    let (_, required) = mismatch_lists(case, &error_message(&body));
    assert_eq!(
        sorted(&required),
        sorted_set(&["Stage", "Widget"]),
        "case {case:?}: the refusal names the set this version registered, got {:?}",
        error_message(&body)
    );
}

/// RULE: past the custom schema count this endpoint enforces, a write that would register one
/// more type is refused `CustomSchemaCountLimitExceededException` naming the count it was
/// refused at — and a type the Region already holds is still written.
#[tokio::test]
async fn a_write_past_the_enforced_custom_schema_count_is_refused() {
    let (router, _engine) = service().await;
    let enforced = custom_inventory::MAX_CUSTOM_INVENTORY_TYPES;
    let entries = json!([{ "Widget": "gizmo" }]);

    // Register one type per count the endpoint enforces, each through the front door.
    for index in 0..enforced {
        let type_name = format!("Custom:Filler{index}");
        let (status, body) = call(
            &router,
            "PutInventory",
            &json!({
                "InstanceId": NODE,
                "Items": [{
                    "TypeName": type_name,
                    "SchemaVersion": VERSION,
                    "CaptureTime": CAPTURED,
                    "Content": entries,
                }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "the write of {type_name} is within the enforced count, got {status} with {body}"
        );
    }

    // (case, the type the next write names, the status and code it must answer)
    let rows: [(&str, &str, u16, &str); 2] = [
        (
            "a type the Region already holds",
            "Custom:Filler0",
            200,
            "",
        ),
        (
            "a type past the enforced count",
            CUSTOM_TYPE,
            400,
            "CustomSchemaCountLimitExceededException",
        ),
    ];
    for (case, type_name, expected_status, expected_code) in rows {
        let (status, body) = call(
            &router,
            "PutInventory",
            &json!({
                "InstanceId": NODE,
                "Items": [{
                    "TypeName": type_name,
                    "SchemaVersion": VERSION,
                    "CaptureTime": CAPTURED,
                    "Content": entries,
                }],
            }),
        )
        .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {expected_status}, got {status} with {body}"
        );
        if expected_code.is_empty() {
            continue;
        }
        assert_eq!(
            error_code(&body).as_str(),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body}"
        );
        // The sentence, written out here independently of the service: fixed text apart from
        // the count the write was refused at.
        assert_eq!(
            error_message(&body),
            format!("Total custom inventory schema count exceeded the limit {enforced}."),
            "case {case:?}: the refusal names the count it was taken at, got {:?}",
            error_message(&body)
        );
        // The refused write registered nothing: the type it named stands for nothing.
        let (status, read) = call(
            &router,
            "ListInventoryEntries",
            &json!({ "InstanceId": NODE, "TypeName": type_name }),
        )
        .await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (400, "InvalidTypeNameException"),
            "case {case:?}: the refused write defined nothing, got {status} with {read}"
        );
    }
}
