// SPDX-License-Identifier: CC0-1.0

//! The Systems Manager inventory TYPE surface, end to end: which inventory types a schema
//! read discovers, how a type name is judged, and what a read of one answers once a write has
//! defined it or a delete has taken it away.
//!
//! RULE under test: **`GetInventorySchema` "return[s] a list of inventory type names for the
//! account, or … a list of attribute names for a specific Inventory item type"
//! (`smithy_query(query="operation", operation="GetInventorySchema")`), so it reports one
//! `InventoryItemSchema` per inventory type Systems Manager publishes — each with that type's
//! published attribute list and schema version, every attribute's `DataType` in the uppercase
//! form the service puts on the wire — whether or not anything has been collected for the
//! type, plus the caller's own `Custom:` types the Region holds items of. A TypeName names an
//! inventory type only if it is one of those published names spelled exactly or a `Custom:`
//! name matching `^(Custom:)([a-zA-Z0-9]{3,93})$`; anything else is
//! `InvalidTypeNameException` (HTTP 400) in the sentence stating those rules, wherever a type
//! name is taken. A well-formed `Custom:` name stands for a type only while a write has
//! defined it, so a read of one nothing stands under is refused too — in the sentence for
//! THAT condition — and so is a delete of it, while a deletion LISTING answers the empty list
//! it matches rather than a refusal.**
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the router. The ONE thing seeded through the engine is a registered managed node,
//! because no call of this service registers one; every custom item is written with
//! `PutInventory`.
//!
//! The rows differ along what the rule generalises over: which published type a read names
//! and how many attributes it has, which rule a rejected name breaks (case, namespace,
//! custom-name length and character set) and at each published bound, which operation takes
//! the name, whether a type is defined, data-deleted or schema-deleted, and whether a
//! deletion listing names an id, names an unissued one, or names none.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    custom_inventory, ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router_with_engine};

/// The node the custom items are written about, of the 17-character EC2 form the modelled
/// `InstanceId` pattern admits (`^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$`).
const NODE: &str = "i-0123456789abcdef0";

/// The instant the seeded node last reported, as a whole number of seconds.
const LAST_PING: Duration = Duration::from_secs(1_700_000_000);

/// A capture time in the form the modelled `InventoryItemCaptureTime` spells.
const CAPTURED: &str = "2024-03-04T05:06:07Z";

/// The schema version a put writes.
const VERSION: &str = "2.0";

/// The custom type the writes here use, well formed under the custom-name pattern.
const CUSTOM_TYPE: &str = "Custom:RackInfo";

/// The inventory types Systems Manager publishes, in the order a schema read reports them
/// (ascending type name). Written out here independently of the service, from AWS's published
/// inventory catalog — the five the model's own doc names
/// (`smithy_query(query="doc", shape="InventoryItemSchema", member="TypeName")`: "Default
/// inventory item types include the following: AWS:AWSComponent, AWS:Application,
/// AWS:InstanceInformation, AWS:Network, and AWS:WindowsUpdate") and the rest of the catalog
/// the AWS Systems Manager User Guide tabulates under "Metadata collected by Systems Manager
/// Inventory".
const PUBLISHED_TYPES: [&str; 15] = [
    "AWS:AWSComponent",
    "AWS:Application",
    "AWS:ComplianceItem",
    "AWS:ComplianceSummary",
    "AWS:File",
    "AWS:InstanceDetailedInformation",
    "AWS:InstanceInformation",
    "AWS:Network",
    "AWS:PatchSummary",
    "AWS:ResourceGroup",
    "AWS:Service",
    "AWS:Tag",
    "AWS:WindowsRegistry",
    "AWS:WindowsRole",
    "AWS:WindowsUpdate",
];

/// The data type value the service puts on the wire for every attribute of a PUBLISHED type,
/// in the case it serializes it in — the modelled `InventoryAttributeDataType` spells its two
/// values in lower case and the recorded catalog answers the uppercase token for every
/// attribute of every published type, the ones whose values read as numbers included.
const DATA_TYPE_STRING: &str = "STRING";

/// The same token for an attribute every value reported under which is a number, which is the
/// case a CUSTOM type's schema can report, its attributes being whatever its items say.
const DATA_TYPE_NUMBER: &str = "NUMBER";

/// The sentence a type name breaking the naming rules is refused with, written out here
/// independently of the service: fixed text apart from the bracketed name, which is the value
/// the request carried. Note the two spaces before "Custom inventory type name".
fn invalid_type_name(type_name: &str) -> String {
    format!(
        "Inventory type name [{type_name}] is invalid. Type name is case sensitive, such as \
         AWS:APpLICation is invalid; Type name must start with either AWS: or Custom:;  Custom \
         inventory type name must satisfy regular expression pattern: \
         ^(Custom:)([a-zA-Z0-9]{{3,93}})$"
    )
}

/// The sentence a well-formed custom type name nothing stands under is refused with when a
/// DELETE names it, written out independently of the service.
fn deleted_or_missing(type_name: &str) -> String {
    format!("The given TypeName {type_name} is either deleted or doesn't exist")
}

/// The sentence a read of an undefined custom type is refused with, written out
/// independently of the service.
fn not_defined(type_name: &str) -> String {
    format!("Inventory type name {type_name} is not defined.")
}

/// A router with just this service mounted, and the engine behind it.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    router_with_engine()
}

/// Register a node the way its own registration would, which is what makes a put about it a
/// put about a node this account holds.
fn register(engine: &Arc<engine::InMemoryEngine>, node_id: &str) {
    let at = SystemTime::UNIX_EPOCH + LAST_PING;
    let mut node = ManagedNodeRecord::registering(node_id, RESOURCE_TYPE_EC2_INSTANCE, at);
    node.properties.platform_type = Some("Linux".to_string());
    node.properties.instance_state = Some("Active".to_string());
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(crate::parameter_store::REGION, node),
        "a seeded node must carry the node id it is stored under"
    );
}

/// `PutInventory` of one item of `type_name` carrying `entries`, which must be served.
async fn put(router: &MultiServiceRouter, case: &str, type_name: &str, entries: Value) {
    let (status, body) = call(
        router,
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
        "case {case:?}: the put must be served, got {status} with {body}"
    );
}

/// The body of a `GetInventorySchema` that must be served.
async fn schemas(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, reported) = call(router, "GetInventorySchema", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetInventorySchema {body} must be served, got {status} with {reported}"
    );
    reported
}

/// The type names a schema read reports, in the order it reports them.
fn schema_types(case: &str, reported: &Value) -> Vec<String> {
    reported["Schemas"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a schema read reports Schemas, got {reported}"))
        .iter()
        .map(|schema| schema["TypeName"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The `(name, data type)` pairs one reported schema's attributes carry, in order.
fn attributes(case: &str, schema: &Value) -> Vec<(String, String)> {
    schema["Attributes"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a schema carries Attributes, got {schema}"))
        .iter()
        .map(|attribute| {
            (
                attribute["Name"].as_str().unwrap_or_default().to_string(),
                attribute["DataType"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// `ListInventoryEntries` of `type_name` about the seeded node.
async fn entries(router: &MultiServiceRouter, type_name: &str) -> (u16, Value) {
    call(
        router,
        "ListInventoryEntries",
        &json!({ "InstanceId": NODE, "TypeName": type_name }),
    )
    .await
}

/// RULE: a schema read with nothing narrowing it reports the whole published catalog — one
/// entry per published type, in ascending type-name order, each carrying a schema version in
/// the form `InventoryItemSchemaVersion` spells and between 1 and 50 attributes
/// (`smithy_query(query="shape", shape="InventoryItemSchema")`: `Attributes` is `@required`,
/// length 1..50), every one of them with the uppercase wire data type. The catalog is what an
/// account MAY query, so it is reported for a Region that holds no node and no item at all.
#[tokio::test]
async fn a_schema_read_reports_every_published_inventory_type() {
    let (router, _engine) = service();

    let reported = schemas(&router, "the whole catalog", &json!({})).await;
    let names = schema_types("the whole catalog", &reported);
    assert_eq!(
        names,
        PUBLISHED_TYPES.map(String::from).to_vec(),
        "case the whole catalog: expected one entry per published type in type-name order, \
         got {names:?}"
    );

    for (index, type_name) in PUBLISHED_TYPES.iter().enumerate() {
        let case = *type_name;
        let schema = &reported["Schemas"][index];
        let version = schema["Version"].as_str().unwrap_or_default();
        let (major, minor) = version
            .split_once('.')
            .unwrap_or_else(|| panic!("case {case:?}: a schema carries a Version, got {schema}"));
        assert!(
            (1..=6).contains(&major.len())
                && (1..=6).contains(&minor.len())
                && major.chars().all(|digit| digit.is_ascii_digit())
                && minor.chars().all(|digit| digit.is_ascii_digit()),
            "case {case:?}: the schema version is one the modelled pattern admits, got \
             {version:?}"
        );
        let attributes = attributes(case, schema);
        assert!(
            (1..=50).contains(&attributes.len()),
            "case {case:?}: a published schema carries 1..50 attributes, got {} in {schema}",
            attributes.len()
        );
        for (name, data_type) in &attributes {
            // The recorded catalog answers the text data type for every attribute of every
            // published type, the count-bearing and version-bearing ones included.
            assert!(
                !name.is_empty() && data_type == DATA_TYPE_STRING,
                "case {case:?}: every published attribute is a named one of the uppercase wire \
                 data type, got ({name:?}, {data_type:?})"
            );
        }
    }
}

/// RULE: `TypeName` "the type of inventory item to return" narrows the read to the one type
/// it names, and a published name is served that type's own published attribute list and
/// version. The rows are published types the issue's own reproduction did not cite beside the
/// ones it did, so the attribute list under test is read from the catalog rather than from one
/// recorded entry.
#[tokio::test]
async fn a_narrowed_schema_read_projects_the_published_type_it_names() {
    /// One row: the type a read names, the version it must report, and the attributes it must
    /// carry, in order. Each list is that type's published attribute set.
    struct Row {
        type_name: &'static str,
        version: &'static str,
        attributes: &'static [&'static str],
    }

    let rows = [
        // The two entries the issue cites, as the published catalog states them.
        Row {
            type_name: "AWS:AWSComponent",
            version: "1.0",
            attributes: &[
                "Name",
                "ApplicationType",
                "Publisher",
                "Version",
                "InstalledTime",
                "Architecture",
                "URL",
            ],
        },
        Row {
            type_name: "AWS:InstanceInformation",
            version: "1.1",
            attributes: &[
                "AgentType",
                "AgentVersion",
                "ComputerName",
                "IamRole",
                "InstanceId",
                "IpAddress",
                "PlatformName",
                "PlatformType",
                "PlatformVersion",
                "ResourceType",
                "AgentStatus",
                "InstanceStatus",
                "SourceType",
                "SourceId",
                "SourceLocation",
                "AvailabilityZone",
                "AvailabilityZoneId",
            ],
        },
        // Entries the issue did not cite: the smallest published schemas, and one the
        // compliance surface is discovered through.
        Row {
            type_name: "AWS:Tag",
            version: "1.0",
            attributes: &["Key", "Value"],
        },
        Row {
            type_name: "AWS:WindowsRegistry",
            version: "1.0",
            attributes: &["KeyPath", "ValueName", "ValueType", "Value"],
        },
        Row {
            type_name: "AWS:WindowsUpdate",
            version: "1.0",
            attributes: &["HotFixId", "Description", "InstalledTime", "InstalledBy"],
        },
    ];

    let (router, _engine) = service();
    for row in rows {
        let case = row.type_name;
        let reported = schemas(&router, case, &json!({ "TypeName": case })).await;
        assert_eq!(
            schema_types(case, &reported),
            vec![case.to_string()],
            "case {case:?}: a narrowed read reports the one type it names, got {reported}"
        );
        let expected: Vec<(String, String)> = row
            .attributes
            .iter()
            .map(|name| ((*name).to_string(), DATA_TYPE_STRING.to_string()))
            .collect();
        assert_eq!(
            attributes(case, &reported["Schemas"][0]),
            expected,
            "case {case:?}: expected that type's published attributes, got {reported}"
        );
        assert_eq!(
            reported["Schemas"][0]["Version"].as_str(),
            Some(row.version),
            "case {case:?}: expected the published schema version, got {reported}"
        );
    }

    // A compliance client discovers the attributes it may report under through this read, so
    // the type is served rather than refused, and it carries the compliance attributes.
    let case = "AWS:ComplianceItem";
    let reported = schemas(&router, case, &json!({ "TypeName": case })).await;
    let names: Vec<String> = attributes(case, &reported["Schemas"][0])
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for attribute in ["ComplianceType", "Id", "Title", "Status", "Severity"] {
        assert!(
            names.contains(&attribute.to_string()),
            "case {case:?}: the published compliance schema names {attribute}, got {names:?}"
        );
    }
}

/// RULE: a custom type's schema is what its items say — the attributes their content carries,
/// each with the data type of the values reported under it, in the same uppercase wire form a
/// published attribute carries — and it is reported beside the published catalog.
#[tokio::test]
async fn a_custom_types_schema_is_what_its_items_say() {
    let (router, engine) = service();
    register(&engine, NODE);
    put(
        &router,
        "a custom type of one text and one numeric attribute",
        CUSTOM_TYPE,
        json!([{ "RackLocation": "Bay B/Row 4", "Units": "42" }]),
    )
    .await;

    let case = "the catalog with one custom type";
    let reported = schemas(&router, case, &json!({})).await;
    let mut expected = PUBLISHED_TYPES.map(String::from).to_vec();
    expected.push(CUSTOM_TYPE.to_string());
    assert_eq!(
        schema_types(case, &reported),
        expected,
        "case {case:?}: the custom type is reported after the published ones, got {reported}"
    );

    let case = CUSTOM_TYPE;
    let reported = schemas(&router, case, &json!({ "TypeName": CUSTOM_TYPE })).await;
    assert_eq!(
        attributes(case, &reported["Schemas"][0]),
        vec![
            ("RackLocation".to_string(), DATA_TYPE_STRING.to_string()),
            ("Units".to_string(), DATA_TYPE_NUMBER.to_string()),
        ],
        "case {case:?}: a custom schema's attributes are what its items say, in the wire data \
         types, got {reported}"
    );
    assert_eq!(
        reported["Schemas"][0]["Version"].as_str(),
        Some(VERSION),
        "case {case:?}: the version is the one the put wrote, got {reported}"
    );
}

/// RULE: a type name names an inventory type only if it is a published name spelled exactly
/// or a `Custom:` name matching `^(Custom:)([a-zA-Z0-9]{3,93})$`. Every operation that takes
/// one refuses anything else with HTTP 400 `InvalidTypeNameException` and the sentence
/// stating those rules, with the submitted name in it.
#[tokio::test]
async fn a_type_name_outside_the_naming_rule_is_refused_wherever_it_is_taken() {
    // One row per rule a name can break: the case, and the name the request carries. The
    // custom-name pattern bounds the name at 3..93 characters, so one row sits one character
    // short of the lower bound; one past the UPPER bound is a separate case below, because a
    // 94-character custom name is also past the type-name member's own modelled length.
    let short_custom = format!("Custom:{}", "a".repeat(2));
    let refused: [(&str, &str); 7] = [
        ("a published name in the wrong case", "AWS:application"),
        ("a published name misspelled", "AWS:Applicaton"),
        ("an AWS: name of no published type", "AWS:NotAType"),
        ("a custom name carrying hyphens", "Custom:rack-info-7"),
        ("a custom name carrying an underscore", "Custom:rack_info"),
        ("a custom name carrying a space", "Custom:rack info"),
        (
            "a custom name one short of the lower bound",
            short_custom.as_str(),
        ),
    ];

    let (router, engine) = service();
    register(&engine, NODE);

    for (case, type_name) in refused {
        // The three operations that take a type name and resolve it: the schema read's
        // filter, the entry read's required member, and the delete's.
        let requests = [
            ("GetInventorySchema", json!({ "TypeName": type_name })),
            (
                "ListInventoryEntries",
                json!({ "InstanceId": NODE, "TypeName": type_name }),
            ),
            ("DeleteInventory", json!({ "TypeName": type_name })),
        ];
        for (operation, body) in requests {
            let (status, reported) = call(&router, operation, &body).await;
            assert_eq!(
                (status, error_code(&reported).as_str()),
                (400, "InvalidTypeNameException"),
                "case {case:?} at {operation}: expected 400 InvalidTypeNameException, got \
                 {status} with {reported}"
            );
            assert_eq!(
                error_message(&reported),
                invalid_type_name(type_name),
                "case {case:?} at {operation}: expected the sentence stating the naming rules, \
                 got {:?}",
                error_message(&reported)
            );
        }
    }

    // The names the same rule ADMITS, including the bounds of the custom-name pattern: a
    // schema read of them is served rather than refused.
    let long_custom = format!("Custom:{}", "a".repeat(93));
    let admitted: [(&str, &str); 4] = [
        ("a published name", "AWS:WindowsUpdate"),
        ("a custom name at the lower bound", "Custom:abc"),
        ("a custom name at the upper bound", long_custom.as_str()),
        ("a custom name mixing letters and digits", "Custom:Rack7Info"),
    ];
    for (case, type_name) in admitted {
        let (status, reported) = call(
            &router,
            "GetInventorySchema",
            &json!({ "TypeName": type_name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?} ({type_name}): a name the rule admits is served, got {status} with \
             {reported}"
        );
    }

    // One character past the upper bound: 94 characters after `Custom:` is past the pattern
    // AND past the 0..100 length the modelled `InventoryItemTypeNameFilter` carries, so the
    // request is refused either way rather than served.
    let past_bound = format!("Custom:{}", "a".repeat(94));
    let (status, reported) = call(
        &router,
        "GetInventorySchema",
        &json!({ "TypeName": past_bound }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case a custom name one past the upper bound: expected a refusal, got {status} with \
         {reported}"
    );
}

/// RULE: a `Custom:` name stands for a type only while a write has DEFINED it: a read of one
/// nothing stands under is `InvalidTypeNameException` saying so, a write defines it, deleting
/// the DATA leaves it defined and holding nothing, and `DeleteSchema` — which "deletes the
/// specified custom type from the Inventory service" — leaves the name standing for nothing
/// again until a later write defines it.
#[tokio::test]
async fn a_custom_type_is_read_only_while_a_write_has_defined_it() {
    let (router, engine) = service();
    register(&engine, NODE);

    let case = "before any write";
    let (status, reported) = entries(&router, CUSTOM_TYPE).await;
    assert_eq!(
        (status, error_code(&reported).as_str()),
        (400, "InvalidTypeNameException"),
        "case {case:?}: a read of an undefined custom type is refused, got {status} with \
         {reported}"
    );
    assert_eq!(
        error_message(&reported),
        not_defined(CUSTOM_TYPE),
        "case {case:?}: expected the undefined-type sentence, got {:?}",
        error_message(&reported)
    );

    put(
        &router,
        "the write that defines the type",
        CUSTOM_TYPE,
        json!([{ "RackLocation": "Bay B/Row 4" }]),
    )
    .await;
    let case = "after the write";
    let (status, reported) = entries(&router, CUSTOM_TYPE).await;
    assert_eq!(status, 200, "case {case:?}: the read is served, got {reported}");
    assert_eq!(
        reported["Entries"],
        json!([{ "RackLocation": "Bay B/Row 4" }]),
        "case {case:?}: the read reports what the write wrote, got {reported}"
    );

    // A delete of the DATA alone leaves the type defined, so the read still answers — with
    // the nothing the type now holds — and a later delete of its SCHEMA is served too,
    // counting the nothing it finds.
    let case = "after a delete of the data alone";
    let (status, deleted) = call(&router, "DeleteInventory", &json!({ "TypeName": CUSTOM_TYPE })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the delete must be served, got {status} with {deleted}"
    );
    let (status, reported) = entries(&router, CUSTOM_TYPE).await;
    assert_eq!(status, 200, "case {case:?}: the read is served, got {reported}");
    assert_eq!(
        reported["Entries"],
        json!([]),
        "case {case:?}: the type holds nothing and is still defined, got {reported}"
    );

    let case = "a schema delete of a defined type holding nothing";
    let (status, deleted) = call(
        &router,
        "DeleteInventory",
        &json!({ "TypeName": CUSTOM_TYPE, "SchemaDeleteOption": "DeleteSchema" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a type the account has defined can have its schema deleted, got \
         {status} with {deleted}"
    );
    assert_eq!(
        deleted["DeletionSummary"]["TotalCount"],
        json!(0),
        "case {case:?}: the summary counts the items it found, got {deleted}"
    );
    let (status, reported) = entries(&router, CUSTOM_TYPE).await;
    assert_eq!(
        (status, error_code(&reported).as_str()),
        (400, "InvalidTypeNameException"),
        "case {case:?}: the type is gone, so the read is refused, got {status} with {reported}"
    );

    // A delete of the SCHEMA takes the type away, so the read is refused as it was before
    // anything defined it.
    put(
        &router,
        "the write before the schema delete",
        CUSTOM_TYPE,
        json!([{ "RackLocation": "Bay B/Row 4" }]),
    )
    .await;
    let case = "after a delete of the schema";
    let (status, deleted) = call(
        &router,
        "DeleteInventory",
        &json!({ "TypeName": CUSTOM_TYPE, "SchemaDeleteOption": "DeleteSchema" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the delete must be served, got {status} with {deleted}"
    );
    let (status, reported) = entries(&router, CUSTOM_TYPE).await;
    assert_eq!(
        (status, error_code(&reported).as_str()),
        (400, "InvalidTypeNameException"),
        "case {case:?}: the type is gone, so the read is refused, got {status} with {reported}"
    );
    assert_eq!(
        error_message(&reported),
        not_defined(CUSTOM_TYPE),
        "case {case:?}: expected the undefined-type sentence, got {:?}",
        error_message(&reported)
    );

    // "You can recreate the schema later, if you want": a later write defines it again.
    put(
        &router,
        "the write that defines the type again",
        CUSTOM_TYPE,
        json!([{ "RackLocation": "Bay D/Row 2" }]),
    )
    .await;
    let case = "after the type is defined again";
    let (status, reported) = entries(&router, CUSTOM_TYPE).await;
    assert_eq!(status, 200, "case {case:?}: the read is served, got {reported}");
    assert_eq!(
        reported["Entries"],
        json!([{ "RackLocation": "Bay D/Row 2" }]),
        "case {case:?}: the read reports the item the later write wrote, got {reported}"
    );

    // A published type is defined for every account, so a read of one nothing has collected
    // reports the nothing it holds rather than a refusal.
    for type_name in ["AWS:Network", "AWS:WindowsUpdate"] {
        let (status, reported) = entries(&router, type_name).await;
        assert_eq!(
            status, 200,
            "case {type_name:?}: a published type is read whether or not anything collected an \
             item of it, got {status} with {reported}"
        );
        assert_eq!(
            reported["Entries"],
            json!([]),
            "case {type_name:?}: nothing has been collected, so nothing is reported, got \
             {reported}"
        );
    }
}

/// RULE: a delete names a type it can address. A well-formed custom name nothing stands under
/// — one no write ever defined, and one a schema delete took away — is HTTP 400
/// `InvalidTypeNameException` naming both possibilities, which is a different sentence from
/// the one a malformed name gets.
#[tokio::test]
async fn a_delete_of_a_type_nothing_stands_under_names_both_possibilities() {
    let (router, engine) = service();
    register(&engine, NODE);

    let rows: [(&str, &str); 2] = [
        ("a custom name nothing ever defined", "Custom:NeverRegistered"),
        ("a second one, of another spelling", "Custom:AlsoNeverThere99"),
    ];
    for (case, type_name) in rows {
        let (status, reported) = call(&router, "DeleteInventory", &json!({ "TypeName": type_name })).await;
        assert_eq!(
            (status, error_code(&reported).as_str()),
            (400, "InvalidTypeNameException"),
            "case {case:?}: expected 400 InvalidTypeNameException, got {status} with {reported}"
        );
        assert_eq!(
            error_message(&reported),
            deleted_or_missing(type_name),
            "case {case:?}: expected the deleted-or-missing sentence, got {:?}",
            error_message(&reported)
        );
    }

    // The same sentence for a type a delete already took away: the two are the same
    // condition, which is why it names both.
    put(
        &router,
        "the write before the delete",
        CUSTOM_TYPE,
        json!([{ "RackLocation": "Bay B/Row 4" }]),
    )
    .await;
    let (status, deleted) = call(
        &router,
        "DeleteInventory",
        &json!({ "TypeName": CUSTOM_TYPE, "SchemaDeleteOption": "DeleteSchema" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the first delete must be served, got {status} with {deleted}"
    );
    let case = "a custom name a delete already took away";
    let (status, reported) = call(&router, "DeleteInventory", &json!({ "TypeName": CUSTOM_TYPE })).await;
    assert_eq!(
        (status, error_code(&reported).as_str()),
        (400, "InvalidTypeNameException"),
        "case {case:?}: expected 400 InvalidTypeNameException, got {status} with {reported}"
    );
    assert_eq!(
        error_message(&reported),
        deleted_or_missing(CUSTOM_TYPE),
        "case {case:?}: expected the deleted-or-missing sentence, got {:?}",
        error_message(&reported)
    );
}

/// RULE: `DescribeInventoryDeletions` is a LISTING over the deletions this Region recorded, so
/// what it answers is the deletions matching the request — an account with no deletion history
/// and a well-formed `DeletionId` no delete handed out both match NOTHING, which is HTTP 200
/// with an empty list rather than a refusal, while an id a delete did hand out reports that
/// deletion.
#[tokio::test]
async fn a_deletion_listing_projects_the_deletions_it_matches() {
    /// A `DeletionId` of the form the modelled `UUID` pattern admits that no delete of this
    /// endpoint handed out.
    const UNISSUED: &str = "0ad1e69c-b8c1-4b4b-9e1b-0c5f5d0a5f20";

    let (router, engine) = service();
    register(&engine, NODE);

    // One row per request shape, against a store holding no deletion at all.
    let rows: [(&str, Value); 3] = [
        ("a listing naming nothing", json!({})),
        (
            "a listing naming an id nothing was recorded under",
            json!({ "DeletionId": UNISSUED }),
        ),
        (
            "a listing naming that id and a page bound",
            json!({ "DeletionId": UNISSUED, "MaxResults": 1 }),
        ),
    ];
    for (case, body) in rows {
        let (status, reported) = call(&router, "DescribeInventoryDeletions", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a listing is a successful read, got {status} with {reported}"
        );
        assert_eq!(
            reported["InventoryDeletions"],
            json!([]),
            "case {case:?}: expected an empty deletion list, got {reported}"
        );
    }

    // A deletion the endpoint DID record is reported, under the id it minted and in the
    // unnarrowed listing; the unissued id still matches nothing.
    put(
        &router,
        "the write the delete removes",
        CUSTOM_TYPE,
        json!([{ "RackLocation": "Bay B/Row 4" }]),
    )
    .await;
    let (status, deleted) = call(&router, "DeleteInventory", &json!({ "TypeName": CUSTOM_TYPE })).await;
    assert_eq!(
        status, 200,
        "the delete must be served, got {status} with {deleted}"
    );
    let deletion_id = deleted["DeletionId"]
        .as_str()
        .unwrap_or_else(|| panic!("a delete mints a DeletionId, got {deleted}"))
        .to_string();

    let case = "a listing naming the id the delete minted";
    let (status, reported) = call(
        &router,
        "DescribeInventoryDeletions",
        &json!({ "DeletionId": deletion_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the deletion must be reported, got {status} with {reported}"
    );
    assert_eq!(
        reported["InventoryDeletions"][0]["DeletionId"].as_str(),
        Some(deletion_id.as_str()),
        "case {case:?}: the deletion reported is the one the id names, got {reported}"
    );

    let case = "the unissued id, once a deletion exists";
    let (status, reported) = call(
        &router,
        "DescribeInventoryDeletions",
        &json!({ "DeletionId": UNISSUED }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a listing is a successful read, got {status} with {reported}"
    );
    assert_eq!(
        reported["InventoryDeletions"],
        json!([]),
        "case {case:?}: an id nothing was recorded under matches nothing, got {reported}"
    );
}

/// RULE: the custom type DEFINITIONS this endpoint holds are bounded across every Region. A
/// definition outlives the items written under it and the Region a call is signed for is the
/// caller's to choose, so neither the item ceiling nor the per-Region type ceiling bounds the
/// set a write grows: the put that would define one past the emulator's own ceiling is refused
/// `CustomSchemaCountLimitExceededException` and writes nothing.
#[tokio::test]
async fn the_definitions_this_endpoint_holds_are_bounded_across_every_region() {
    let (router, engine) = service();
    register(&engine, NODE);

    // Fill the store to its ceiling the way a write-then-delete-the-data loop across Regions
    // would: each write defines a type, and the definition stays when the item under it goes.
    // Seeded through the engine rather than over as many calls, because what is under test is
    // the ceiling the next PUT sees, not the loop's own plumbing.
    {
        let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
        let mut store = state.lock();
        for index in 0..custom_inventory::MAX_CUSTOM_INVENTORY_TYPE_DEFINITIONS {
            let region = format!("filler-region-{index}");
            let type_name = format!("Custom:Filler{index}");
            store.insert_custom_inventory(custom_inventory::CustomInventoryItemRecord {
                region: region.clone(),
                node_id: NODE.to_string(),
                type_name: type_name.clone(),
                schema_version: VERSION.to_string(),
                capture_time: CAPTURED.to_string(),
                content_hash: None,
                content: vec![HashMap::from([(
                    "Filler".to_string(),
                    "1".to_string(),
                )])],
                written_at: SystemTime::now(),
            });
            store.remove_custom_inventory_of_type(&region, &type_name);
        }
        assert_eq!(
            store.custom_inventory_types.len(),
            custom_inventory::MAX_CUSTOM_INVENTORY_TYPE_DEFINITIONS,
            "the seeded store holds one definition per filler type, got {}",
            store.custom_inventory_types.len()
        );
        assert!(
            store.custom_inventory.is_empty(),
            "the fillers left no item behind, so the item ceiling is not what refuses the put, \
             got {} item(s)",
            store.custom_inventory.len()
        );
    }

    let case = "a put that would define one type past the ceiling";
    let (status, body) = call(
        &router,
        "PutInventory",
        &json!({
            "InstanceId": NODE,
            "Items": [{
                "TypeName": CUSTOM_TYPE,
                "SchemaVersion": VERSION,
                "CaptureTime": CAPTURED,
                "Content": [{ "RackLocation": "Bay B/Row 4" }],
            }],
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "CustomSchemaCountLimitExceededException"),
        "case {case:?}: expected the schema-count refusal, got {status} with {body}"
    );
    assert_eq!(
        error_message(&body),
        format!(
            "This emulator holds at most {} custom inventory type definitions across every \
             Region",
            custom_inventory::MAX_CUSTOM_INVENTORY_TYPE_DEFINITIONS
        ),
        "case {case:?}: the refusal names the emulator's own ceiling, got {:?}",
        error_message(&body)
    );

    // The refused put wrote nothing: the type it named stands for nothing.
    let (status, reported) = entries(&router, CUSTOM_TYPE).await;
    assert_eq!(
        (status, error_code(&reported).as_str()),
        (400, "InvalidTypeNameException"),
        "case {case:?}: the refused put defined nothing, got {status} with {reported}"
    );
}
