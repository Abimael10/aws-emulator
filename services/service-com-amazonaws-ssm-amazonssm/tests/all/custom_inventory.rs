// SPDX-License-Identifier: CC0-1.0

//! The CUSTOM inventory a caller writes, end to end: `PutInventory` writes items about a
//! node, `ListInventoryEntries` reads them back, `GetInventorySchema` reports the schema
//! those items make, `DeleteInventory` removes them and `DescribeInventoryDeletions` reports
//! the deletion it recorded.
//!
//! RULE: `PutInventory` writes "custom inventory items on one or more managed nodes. The
//! request adds an inventory item, if it doesn't already exist, or updates an inventory item,
//! if it does exist" (`smithy_query(query="operation", operation="PutInventory")`), so an
//! item is keyed by `(node, type name)` and what a put persisted is what a later read
//! reports; a put naming a node this account holds none of is refused `InvalidInstanceId`
//! with no message, and one naming a type outside the caller's own `Custom:` namespace is
//! refused `InvalidTypeNameException`. `DeleteInventory` then addresses a CUSTOM type only —
//! "delete a custom inventory type or the data associated with a custom Inventory type" —
//! so a predefined `AWS:` type and a custom type nothing was written under are both
//! `InvalidTypeNameException`, a `DryRun` counts without deleting, and every other form
//! removes the type's items and records a deletion the minted `DeletionId` resolves to.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call`], [`error_code`], [`error_message`],
//! [`router_with_engine`]) dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. The ONE thing seeded through the engine is a registered
//! managed node, because no call of this service registers one (the agent's own call is not
//! in this model); every inventory item is written through `PutInventory` itself.
//!
//! The rows span what the rule generalises over: how many items a put carries, how many
//! entries each item holds, the value types those entries carry, which node and which Region
//! holds them, whether the type was already held, and the delete's own `DryRun` and
//! `SchemaDeleteOption` forms.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, call_in_region, error_code, error_message, router_with_engine};

/// A second Region, to pin that inventory is written and read through the one Region its put
/// was signed for. The first is the harness's own [`crate::parameter_store::REGION`].
const OTHER_REGION: &str = "us-east-1";

/// The node every case writes about: an EC2 node id of the 17-character form the modelled
/// `InstanceId` pattern admits (`^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$`).
const NODE: &str = "i-0123456789abcdef0";

/// A second registered node, so a case can pin that an item is about ONE node.
const OTHER_NODE: &str = "i-0abc1234";

/// A node id that is well formed and names no registered node.
const UNREGISTERED: &str = "i-0dead999";

/// The instant every seeded node last reported, as a whole number of seconds.
const LAST_PING: Duration = Duration::from_secs(1_700_000_000);

/// A capture time in the form the modelled `InventoryItemCaptureTime` spells (pattern
/// `^(20)[0-9][0-9]-(0[1-9]|1[012])-([12][0-9]|3[01]|0[1-9])(T)…(Z)$`).
const CAPTURED: &str = "2024-03-04T05:06:07Z";

/// The schema version an item carries (`InventoryItemSchemaVersion`, pattern
/// `^([0-9]{1,6})(\.[0-9]{1,6})$`).
const VERSION: &str = "1.0";

/// The type name every custom item is written under, in the caller's own namespace
/// ("Custom inventory type names will start with Custom" —
/// `smithy_query(query="doc", shape="InventoryItemSchema", member="TypeName")`).
const CUSTOM_TYPE: &str = "Custom:RackInfo";

/// A second custom type, for the rows that write more than one.
const SECOND_CUSTOM_TYPE: &str = "Custom:Compliance";

/// The one type this service publishes a schema of and holds the item of as the projection
/// of the node record.
const INSTANCE_INFORMATION: &str = "AWS:InstanceInformation";

/// A router with just this service mounted, and the engine behind it — the handle a case
/// seeds the node store through.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    router_with_engine()
}

/// Register a node in `region` the way its own registration would.
fn register(engine: &Arc<engine::InMemoryEngine>, region: &str, node_id: &str) {
    let at = SystemTime::UNIX_EPOCH + LAST_PING;
    let mut node = ManagedNodeRecord::registering(node_id, RESOURCE_TYPE_EC2_INSTANCE, at);
    node.properties.platform_type = Some("Linux".to_string());
    node.properties.instance_state = Some("Active".to_string());
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(region, node),
        "a seeded node must carry the node id it is stored under"
    );
}

/// One inventory item as a put carries it.
fn item(type_name: &str, entries: Value) -> Value {
    json!({
        "TypeName": type_name,
        "SchemaVersion": VERSION,
        "CaptureTime": CAPTURED,
        "Content": entries,
    })
}

/// `PutInventory` of `items` about `node_id`, signed for `region`.
async fn put(
    router: &MultiServiceRouter,
    region: &str,
    node_id: &str,
    items: Value,
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "PutInventory",
        &json!({ "InstanceId": node_id, "Items": items }),
    )
    .await
}

/// The entries `ListInventoryEntries` reports of `type_name` about `node_id`.
async fn entries(
    router: &MultiServiceRouter,
    node_id: &str,
    type_name: &str,
) -> (u16, Value) {
    call(
        router,
        "ListInventoryEntries",
        &json!({ "InstanceId": node_id, "TypeName": type_name }),
    )
    .await
}

/// The schemas `GetInventorySchema` reports, keyed by type name.
async fn schemas(router: &MultiServiceRouter, body: &Value) -> Value {
    let (status, reported) = call(router, "GetInventorySchema", body).await;
    assert_eq!(
        status, 200,
        "GetInventorySchema {body} must be served, got {status} with {reported}"
    );
    reported
}

/// The type names a schema read reports, in the order it reports them.
fn schema_types(reported: &Value) -> Vec<String> {
    reported["Schemas"]
        .as_array()
        .unwrap_or_else(|| panic!("a schema read reports Schemas, got {reported}"))
        .iter()
        .map(|schema| schema["TypeName"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The CUSTOM type names a schema read reports, in the order it reports them: the ones of the
/// caller's own namespace, as against the catalog Systems Manager publishes for every account
/// (covered by `all/inventory_type_catalog.rs`).
fn custom_schema_types(reported: &Value) -> Vec<String> {
    schema_types(reported)
        .into_iter()
        .filter(|type_name| type_name.starts_with("Custom:"))
        .collect()
}

/// RULE: what a put persisted is what a later read reports — one item per `(node, type)`,
/// its entries, schema version and capture time read back exactly as the put supplied them,
/// and a second put of the same type on the same node UPDATES that item rather than adding
/// a second one.
#[tokio::test]
async fn a_put_is_read_back_as_the_item_of_that_node_and_type() {
    /// One row: the entries a put carries, and what the read must report back.
    struct Row {
        case: &'static str,
        entries: Value,
    }

    let rows = [
        Row {
            case: "one entry of one attribute",
            entries: json!([{ "RackLocation": "Bay B/Row 4" }]),
        },
        Row {
            case: "one entry of several attributes, one of them numeric",
            entries: json!([{ "RackLocation": "Bay C/Row 1", "Units": "42" }]),
        },
        Row {
            case: "several entries",
            entries: json!([
                { "RackLocation": "Bay D/Row 2" },
                { "RackLocation": "Bay D/Row 3" },
            ]),
        },
        Row {
            case: "an entry carrying no attribute at all",
            entries: json!([{}]),
        },
        Row {
            case: "an item carrying no entry at all",
            entries: json!([]),
        },
    ];

    for row in rows {
        let case = row.case;
        let (router, engine) = service();
        register(&engine, crate::parameter_store::REGION, NODE);

        let (status, body) = put(
            &router,
            crate::parameter_store::REGION,
            NODE,
            json!([item(CUSTOM_TYPE, row.entries.clone())]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the put must be served, got {status} with {body}"
        );

        let (status, read) = entries(&router, NODE, CUSTOM_TYPE).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must be served, got {status} with {read}"
        );
        assert_eq!(
            read["Entries"], row.entries,
            "case {case:?}: the read reports the entries the put wrote, got {read}"
        );
        for (member, expected) in [
            ("InstanceId", NODE),
            ("TypeName", CUSTOM_TYPE),
            ("SchemaVersion", VERSION),
            ("CaptureTime", CAPTURED),
        ] {
            assert_eq!(
                read[member].as_str(),
                Some(expected),
                "case {case:?}: {member} is what the put supplied, got {read}"
            );
        }

        // A second put of the same type REPLACES the item rather than adding a second one.
        let replacement = json!([{ "RackLocation": "Bay Z/Row 9" }]);
        let (status, body) = put(
            &router,
            crate::parameter_store::REGION,
            NODE,
            json!([item(CUSTOM_TYPE, replacement.clone())]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the second put must be served, got {status} with {body}"
        );
        let (_, read) = entries(&router, NODE, CUSTOM_TYPE).await;
        assert_eq!(
            read["Entries"], replacement,
            "case {case:?}: the second put replaces the item, got {read}"
        );
    }
}

/// RULE: an item is about ONE node at ONE Region's endpoint, so a read of another node, or
/// of the same node through another Region, reports none of it.
#[tokio::test]
async fn an_item_belongs_to_the_node_and_region_its_put_named() {
    let (router, engine) = service();
    for region in [crate::parameter_store::REGION, OTHER_REGION] {
        register(&engine, region, NODE);
        register(&engine, region, OTHER_NODE);
    }
    let written = json!([{ "RackLocation": "Bay A/Row 1" }]);
    let (status, body) = put(
        &router,
        crate::parameter_store::REGION,
        NODE,
        json!([item(CUSTOM_TYPE, written.clone())]),
    )
    .await;
    assert_eq!(
        status, 200,
        "the put must be served, got {status} with {body}"
    );

    // (case, the Region the read is signed for, the node it asks about, the entries it must
    // report)
    let rows: [(&str, &str, &str, Value); 2] = [
        (
            "the node and Region the put named",
            crate::parameter_store::REGION,
            NODE,
            written.clone(),
        ),
        (
            "another node of the same Region",
            crate::parameter_store::REGION,
            OTHER_NODE,
            json!([]),
        ),
    ];
    for (case, region, node_id, expected) in rows {
        let (status, read) = call_in_region(
            &router,
            region,
            "ListInventoryEntries",
            &json!({ "InstanceId": node_id, "TypeName": CUSTOM_TYPE }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must be served, got {status} with {read}"
        );
        assert_eq!(
            read["Entries"], expected,
            "case {case:?}: expected {expected}, got {read}"
        );
    }

    // The type a put DEFINED belongs to that put's Region too, so the same name through
    // another Region's endpoint stands for nothing there.
    let case = "the same node through another Region";
    let (status, read) = call_in_region(
        &router,
        OTHER_REGION,
        "ListInventoryEntries",
        &json!({ "InstanceId": NODE, "TypeName": CUSTOM_TYPE }),
    )
    .await;
    assert_eq!(
        (status, error_code(&read).as_str()),
        (400, "InvalidTypeNameException"),
        "case {case:?}: the type is defined in the Region its put named, got {status} with \
         {read}"
    );
}

/// RULE: a put is refused where the request names a node this account holds none of
/// (`InvalidInstanceId`, with no message of its own) or a type outside the caller's own
/// namespace (`InvalidTypeNameException`), and a refused put writes NOTHING.
#[tokio::test]
async fn a_put_is_refused_for_an_unknown_node_or_a_type_that_is_not_the_callers() {
    // (case, the node the put names, the type it writes, the error code expected)
    let rows: [(&str, &str, &str, &str); 6] = [
        (
            "a well-formed node id nothing registered",
            UNREGISTERED,
            CUSTOM_TYPE,
            "InvalidInstanceId",
        ),
        (
            "a hybrid node id nothing registered",
            "mi-01234567890123456",
            CUSTOM_TYPE,
            "InvalidInstanceId",
        ),
        (
            "the type whose item is the node's own projection",
            NODE,
            INSTANCE_INFORMATION,
            "InvalidTypeNameException",
        ),
        (
            "another type Systems Manager publishes",
            NODE,
            "AWS:Application",
            "InvalidTypeNameException",
        ),
        (
            "a type of the AWS namespace nothing publishes",
            NODE,
            "AWS:NotAType",
            "InvalidTypeNameException",
        ),
        // The node is resolved before the items are read, so an unknown node beside a bad
        // type is still the node's refusal.
        (
            "an unknown node AND a predefined type",
            UNREGISTERED,
            INSTANCE_INFORMATION,
            "InvalidInstanceId",
        ),
    ];

    for (case, node_id, type_name, expected) in rows {
        let (router, engine) = service();
        register(&engine, crate::parameter_store::REGION, NODE);

        let (status, body) = put(
            &router,
            crate::parameter_store::REGION,
            node_id,
            json!([item(type_name, json!([{ "RackLocation": "Bay B/Row 4" }]))]),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the put must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            expected,
            "case {case:?}: expected {expected}, got {body}"
        );
        if expected == "InvalidInstanceId" {
            assert_eq!(
                error_message(&body),
                "",
                "case {case:?}: this refusal carries no message, got {body}"
            );
        }
        // Nothing was written: the refused put defined no type, so the name it carried stands
        // for nothing and the read of it says so. A type name the read cannot address either
        // is left to its own case above.
        if type_name == CUSTOM_TYPE {
            let (status, read) = entries(&router, NODE, type_name).await;
            assert_eq!(
                (status, error_code(&read).as_str()),
                (400, "InvalidTypeNameException"),
                "case {case:?}: a refused put defines nothing, got {status} with {read}"
            );
            assert_eq!(
                error_message(&read),
                format!("Inventory type name {type_name} is not defined."),
                "case {case:?}: expected the undefined-type sentence, got {read}"
            );
        }
    }
}

/// RULE: every item of a request is checked before the first is written, so a request
/// carrying one bad item leaves the node's inventory as it was.
#[tokio::test]
async fn a_put_carrying_one_bad_item_writes_none_of_them() {
    let (router, engine) = service();
    register(&engine, crate::parameter_store::REGION, NODE);
    let (status, body) = put(
        &router,
        crate::parameter_store::REGION,
        NODE,
        json!([
            item(CUSTOM_TYPE, json!([{ "RackLocation": "Bay B/Row 4" }])),
            item(INSTANCE_INFORMATION, json!([{ "InstanceId": NODE }])),
        ]),
    )
    .await;
    assert_eq!(
        status, 400,
        "the put must be refused, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidTypeNameException",
        "expected the type refusal, got {body}"
    );
    // The good item was not written either, so its type was never defined and the read of it
    // says so.
    let (status, read) = entries(&router, NODE, CUSTOM_TYPE).await;
    assert_eq!(
        (status, error_code(&read).as_str()),
        (400, "InvalidTypeNameException"),
        "the good item of a refused put is not written either, got {status} with {read}"
    );
    assert_eq!(
        error_message(&read),
        format!("Inventory type name {CUSTOM_TYPE} is not defined."),
        "expected the undefined-type sentence, got {read}"
    );
}

/// RULE: a schema read reports the CUSTOM types the Region holds items of, whose attributes
/// and data types are what those items say, beside the catalog Systems Manager publishes for
/// every account.
#[tokio::test]
async fn a_schema_read_reports_the_types_the_items_make() {
    let (router, engine) = service();
    register(&engine, crate::parameter_store::REGION, NODE);

    // Before any put: the published catalog, and no custom type at all.
    let reported = schemas(&router, &json!({})).await;
    assert!(
        schema_types(&reported).contains(&INSTANCE_INFORMATION.to_string()),
        "the published catalog carries the type whose item is the node's own projection, got \
         {reported}"
    );
    assert_eq!(
        custom_schema_types(&reported),
        Vec::<String>::new(),
        "with no custom item written, no custom type has a schema, got {reported}"
    );

    let (status, body) = put(
        &router,
        crate::parameter_store::REGION,
        NODE,
        json!([
            item(
                CUSTOM_TYPE,
                json!([{ "RackLocation": "Bay B/Row 4", "Units": "42" }])
            ),
            item(SECOND_CUSTOM_TYPE, json!([{ "Standard": "CIS" }])),
        ]),
    )
    .await;
    assert_eq!(
        status, 200,
        "the put must be served, got {status} with {body}"
    );

    let reported = schemas(&router, &json!({})).await;
    assert_eq!(
        custom_schema_types(&reported),
        vec![SECOND_CUSTOM_TYPE.to_string(), CUSTOM_TYPE.to_string()],
        "every type an item was written under has a schema, in type-name order, got {reported}"
    );

    // The custom type's own schema: its attributes in name order, each with the data type
    // its values carry in the case the service puts on the wire, and the schema version the
    // put wrote.
    let narrowed = schemas(&router, &json!({ "TypeName": CUSTOM_TYPE })).await;
    assert_eq!(
        narrowed["Schemas"],
        json!([{
            "TypeName": CUSTOM_TYPE,
            "Version": VERSION,
            "Attributes": [
                { "Name": "RackLocation", "DataType": "STRING" },
                { "Name": "Units", "DataType": "NUMBER" },
            ],
        }]),
        "a custom type's schema is what its items say, got {narrowed}"
    );

    // `Aggregator` reports only what an aggregation may count over.
    let aggregatable = schemas(&router, &json!({ "Aggregator": true })).await;
    assert_eq!(
        schema_types(&aggregatable),
        vec![INSTANCE_INFORMATION.to_string()],
        "only the type that supports aggregation is reported, got {aggregatable}"
    );
    let attributes = aggregatable["Schemas"][0]["Attributes"]
        .as_array()
        .unwrap_or_else(|| panic!("a schema carries Attributes, got {aggregatable}"))
        .iter()
        .map(|attribute| attribute["Name"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<String>>();
    assert_eq!(
        attributes,
        vec![
            "PlatformName".to_string(),
            "PlatformType".to_string(),
            "PlatformVersion".to_string()
        ],
        "an aggregatable schema carries the attributes aggregation is supported on, got \
         {aggregatable}"
    );

    // A sub-type schema: no type here has one.
    let sub_types = schemas(&router, &json!({ "SubType": true })).await;
    assert_eq!(
        schema_types(&sub_types),
        Vec::<String>::new(),
        "no type here has a sub-type schema, got {sub_types}"
    );

    // A type name that names no inventory type at all cannot be served.
    let (status, body) = call(
        &router,
        "GetInventorySchema",
        &json!({ "TypeName": "NotAType" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a type name of no namespace must be refused, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidTypeNameException",
        "expected the type refusal, got {body}"
    );
}

/// RULE: `DeleteInventory` addresses a CUSTOM type only, and removes the items of that type:
/// a predefined `AWS:` name and a custom name nothing was written under are both
/// `InvalidTypeNameException`, a `DryRun` counts without removing anything, and every other
/// form leaves the type holding nothing while the deletion it recorded is reported under the
/// `DeletionId` it minted.
#[tokio::test]
async fn a_delete_removes_the_items_of_a_custom_type_and_records_what_it_did() {
    /// One accepted delete: the request's own form, and whether the items survive it.
    struct Row {
        case: &'static str,
        dry_run: bool,
        schema_delete_option: Option<&'static str>,
    }

    let rows = [
        Row {
            case: "a delete of the data alone",
            dry_run: false,
            schema_delete_option: None,
        },
        Row {
            case: "a delete of the schema",
            dry_run: false,
            schema_delete_option: Some("DeleteSchema"),
        },
        Row {
            case: "a disable of the schema",
            dry_run: false,
            schema_delete_option: Some("DisableSchema"),
        },
        Row {
            case: "a dry run",
            dry_run: true,
            schema_delete_option: None,
        },
        Row {
            case: "a dry run of a schema delete",
            dry_run: true,
            schema_delete_option: Some("DeleteSchema"),
        },
    ];

    for row in rows {
        let case = row.case;
        let (router, engine) = service();
        register(&engine, crate::parameter_store::REGION, NODE);
        register(&engine, crate::parameter_store::REGION, OTHER_NODE);
        // Two nodes hold an item of the type, and one of them holds a second type too, so
        // the delete's counts and what it leaves behind are both observable.
        for node_id in [NODE, OTHER_NODE] {
            let (status, body) = put(
                &router,
                crate::parameter_store::REGION,
                node_id,
                json!([item(CUSTOM_TYPE, json!([{ "RackLocation": "Bay B/Row 4" }]))]),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the put must be served, got {status} with {body}"
            );
        }
        let (status, body) = put(
            &router,
            crate::parameter_store::REGION,
            NODE,
            json!([item(SECOND_CUSTOM_TYPE, json!([{ "Standard": "CIS" }]))]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the second type's put must be served, got {status} with {body}"
        );

        let mut request = json!({ "TypeName": CUSTOM_TYPE, "DryRun": row.dry_run });
        if let Some(option) = row.schema_delete_option {
            request["SchemaDeleteOption"] = json!(option);
        }
        let (status, deleted) = call(&router, "DeleteInventory", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the delete must be served, got {status} with {deleted}"
        );
        assert_eq!(
            deleted["TypeName"].as_str(),
            Some(CUSTOM_TYPE),
            "case {case:?}: the delete reports the type it addressed, got {deleted}"
        );
        let remaining = if row.dry_run { 2 } else { 0 };
        assert_eq!(
            deleted["DeletionSummary"],
            json!({
                "TotalCount": 2,
                "RemainingCount": remaining,
                "SummaryItems": [{ "Version": VERSION, "Count": 2, "RemainingCount": remaining }],
            }),
            "case {case:?}: the summary counts the items of that type, got {deleted}"
        );
        let deletion_id = deleted["DeletionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a delete mints a DeletionId, got {deleted}"))
            .to_string();

        // What the type holds afterwards, and what the other type still holds. A delete of
        // the SCHEMA takes the type away, so a read of it is refused rather than served the
        // nothing it holds; every other form leaves the type defined.
        let schema_deleted = !row.dry_run && row.schema_delete_option == Some("DeleteSchema");
        for node_id in [NODE, OTHER_NODE] {
            let (status, read) = entries(&router, node_id, CUSTOM_TYPE).await;
            if schema_deleted {
                assert_eq!(
                    (status, error_code(&read).as_str()),
                    (400, "InvalidTypeNameException"),
                    "case {case:?}: node {node_id}: a read of the type the delete took away is \
                     refused, got {status} with {read}"
                );
                continue;
            }
            let expected = if row.dry_run {
                json!([{ "RackLocation": "Bay B/Row 4" }])
            } else {
                json!([])
            };
            assert_eq!(
                read["Entries"], expected,
                "case {case:?}: node {node_id} holds {expected} after the delete, got {read}"
            );
        }
        let (_, untouched) = entries(&router, NODE, SECOND_CUSTOM_TYPE).await;
        assert_eq!(
            untouched["Entries"],
            json!([{ "Standard": "CIS" }]),
            "case {case:?}: a delete of one type leaves another type alone, got {untouched}"
        );

        // The deletion resolves under the id it minted.
        let (status, described) = call(
            &router,
            "DescribeInventoryDeletions",
            &json!({ "DeletionId": deletion_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the deletion must be reported, got {status} with {described}"
        );
        let reported = &described["InventoryDeletions"][0];
        assert_eq!(
            reported["DeletionId"].as_str(),
            Some(deletion_id.as_str()),
            "case {case:?}: the deletion reported is the one the id names, got {described}"
        );
        assert_eq!(
            reported["TypeName"].as_str(),
            Some(CUSTOM_TYPE),
            "case {case:?}: it reports the type it addressed, got {described}"
        );
        assert_eq!(
            reported["LastStatus"].as_str(),
            Some("Complete"),
            "case {case:?}: a deletion this endpoint performed is complete, got {described}"
        );
        assert_eq!(
            reported["DeletionSummary"], deleted["DeletionSummary"],
            "case {case:?}: the summary is the one the delete answered, got {described}"
        );
    }
}

/// RULE: the delete's refusals — a type name outside the custom namespace, and a custom name
/// this Region holds no inventory of — and the `DeletionId` of a deletion nothing recorded.
#[tokio::test]
async fn a_delete_is_refused_for_a_type_it_cannot_address() {
    /// The sentence a predefined type name is refused with, written out here independently
    /// of the service: fixed text, with nothing of the request in it.
    const PREDEFINED: &str = "Deleting predefined type names is not allowed.";

    // (case, the type name the delete names, the message expected)
    let rows: [(&str, &str, &str); 4] = [
        (
            "the type whose item is the node's own projection",
            INSTANCE_INFORMATION,
            PREDEFINED,
        ),
        ("another predefined type", "AWS:Network", PREDEFINED),
        (
            "a custom type nothing was written under",
            "Custom:NobodyWroteThis",
            "The given TypeName Custom:NobodyWroteThis is either deleted or doesn't exist",
        ),
        (
            "a custom type written under another Region",
            SECOND_CUSTOM_TYPE,
            "The given TypeName Custom:Compliance is either deleted or doesn't exist",
        ),
    ];

    let (router, engine) = service();
    register(&engine, crate::parameter_store::REGION, NODE);
    register(&engine, OTHER_REGION, NODE);
    // The second type exists in the OTHER Region only, so naming it here names nothing.
    let (status, body) = put(
        &router,
        OTHER_REGION,
        NODE,
        json!([item(SECOND_CUSTOM_TYPE, json!([{ "Standard": "CIS" }]))]),
    )
    .await;
    assert_eq!(
        status, 200,
        "the other Region's put must be served, got {status} with {body}"
    );

    for (case, type_name, expected) in rows {
        let (status, body) = call(
            &router,
            "DeleteInventory",
            &json!({ "TypeName": type_name, "SchemaDeleteOption": "DeleteSchema" }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the delete must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidTypeNameException",
            "case {case:?}: expected InvalidTypeNameException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            error_message(&body)
        );
    }

    // An id this endpoint recorded no deletion under matches no deletion, which a LISTING
    // reports as the empty list rather than refusing.
    let (status, body) = call(
        &router,
        "DescribeInventoryDeletions",
        &json!({ "DeletionId": "0ad1e69c-b8c1-4b4b-9e1b-0c5f5d0a5f20" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a listing naming an unrecorded DeletionId is a successful read, got {status} with \
         {body}"
    );
    assert_eq!(
        body["InventoryDeletions"],
        json!([]),
        "it matches no deletion, so it reports none, got {body}"
    );
}

/// RULE: `ClientToken` is an `@idempotencyToken`, so a repeat of the same delete is the SAME
/// deletion — one id, one recorded deletion, and nothing counted or removed twice.
#[tokio::test]
async fn a_repeated_delete_under_one_client_token_is_one_deletion() {
    let (router, engine) = service();
    register(&engine, crate::parameter_store::REGION, NODE);
    let (status, body) = put(
        &router,
        crate::parameter_store::REGION,
        NODE,
        json!([item(CUSTOM_TYPE, json!([{ "RackLocation": "Bay B/Row 4" }]))]),
    )
    .await;
    assert_eq!(
        status, 200,
        "the put must be served, got {status} with {body}"
    );

    let request = json!({
        "TypeName": CUSTOM_TYPE,
        "ClientToken": "8f9a1b2c-3d4e-4f50-9a1b-2c3d4e5f6071",
    });
    let (status, first) = call(&router, "DeleteInventory", &request).await;
    assert_eq!(
        status, 200,
        "the first delete must be served, got {status} with {first}"
    );
    let (status, again) = call(&router, "DeleteInventory", &request).await;
    assert_eq!(
        status, 200,
        "the repeat must be served, got {status} with {again}"
    );
    assert_eq!(
        again["DeletionId"], first["DeletionId"],
        "the repeat is the same deletion, got {again}"
    );
    assert_eq!(
        again["DeletionSummary"], first["DeletionSummary"],
        "the repeat reports the summary of that deletion, got {again}"
    );

    let (status, described) = call(&router, "DescribeInventoryDeletions", &json!({})).await;
    assert_eq!(
        status, 200,
        "the deletions must be reported, got {status} with {described}"
    );
    assert_eq!(
        described["InventoryDeletions"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        1,
        "one deletion was performed, so one is recorded, got {described}"
    );
}

/// RULE: `ListInventoryEntries` narrows the entries by the filters it carries and refuses a
/// filter key that names no attribute of the type whose schema this service publishes.
#[tokio::test]
async fn a_read_narrows_the_entries_by_its_filters() {
    let (router, engine) = service();
    register(&engine, crate::parameter_store::REGION, NODE);
    let written = json!([
        { "RackLocation": "Bay B/Row 4", "Units": "4" },
        { "RackLocation": "Bay C/Row 1", "Units": "12" },
    ]);
    let (status, body) = put(
        &router,
        crate::parameter_store::REGION,
        NODE,
        json!([item(CUSTOM_TYPE, written)]),
    )
    .await;
    assert_eq!(
        status, 200,
        "the put must be served, got {status} with {body}"
    );

    // (case, the filter, the entries it must select)
    let rows: [(&str, Value, Value); 5] = [
        (
            "an equality on one attribute",
            json!([{ "Key": "RackLocation", "Values": ["Bay B/Row 4"] }]),
            json!([{ "RackLocation": "Bay B/Row 4", "Units": "4" }]),
        ),
        (
            "a prefix on the same attribute",
            json!([{ "Key": "RackLocation", "Values": ["Bay C"], "Type": "BeginWith" }]),
            json!([{ "RackLocation": "Bay C/Row 1", "Units": "12" }]),
        ),
        (
            "an existence test",
            json!([{ "Key": "Units", "Values": ["anything"], "Type": "Exists" }]),
            json!([
                { "RackLocation": "Bay B/Row 4", "Units": "4" },
                { "RackLocation": "Bay C/Row 1", "Units": "12" },
            ]),
        ),
        (
            "a value no entry carries",
            json!([{ "Key": "RackLocation", "Values": ["Bay Z/Row 9"] }]),
            json!([]),
        ),
        (
            "an attribute no entry of this type carries",
            json!([{ "Key": "NobodyWroteThis", "Values": ["x"] }]),
            json!([]),
        ),
    ];

    for (case, filters, expected) in rows {
        let (status, read) = call(
            &router,
            "ListInventoryEntries",
            &json!({
                "InstanceId": NODE,
                "TypeName": CUSTOM_TYPE,
                "Filters": filters,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must be served, got {status} with {read}"
        );
        assert_eq!(
            read["Entries"], expected,
            "case {case:?}: expected {expected}, got {read}"
        );
    }

    // A filter on the published type has to name one of that schema's attributes.
    let (status, body) = call(
        &router,
        "ListInventoryEntries",
        &json!({
            "InstanceId": NODE,
            "TypeName": INSTANCE_INFORMATION,
            "Filters": [{ "Key": "NotAnAttribute", "Values": ["x"] }],
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a key outside the published schema must be refused, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidFilter",
        "expected InvalidFilter, got {body}"
    );
}

/// RULE: the node's own `AWS:InstanceInformation` item is the projection of the registered
/// node, so a read of it reports the node's properties for a node the Region holds and
/// nothing for one it does not — without any put.
#[tokio::test]
async fn the_nodes_own_item_is_the_projection_of_its_registration() {
    let (router, engine) = service();
    register(&engine, crate::parameter_store::REGION, NODE);

    let (status, read) = entries(&router, NODE, INSTANCE_INFORMATION).await;
    assert_eq!(
        status, 200,
        "the read must be served, got {status} with {read}"
    );
    assert_eq!(
        read["Entries"],
        json!([{
            "InstanceId": NODE,
            "InstanceStatus": "Active",
            "PlatformType": "Linux",
            "ResourceType": RESOURCE_TYPE_EC2_INSTANCE,
        }]),
        "the item carries every attribute the node holds a value for, got {read}"
    );
    assert_eq!(
        read["SchemaVersion"].as_str(),
        Some(VERSION),
        "the published schema version, got {read}"
    );

    let (status, read) = entries(&router, UNREGISTERED, INSTANCE_INFORMATION).await;
    assert_eq!(
        status, 200,
        "the read must be served, got {status} with {read}"
    );
    assert_eq!(
        read["Entries"],
        json!([]),
        "a node this Region holds none of reports no item, got {read}"
    );
}
