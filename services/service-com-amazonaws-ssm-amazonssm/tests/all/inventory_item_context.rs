// SPDX-License-Identifier: CC0-1.0

//! The `Context` an inventory item carries, where `PutInventory` writes the caller's own
//! inventory about a managed node.
//!
//! RULE under test: **`InventoryItem$Context` is "a map of associated properties for a
//! specified inventory type. For example, with this attribute, you can specify the
//! `ExecutionId`, `ExecutionType`, `ComplianceType` properties of the `AWS:ComplianceItem`
//! type" (`smithy_query(query="shape", shape="InventoryItem")`), so the properties a context
//! states are the ones Systems Manager DEFINES for a type it gathers itself — a type of the
//! caller's own `Custom:` namespace, which is the only kind `PutInventory` writes, defines
//! none and therefore takes no context. A put stating one for such a type is refused the
//! modelled `UnsupportedInventoryItemContextException` (HTTP 400), which names that type in
//! its own `TypeName` member, and writes NOTHING; a put stating no property — no `Context`
//! member at all, or an empty map, which the modelled `0..50` length admits — is served, and
//! what it wrote is what a later read reports.**
//!
//! Why it matters beyond the refusal: a context is the only part of an item this service
//! cannot store, because nothing of the type it is stated for gives those properties a
//! meaning. An emulator that answered 200 and dropped it would report success for data it
//! discarded, and the caller's own handling of the refusal would never be exercised.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router, and the node, the item and the two calls are the ones the
//! sibling suite `all/custom_inventory.rs` writes custom inventory with. The ONE thing seeded
//! through the engine is a registered managed node, because no call of this service registers
//! one; every item is written with `PutInventory` itself and read back with
//! `ListInventoryEntries`.
//!
//! The rows differ along what the rule generalises over: whether a context is absent, empty or
//! states properties, how many properties it states and whether their values are empty, WHICH
//! custom type the item is written under, which item of a multi-item put carries it, and
//! whether the item's type name is one a put may write at all.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::custom_inventory::{
    entries, item, put, register, CUSTOM_TYPE, NODE, SECOND_CUSTOM_TYPE,
};
use crate::parameter_store::{error_code, error_message, router_with_engine, REGION};

/// The one type Systems Manager publishes whose properties a context would state, which is
/// also a type no put may write (`all/custom_inventory.rs` covers that refusal): it stands
/// here for the case where the TYPE NAME decides the call before the context is read.
const COMPLIANCE_ITEM: &str = "AWS:ComplianceItem";

/// The sentence a context stated for a type that takes none is refused with, written out here
/// independently of the service: the sentence the modelled
/// `UnsupportedInventoryItemContextException` states for that condition, with nothing of the
/// request in it.
const UNSUPPORTED_CONTEXT: &str = "The Context attribute that you specified for the \
                                   InventoryItem isn't allowed for this inventory type. You \
                                   can only use the Context attribute with inventory types \
                                   like AWS:ComplianceItem.";

/// The content an item carries where a row is not about the content.
fn content() -> Value {
    json!([{ "RackLocation": "Bay B/Row 4" }])
}

/// The sentence a read of a custom type name nothing stands under is refused with, written out
/// independently of the service — which is how a case sees that a refused put wrote nothing.
fn not_defined(type_name: &str) -> String {
    format!("Inventory type name {type_name} is not defined.")
}

/// A router with just this service mounted, holding one registered node — the handle a case
/// puts inventory about.
fn service() -> MultiServiceRouter {
    let (router, engine) = router_with_engine();
    register(&engine, REGION, NODE);
    router
}

/// One inventory item of `type_name` carrying `content()`, with `context` stated for it when a
/// row states one.
fn item_with_context(type_name: &str, context: Option<Value>) -> Value {
    let mut item = item(type_name, content());
    if let Some(context) = context {
        item["Context"] = context;
    }
    item
}

/// `PutInventory` of `items` about the seeded node.
async fn put_items(router: &MultiServiceRouter, items: Value) -> (u16, Value) {
    put(router, REGION, NODE, items).await
}

/// Assert that `type_name` stands for nothing about the seeded node: the read of it is refused
/// with the undefined-type sentence, which is what a put that wrote nothing leaves behind.
async fn assert_nothing_written(router: &MultiServiceRouter, case: &str, type_name: &str) {
    let (status, read) = entries(router, NODE, type_name).await;
    assert_eq!(
        (status, error_code(&read).as_str()),
        (400, "InvalidTypeNameException"),
        "case {case:?}: expected the refused put to define nothing, got {status} with {read}"
    );
    assert_eq!(
        error_message(&read),
        not_defined(type_name),
        "case {case:?}: expected the undefined-type sentence, got {read}"
    );
}

/// RULE: a context states properties of the inventory type it is carried for, so a put of a
/// type of the caller's own namespace is refused where it states any property and served where
/// it states none — and the refused put writes nothing, while the served put's entries are
/// what a later read reports.
#[tokio::test]
async fn a_context_stated_for_a_type_that_defines_none_is_refused() {
    /// One row: the context the item carries, and whether the put must be served.
    struct Row {
        case: &'static str,
        type_name: &'static str,
        context: Option<Value>,
        served: bool,
    }

    let rows = [
        Row {
            case: "no Context member at all",
            type_name: CUSTOM_TYPE,
            context: None,
            served: true,
        },
        Row {
            // The modelled `InventoryItemContentContext` length is 0..50, so an empty map is a
            // value the walk admits, and it states no property to refuse.
            case: "an empty Context",
            type_name: CUSTOM_TYPE,
            context: Some(json!({})),
            served: true,
        },
        Row {
            case: "one property",
            type_name: CUSTOM_TYPE,
            context: Some(json!({ "ExecutionId": "f8b1a0c4" })),
            served: false,
        },
        Row {
            case: "the three properties the model's own example names",
            type_name: CUSTOM_TYPE,
            context: Some(json!({
                "ExecutionId": "f8b1a0c4",
                "ExecutionType": "Command",
                "ComplianceType": "Custom:RackCompliance",
            })),
            served: false,
        },
        Row {
            // A property stated with an empty value is still a property: the map is not empty.
            case: "one property whose value is empty",
            type_name: CUSTOM_TYPE,
            context: Some(json!({ "ExecutionId": "" })),
            served: false,
        },
        Row {
            case: "a property stated for another custom type",
            type_name: SECOND_CUSTOM_TYPE,
            context: Some(json!({ "ExecutionType": "Command" })),
            served: false,
        },
        Row {
            case: "another custom type stating no property",
            type_name: SECOND_CUSTOM_TYPE,
            context: Some(json!({})),
            served: true,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = service();
        let (status, body) = put_items(
            &router,
            json!([item_with_context(row.type_name, row.context.clone())]),
        )
        .await;

        if row.served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected a put stating no property to be served, got {status} \
                 with {body}"
            );
            let (status, read) = entries(&router, NODE, row.type_name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the read must be served, got {status} with {read}"
            );
            assert_eq!(
                read["Entries"],
                content(),
                "case {case:?}: expected the read to report the entries the put wrote, got {read}"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "UnsupportedInventoryItemContextException"),
            "case {case:?}: expected the put to be refused, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            UNSUPPORTED_CONTEXT,
            "case {case:?}: expected the refusal to carry the sentence for this condition, got \
             {body}"
        );
        assert_eq!(
            body["TypeName"].as_str(),
            Some(row.type_name),
            "case {case:?}: expected the refusal to name the type the context was stated for, \
             got {body}"
        );
        assert_nothing_written(&router, case, row.type_name).await;
    }
}

/// RULE: every item of a request is judged before the first one is written, so a context on
/// ANY item refuses the whole put and leaves the node's inventory as it was — while the type
/// NAME decides the call first, a name a put may not write at all being refused whatever
/// context the item carries beside it.
#[tokio::test]
async fn a_context_on_any_item_refuses_the_whole_put() {
    // (case, the items the put carries, the error code expected, the type the refusal names)
    let rows: [(&str, Value, &str, &str); 4] = [
        (
            "the first item of two carries it",
            json!([
                item_with_context(CUSTOM_TYPE, Some(json!({ "ExecutionId": "f8b1a0c4" }))),
                item_with_context(SECOND_CUSTOM_TYPE, None),
            ]),
            "UnsupportedInventoryItemContextException",
            CUSTOM_TYPE,
        ),
        (
            "the second item of two carries it",
            json!([
                item_with_context(CUSTOM_TYPE, None),
                item_with_context(SECOND_CUSTOM_TYPE, Some(json!({ "ExecutionType": "Command" }))),
            ]),
            "UnsupportedInventoryItemContextException",
            SECOND_CUSTOM_TYPE,
        ),
        (
            // The name is what says WHICH type a context would state properties of, so the
            // name a put may not write is refused before the context is read at all.
            "an item of a published type carrying a context",
            json!([item_with_context(
                COMPLIANCE_ITEM,
                Some(json!({ "ExecutionId": "f8b1a0c4" })),
            )]),
            "InvalidTypeNameException",
            COMPLIANCE_ITEM,
        ),
        (
            "a good item beside an item of a published type carrying a context",
            json!([
                item_with_context(CUSTOM_TYPE, None),
                item_with_context(COMPLIANCE_ITEM, Some(json!({ "ExecutionId": "f8b1a0c4" }))),
            ]),
            "InvalidTypeNameException",
            COMPLIANCE_ITEM,
        ),
    ];

    for (case, items, expected_code, named) in rows {
        let router = service();
        let (status, body) = put_items(&router, items).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, expected_code),
            "case {case:?}: expected the put to be refused, got {status} with {body}"
        );
        if expected_code == "UnsupportedInventoryItemContextException" {
            assert_eq!(
                error_message(&body),
                UNSUPPORTED_CONTEXT,
                "case {case:?}: expected the refusal to carry the sentence for this condition, \
                 got {body}"
            );
            assert_eq!(
                body["TypeName"].as_str(),
                Some(named),
                "case {case:?}: expected the refusal to name the type the context was stated \
                 for, got {body}"
            );
        }
        // Neither item was written: the refused put defined no type at all, so the read of
        // each custom name it carried says that name stands for nothing.
        for type_name in [CUSTOM_TYPE, SECOND_CUSTOM_TYPE] {
            assert_nothing_written(&router, case, type_name).await;
        }
    }
}
