// SPDX-License-Identifier: CC0-1.0

//! What a Systems Manager INVENTORY SCHEMA read reports, over the whole mounted service set —
//! the one `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **`GetInventorySchema` "return[s] a list of inventory type names for
//! the account, or … a list of attribute names for a specific Inventory item type"
//! (`smithy_query(query="operation", operation="GetInventorySchema")`), so the schemas it
//! reports are the inventory types Systems Manager publishes for every account — reported
//! whether or not anything has been collected for them, because this is the discovery call —
//! plus every custom type the Region the call was signed for holds items of, whose attributes,
//! data types and version are what those items say. The request narrows that set and nothing
//! else: `TypeName` selects the one type it names (a name of no inventory namespace is
//! `InvalidTypeNameException`, and a well-formed custom name this endpoint holds no schema of
//! reports nothing), `Aggregator` reports only the attributes aggregation counts over,
//! `SubType` reports the nothing this endpoint holds of sub-type schemas, `MaxResults` bounds
//! the page inside its modelled range and is refused outside it, and `NextToken` resumes at
//! the type the page it came from stopped at — a token naming no schema of this read being
//! `InvalidNextToken`.**
//!
//! Every case goes through the front door a client uses: `PutInventory` writes the custom
//! items and `GetInventorySchema` reads the schemas back, so a schema reported here is one the
//! caller's own write put there. The ONE thing seeded through the engine is a registered
//! managed node, because no call of this service registers one (the agent's own call is not in
//! this model).
//!
//! The rows differ along what the rule generalises over: which type a read names and which
//! namespace that name belongs to, how many types the Region holds, the attributes and value
//! types the items carry, the schema version the put wrote, the Region the call is signed for,
//! both narrowing flags in every combination, each published `MaxResults` bound and one past
//! it, and where a token resumes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ssm_amazonssm as ssm;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::authorization;

/// Systems Manager's `X-Amz-Target` prefix and its signing name.
const TARGET_PREFIX: &str = "AmazonSSM";
const SIGNING_NAME: &str = "ssm";

/// The Region most calls are signed for.
const REGION: &str = "us-west-2";

/// A second Region, which holds a custom type of its own.
const OTHER_REGION: &str = "us-east-1";

/// A third Region, which holds no node and no item at all.
const EMPTY_REGION: &str = "eu-west-1";

/// The node every put writes about, of the 17-character EC2 form the modelled `InstanceId`
/// pattern admits (`^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$`).
const NODE: &str = "i-0123456789abcdef0";

/// The instant a seeded node last reported, as a whole number of seconds.
const LAST_PING: Duration = Duration::from_secs(1_700_000_000);

/// A capture time in the form the modelled `InventoryItemCaptureTime` spells.
const CAPTURED: &str = "2024-03-04T05:06:07Z";

/// The one type Systems Manager publishes whose item this endpoint derives from the node
/// record.
const PUBLISHED_TYPE: &str = "AWS:InstanceInformation";

/// The attributes `GetInventorySchemaRequest$Aggregator` documents that type as supporting
/// aggregation on: "this call returns the AWS:InstanceInformation type, because it supports
/// aggregation based on the PlatformName, PlatformType, and PlatformVersion attributes"
/// (`smithy_query(query="doc", shape="GetInventorySchemaRequest", member="Aggregator")`).
const AGGREGATABLE_ATTRIBUTES: [&str; 3] = ["PlatformName", "PlatformType", "PlatformVersion"];

/// The inventory types Systems Manager PUBLISHES, in the order a schema read reports them
/// (ascending type name). Written out here independently of the service, from AWS's published
/// inventory catalog: the five the model's own doc names
/// (`smithy_query(query="doc", shape="InventoryItemSchema", member="TypeName")`: "Default
/// inventory item types include the following: AWS:AWSComponent, AWS:Application,
/// AWS:InstanceInformation, AWS:Network, and AWS:WindowsUpdate") and the rest of the catalog
/// the AWS Systems Manager User Guide tabulates under "Metadata collected by Systems Manager
/// Inventory".
///
/// A published schema says what an account MAY query, filter and aggregate on, so a read
/// reports it whether or not anything has collected an item of that type; the attribute lists
/// themselves are under test in the owning crate's own suite.
const PUBLISHED_TYPES: [&str; 15] = [
    "AWS:AWSComponent",
    "AWS:Application",
    "AWS:ComplianceItem",
    "AWS:ComplianceSummary",
    "AWS:File",
    "AWS:InstanceDetailedInformation",
    PUBLISHED_TYPE,
    "AWS:Network",
    "AWS:PatchSummary",
    "AWS:ResourceGroup",
    "AWS:Service",
    "AWS:Tag",
    "AWS:WindowsRegistry",
    "AWS:WindowsRole",
    "AWS:WindowsUpdate",
];

/// The type names a read reports for a Region holding items of `custom` — the published
/// catalog, then the custom types in type-name order (every `AWS:` name sorts before every
/// `Custom:` one).
fn published_and(custom: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = PUBLISHED_TYPES.iter().map(|name| name.to_string()).collect();
    let mut custom: Vec<String> = custom.iter().map(|name| name.to_string()).collect();
    custom.sort();
    names.extend(custom);
    names
}

/// The custom types the main Region's put writes.
const RACK_INFO: &str = "Custom:RackInfo";
const COMPLIANCE: &str = "Custom:Compliance";
const POWER_DRAW: &str = "Custom:PowerDraw";

/// The custom type the second Region's put writes, so neither Region's answer could be the
/// other's.
const FLEET: &str = "Custom:Fleet";

/// The schema version the main Region's `Custom:RackInfo` put writes, which differs from the
/// one every other put writes so a reported version can only have come from the item.
const RACK_INFO_VERSION: &str = "2.1";

/// The schema version every other put writes.
const VERSION: &str = "1.0";

/// How many custom types the page-truncation case holds: more than the smallest page
/// `GetInventorySchemaMaxResults` admits (50), so a page leaves schemas behind and has a token
/// to hand back.
///
/// The emulator's own per-Region ceiling on the custom types a `PutInventory` may admit
/// (`state::custom_inventory::MAX_CUSTOM_INVENTORY_TYPES`) sits below that page, so this one
/// case stores its items the way a put stores them — one record per `(Region, node, type)` —
/// instead of writing them through the operation. Every other case here writes through
/// `PutInventory`.
const TRUNCATING_TYPES: usize = 56;

/// The whole emulator over one engine, with the engine handle a case seeds the node store
/// through.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    /// Every mounted service on one engine, exactly as the binary assembles it.
    fn new() -> Self {
        let engine = aws_emulator_router::new_engine();
        let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
        // Signature verification is the front door's concern, not this suite's; the router
        // still reads the SigV4 credential scope, which is where each call's Region comes
        // from.
        router.set_request_authentication(false);
        Self { router, engine }
    }

    /// Register a node in `region` the way its own registration would, which is what makes a
    /// put about it a put about a node this account holds.
    fn register(&self, region: &str, node_id: &str) {
        let at = SystemTime::UNIX_EPOCH + LAST_PING;
        let mut node = ssm::state::ManagedNodeRecord::registering(
            node_id,
            ssm::state::RESOURCE_TYPE_EC2_INSTANCE,
            at,
        );
        node.properties.platform_type = Some("Linux".to_string());
        node.properties.instance_state = Some("Active".to_string());
        assert!(
            self.engine
                .state::<ssm::state::ComAmazonawsSsmAmazonssmState>()
                .lock()
                .insert_managed_node(region, node),
            "a seeded node must carry the node id it is stored under"
        );
    }

    /// Hold one item of each of `type_names` about `node_id` in `region`, exactly as a put
    /// holds it: one record per `(Region, node, type)`, carrying one entry.
    fn hold_custom_items(&self, region: &str, node_id: &str, type_names: &[String]) {
        let state = self
            .engine
            .state::<ssm::state::ComAmazonawsSsmAmazonssmState>();
        let mut store = state.lock();
        for type_name in type_names {
            store.insert_custom_inventory(
                ssm::state::custom_inventory::CustomInventoryItemRecord {
                    region: region.to_string(),
                    node_id: node_id.to_string(),
                    type_name: type_name.clone(),
                    schema_version: VERSION.to_string(),
                    capture_time: CAPTURED.to_string(),
                    content_hash: None,
                    content: vec![HashMap::from([(
                        "Serial".to_string(),
                        "SN-7781".to_string(),
                    )])],
                    written_at: SystemTime::now(),
                },
            );
        }
    }

    /// One Systems Manager call over its `awsJson1_1` wire, answering `(status, body)`.
    async fn ssm(&self, region: &str, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{SIGNING_NAME}.{region}.amazonaws.com"))
            .header("authorization", authorization(SIGNING_NAME, region))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
            .body(Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let mut parsed: Value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        // The error shape travels in `__type`; fold in the header the framework also sets so
        // one accessor reads either.
        if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
            map.entry("__type")
                .or_insert_with(|| Value::String(error_type));
        }
        (status, parsed)
    }

    /// `PutInventory` of `items` about `node_id` in `region`, which must be served.
    async fn put(&self, case: &str, region: &str, node_id: &str, items: Value) {
        let (status, body) = self
            .ssm(
                region,
                "PutInventory",
                &json!({ "InstanceId": node_id, "Items": items }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutInventory must be served, got {status} with {body}"
        );
    }

    /// The body of a `GetInventorySchema` that must be served.
    async fn schemas(&self, case: &str, region: &str, body: &Value) -> Value {
        let (status, reported) = self.ssm(region, "GetInventorySchema", body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetInventorySchema {body} must be served, got {status} with \
             {reported}"
        );
        reported
    }

    /// The type names a served `GetInventorySchema` reports, in the order it reports them.
    async fn schema_types(&self, case: &str, region: &str, body: &Value) -> Vec<String> {
        let reported = self.schemas(case, region, body).await;
        schema_types(case, &reported)
    }

    /// The refusal a `GetInventorySchema` answers: its `(status, code, message)`.
    async fn refusal(&self, region: &str, body: &Value) -> (u16, String, String) {
        let (status, parsed) = self.ssm(region, "GetInventorySchema", body).await;
        (status, error_code(&parsed), error_message(&parsed))
    }
}

/// One custom inventory item as a put carries it.
fn item(type_name: &str, version: &str, entries: Value) -> Value {
    json!({
        "TypeName": type_name,
        "SchemaVersion": version,
        "CaptureTime": CAPTURED,
        "Content": entries,
    })
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

/// The attribute names one reported schema carries, in the order it carries them.
fn attribute_names(schema: &Value) -> Vec<String> {
    schema["Attributes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|attribute| attribute["Name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The wire error code of a failure body (`__type`, without its namespace).
fn error_code(body: &Value) -> String {
    let rendered = body["__type"].as_str().unwrap_or("<no __type>");
    rendered
        .split('#')
        .next_back()
        .unwrap_or(rendered)
        .to_string()
}

/// The message a refusal carries. A modelled error of this service renders it as `Message`
/// and the front-door `ValidationException` as `message`, so one accessor reads either.
fn error_message(body: &Value) -> String {
    body["Message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// An emulator holding a node in [`REGION`] with three custom types written about it, and the
/// type names a read of that Region reports when nothing narrows it.
async fn three_custom_types() -> (Emulator, Vec<String>) {
    let emulator = Emulator::new();
    emulator.register(REGION, NODE);
    emulator
        .put(
            "three custom types",
            REGION,
            NODE,
            json!([
                item(
                    RACK_INFO,
                    RACK_INFO_VERSION,
                    json!([{ "RackLocation": "Bay B/Row 4", "Units": "42" }])
                ),
                item(COMPLIANCE, VERSION, json!([{ "Standard": "CIS" }])),
                item(POWER_DRAW, VERSION, json!([{ "Watts": "120" }])),
            ]),
        )
        .await;
    let held = published_and(&[COMPLIANCE, POWER_DRAW, RACK_INFO]);
    (emulator, held)
}

/// RULE: `TypeName` narrows the read to the one type it names — a type Systems Manager
/// publishes, or a custom type the Region holds items of, a well-formed custom name this
/// endpoint holds no schema of reporting nothing — and naming none reports every schema the
/// Region holds.
#[tokio::test]
async fn a_schema_read_narrows_to_the_type_the_request_names() {
    let (emulator, held) = three_custom_types().await;

    /// One row: what the request names, and the type names the read must report.
    struct Row {
        case: &'static str,
        type_name: Option<&'static str>,
        expected: Vec<String>,
    }

    let mut rows = vec![
        Row {
            case: "no TypeName at all",
            type_name: None,
            expected: held.clone(),
        },
        Row {
            // The member's modelled length is 0..100, so the empty string is a value the walk
            // admits: it narrows to no type and reports what the Region holds.
            case: "the empty TypeName the modelled 0..100 length admits",
            type_name: Some(""),
            expected: held.clone(),
        },
        Row {
            case: "the type Systems Manager publishes",
            type_name: Some(PUBLISHED_TYPE),
            expected: vec![PUBLISHED_TYPE.to_string()],
        },
        Row {
            case: "a custom type written with two attributes",
            type_name: Some(RACK_INFO),
            expected: vec![RACK_INFO.to_string()],
        },
        Row {
            case: "a custom type written with one attribute",
            type_name: Some(POWER_DRAW),
            expected: vec![POWER_DRAW.to_string()],
        },
        Row {
            case: "a custom name nothing was written under",
            type_name: Some("Custom:NothingWritten"),
            expected: Vec::new(),
        },
    ];
    // Every other published type name, each of which reports its own published schema — the
    // ones the issue's own reproduction never named among them.
    rows.extend(
        PUBLISHED_TYPES
            .iter()
            .filter(|type_name| **type_name != PUBLISHED_TYPE)
            .map(|type_name| Row {
                case: "another published type name",
                type_name: Some(type_name),
                expected: vec![(*type_name).to_string()],
            }),
    );

    for row in rows {
        let case = row.case;
        let body = match row.type_name {
            None => json!({}),
            Some(type_name) => json!({ "TypeName": type_name }),
        };
        let reported = emulator.schema_types(case, REGION, &body).await;
        assert_eq!(
            reported, row.expected,
            "case {case:?} with TypeName {:?}: expected {:?}, got {reported:?}",
            row.type_name, row.expected
        );
    }
}

/// RULE: a `TypeName` that names no inventory type at all — neither one Systems Manager
/// publishes, spelled exactly, nor a well-formed name of the caller's own `Custom:` namespace
/// — is refused `InvalidTypeNameException`, in the sentence stating those naming rules with
/// the value the request carried in it.
#[tokio::test]
async fn a_type_name_of_no_inventory_namespace_is_refused() {
    let (emulator, _) = three_custom_types().await;

    // One row per shape a name outside both namespaces can take: no namespace at all, a
    // namespace spelled in the wrong case, a namespace with no type after it, a name that
    // only looks like one of the published ones, and a custom name the custom-name pattern
    // does not admit.
    let cases = [
        "NotAType",
        "InstanceInformation",
        "aws:instanceinformation",
        "custom:RackInfo",
        "AWS:",
        "Custom",
        "AWS:InstanceInformation.PlatformType",
        "Custom:Rack-Info",
    ];

    for case in cases {
        let (status, code, message) = emulator
            .refusal(REGION, &json!({ "TypeName": case }))
            .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidTypeNameException"),
            "case {case:?}: expected 400 InvalidTypeNameException, got {status} {code} with \
             {message:?}"
        );
        // The sentence, written out here independently of the service: fixed text apart from
        // the bracketed name, with two spaces before "Custom inventory type name".
        let expected = format!(
            "Inventory type name [{case}] is invalid. Type name is case sensitive, such as \
             AWS:APpLICation is invalid; Type name must start with either AWS: or Custom:;  \
             Custom inventory type name must satisfy regular expression pattern: \
             ^(Custom:)([a-zA-Z0-9]{{3,93}})$"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

/// RULE: `MaxResults` "the maximum number of items to return for this call" bounds the page
/// inside its modelled 50..200 range — a request naming none is bounded by the maximum the
/// operation allows — and a value outside that range is refused by the boundary walk before
/// any schema is read. This endpoint holds fewer schemas than the smallest page it allows, so
/// every accepted bound serves the whole set and hands out no token.
#[tokio::test]
async fn a_schema_read_is_bounded_by_the_page_its_max_results_asks_for() {
    let (emulator, held) = three_custom_types().await;

    // The published bounds of `GetInventorySchemaMaxResults` (range 50..200), the inside of
    // that range, and a request naming no bound at all.
    let accepted: [(&str, Option<i64>); 5] = [
        ("no MaxResults at all", None),
        ("the published minimum", Some(50)),
        ("one inside the minimum", Some(51)),
        ("one inside the maximum", Some(199)),
        ("the published maximum", Some(200)),
    ];
    for (case, max_results) in accepted {
        let body = match max_results {
            None => json!({}),
            Some(max_results) => json!({ "MaxResults": max_results }),
        };
        let reported = emulator.schemas(case, REGION, &body).await;
        assert_eq!(
            schema_types(case, &reported),
            held,
            "case {case:?}: expected every schema the Region holds, got {reported}"
        );
        assert_eq!(
            reported["NextToken"],
            Value::Null,
            "case {case:?}: a page that left no schema behind hands out no token, got \
             {reported}"
        );
    }

    // One past each published bound, and further past each — with the bound the refusal
    // states, so a row cannot pass on any 400 whatever its wording.
    let under = "Member must have value greater than or equal to 50";
    let over = "Member must have value less than or equal to 200";
    let refused: [(&str, i64, &str); 4] = [
        ("one under the published minimum", 49, under),
        ("one over the published maximum", 201, over),
        ("no items at all", 0, under),
        ("a negative page", -1, under),
    ];
    for (case, max_results, clause) in refused {
        let (status, code, message) = emulator
            .refusal(REGION, &json!({ "MaxResults": max_results }))
            .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?} (MaxResults {max_results}): expected 400 ValidationException, got \
             {status} {code} with {message:?}"
        );
        let expected = format!(
            "1 validation error detected: Value '{max_results}' at 'maxResults' failed to \
             satisfy constraint: {clause}"
        );
        assert_eq!(
            message, expected,
            "case {case:?} (MaxResults {max_results}): expected {expected:?}, got {message:?}"
        );
    }
}

/// RULE: a page that leaves schemas behind hands back "the token to use when requesting the
/// next set of items", and the page that token resumes carries on where the one before it
/// stopped — so walking the token chain reports every schema the Region holds, each once, in
/// one order, with no page longer than the `MaxResults` it was bounded by; a page that leaves
/// nothing behind hands back no token.
#[tokio::test]
async fn a_page_that_leaves_schemas_behind_hands_back_the_token_the_next_one_resumes_at() {
    let emulator = Emulator::new();
    emulator.register(REGION, NODE);
    let types: Vec<String> = (0..TRUNCATING_TYPES)
        .map(|index| format!("Custom:Type{index:03}"))
        .collect();
    emulator.hold_custom_items(REGION, NODE, &types);
    // What the Region holds: the published catalog, then the custom ones in type-name order.
    let held = published_and(&types.iter().map(String::as_str).collect::<Vec<&str>>());

    // One row per page the held set divides into: the smallest page the operation admits, one
    // just under the held set, and one exactly as large as it.
    let pages: [(&str, usize); 3] = [
        ("the published minimum page", 50),
        ("a page one schema short of the set", held.len() - 1),
        ("a page exactly as large as the set", held.len()),
    ];
    for (case, page) in pages {
        let expected_pages = held.len().div_ceil(page);
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut served = 0usize;
        loop {
            let mut body = serde_json::Map::new();
            body.insert("MaxResults".to_string(), json!(page));
            if let Some(token) = &token {
                body.insert("NextToken".to_string(), json!(token));
            }
            let reported = emulator.schemas(case, REGION, &Value::Object(body)).await;
            let names = schema_types(case, &reported);
            assert!(
                !names.is_empty() && names.len() <= page,
                "case {case:?}: a page of at most {page} schema(s) and never an empty one, got \
                 {} in {reported}",
                names.len()
            );
            seen.extend(names);
            served += 1;
            token = reported["NextToken"].as_str().map(str::to_string);
            // A page that left nothing behind is the last one.
            if token.is_none() {
                break;
            }
            assert!(
                served <= expected_pages,
                "case {case:?}: expected {expected_pages} page(s) of {page} over {} schema(s), \
                 and page {served} still handed back {token:?}",
                held.len()
            );
        }
        assert_eq!(
            served, expected_pages,
            "case {case:?}: expected {} schema(s) to divide into {expected_pages} page(s) of \
             {page}, got {served}",
            held.len()
        );
        assert_eq!(
            seen, held,
            "case {case:?}: walking the token chain reports every schema the Region holds, each \
             once and in one order"
        );
    }
}

/// RULE: `NextToken` "the token for the next set of items to return" resumes the read at the
/// schema the page it came from stopped at, so a token naming a schema this read reports
/// serves that schema and the ones after it; a token naming no schema of this read is
/// refused `InvalidNextToken`.
#[tokio::test]
async fn a_schema_read_resumes_at_the_token_it_is_handed() {
    let (emulator, held) = three_custom_types().await;

    /// One row: what the request carries, and the type names the read must report.
    struct Row {
        case: &'static str,
        type_name: Option<&'static str>,
        next_token: Option<&'static str>,
        expected: Vec<String>,
    }

    let rows = [
        Row {
            case: "no token at all",
            type_name: None,
            next_token: None,
            expected: held.clone(),
        },
        Row {
            case: "the empty token",
            type_name: None,
            next_token: Some(""),
            expected: held.clone(),
        },
        Row {
            case: "a token at the first schema",
            type_name: None,
            next_token: Some(PUBLISHED_TYPES[0]),
            expected: held.clone(),
        },
        Row {
            case: "a token in the middle of the published catalog",
            type_name: None,
            next_token: Some(PUBLISHED_TYPE),
            expected: held
                .iter()
                .skip_while(|name| name.as_str() != PUBLISHED_TYPE)
                .cloned()
                .collect(),
        },
        Row {
            case: "a token in the middle of the set",
            type_name: None,
            next_token: Some(POWER_DRAW),
            expected: vec![POWER_DRAW.to_string(), RACK_INFO.to_string()],
        },
        Row {
            case: "a token at the last schema",
            type_name: None,
            next_token: Some(RACK_INFO),
            expected: vec![RACK_INFO.to_string()],
        },
        Row {
            case: "a token at the one schema a narrowed read reports",
            type_name: Some(RACK_INFO),
            next_token: Some(RACK_INFO),
            expected: vec![RACK_INFO.to_string()],
        },
    ];

    for row in rows {
        let case = row.case;
        let mut body = serde_json::Map::new();
        if let Some(type_name) = row.type_name {
            body.insert("TypeName".to_string(), json!(type_name));
        }
        if let Some(next_token) = row.next_token {
            body.insert("NextToken".to_string(), json!(next_token));
        }
        let body = Value::Object(body);
        let reported = emulator.schema_types(case, REGION, &body).await;
        assert_eq!(
            reported, row.expected,
            "case {case:?} with {body}: expected {:?}, got {reported:?}",
            row.expected
        );
    }

    // A token that names no schema THIS read reports names no page of it to resume.
    let refused: [(&str, Option<&str>, &str); 4] = [
        ("a token of no inventory namespace", None, "NotAToken"),
        (
            "a token naming a custom type nothing was written under",
            None,
            "Custom:NothingWritten",
        ),
        (
            "a token naming a type no narrowed read of another type reports",
            Some(RACK_INFO),
            "AWS:Network",
        ),
        (
            "a token outside what the narrowed read reports",
            Some(RACK_INFO),
            PUBLISHED_TYPE,
        ),
    ];
    for (case, type_name, next_token) in refused {
        let mut body = serde_json::Map::new();
        if let Some(type_name) = type_name {
            body.insert("TypeName".to_string(), json!(type_name));
        }
        body.insert("NextToken".to_string(), json!(next_token));
        let body = Value::Object(body);
        let (status, code, message) = emulator.refusal(REGION, &body).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidNextToken"),
            "case {case:?} with {body}: expected 400 InvalidNextToken, got {status} {code} \
             with {message:?}"
        );
        assert_eq!(
            message, "The NextToken value is not valid",
            "case {case:?}: expected the token refusal Systems Manager words for this error \
             class, got {message:?}"
        );
    }
}

/// RULE: `Aggregator` "returns inventory schemas that support aggregation" — the published
/// type, carrying only the attributes aggregation counts over — and `SubType` "returns the
/// sub-type schema for a specified inventory type", of which this endpoint holds none, so a
/// sub-type read reports nothing however the other flag is set.
#[tokio::test]
async fn the_narrowing_flags_report_what_the_endpoint_holds_of_them() {
    let (emulator, held) = three_custom_types().await;

    /// One row: both flags as the request carries them, and the type names it must report.
    struct Row {
        case: &'static str,
        aggregator: Option<bool>,
        sub_type: Option<bool>,
        expected: Vec<String>,
    }

    let rows = [
        Row {
            case: "neither flag",
            aggregator: None,
            sub_type: None,
            expected: held.clone(),
        },
        Row {
            case: "both flags off",
            aggregator: Some(false),
            sub_type: Some(false),
            expected: held.clone(),
        },
        Row {
            case: "aggregation only",
            aggregator: Some(true),
            sub_type: None,
            expected: vec![PUBLISHED_TYPE.to_string()],
        },
        Row {
            case: "aggregation with the sub-type flag off",
            aggregator: Some(true),
            sub_type: Some(false),
            expected: vec![PUBLISHED_TYPE.to_string()],
        },
        Row {
            case: "sub-type schemas",
            aggregator: None,
            sub_type: Some(true),
            expected: Vec::new(),
        },
        Row {
            case: "sub-type schemas that support aggregation",
            aggregator: Some(true),
            sub_type: Some(true),
            expected: Vec::new(),
        },
    ];

    for row in rows {
        let case = row.case;
        let mut body = serde_json::Map::new();
        if let Some(aggregator) = row.aggregator {
            body.insert("Aggregator".to_string(), json!(aggregator));
        }
        if let Some(sub_type) = row.sub_type {
            body.insert("SubType".to_string(), json!(sub_type));
        }
        let body = Value::Object(body);
        let reported = emulator.schema_types(case, REGION, &body).await;
        assert_eq!(
            reported, row.expected,
            "case {case:?} with {body}: expected {:?}, got {reported:?}",
            row.expected
        );
    }

    // An aggregatable schema carries the three attributes the member's own documentation
    // names, which are a PROPER subset of the attributes the unnarrowed read reports.
    let aggregatable = emulator
        .schemas("aggregation only", REGION, &json!({ "Aggregator": true }))
        .await;
    let aggregatable_attributes = attribute_names(&aggregatable["Schemas"][0]);
    assert_eq!(
        aggregatable_attributes, AGGREGATABLE_ATTRIBUTES,
        "case aggregation only: expected {AGGREGATABLE_ATTRIBUTES:?}, got {aggregatable}"
    );
    let full = emulator
        .schemas(
            "the published type unnarrowed",
            REGION,
            &json!({ "TypeName": PUBLISHED_TYPE }),
        )
        .await;
    let full_attributes = attribute_names(&full["Schemas"][0]);
    assert!(
        full_attributes.len() > aggregatable_attributes.len()
            && aggregatable_attributes
                .iter()
                .all(|name| full_attributes.contains(name)),
        "case the published type unnarrowed: the aggregatable attributes are a proper subset \
         of the type's own, got {full_attributes:?} against {aggregatable_attributes:?}"
    );
}

/// RULE: the schemas a read reports are the ones the Region it was signed for holds items of,
/// and a custom type's attributes, their data types and its version are what those items say
/// — so what one Region's `PutInventory` wrote is what that Region's read reports, and no
/// other Region's.
#[tokio::test]
async fn the_schemas_are_the_ones_the_signed_region_holds_items_of() {
    let emulator = Emulator::new();
    emulator.register(REGION, NODE);
    emulator.register(OTHER_REGION, NODE);
    emulator
        .put(
            "the main Region's type",
            REGION,
            NODE,
            json!([item(
                RACK_INFO,
                RACK_INFO_VERSION,
                json!([{ "RackLocation": "Bay B/Row 4", "Units": "42" }])
            )]),
        )
        .await;
    emulator
        .put(
            "the second Region's type",
            OTHER_REGION,
            NODE,
            json!([item(FLEET, VERSION, json!([{ "Fleet": "edge-7" }]))]),
        )
        .await;

    /// One row: the Region the read is signed for, and the type names it must report.
    struct Row {
        case: &'static str,
        region: &'static str,
        expected: Vec<String>,
    }

    let rows = [
        Row {
            case: "the Region the first put wrote in",
            region: REGION,
            expected: published_and(&[RACK_INFO]),
        },
        Row {
            case: "the Region the second put wrote in",
            region: OTHER_REGION,
            expected: published_and(&[FLEET]),
        },
        Row {
            case: "a Region holding no node and no item",
            region: EMPTY_REGION,
            expected: published_and(&[]),
        },
    ];
    for row in rows {
        let case = row.case;
        let reported = emulator.schema_types(case, row.region, &json!({})).await;
        assert_eq!(
            reported, row.expected,
            "case {case:?} in {}: expected {:?}, got {reported:?}",
            row.region, row.expected
        );
    }

    // The custom schema itself: the attribute names its items carry in name order, the
    // uppercase wire data type the service serializes — `NUMBER` for the one whose every value
    // is a number — and the version the put that wrote it carried, which differs from every
    // other put's here.
    let narrowed = emulator
        .schemas(
            "the custom type of the first Region",
            REGION,
            &json!({ "TypeName": RACK_INFO }),
        )
        .await;
    assert_eq!(
        narrowed["Schemas"],
        json!([{
            "TypeName": RACK_INFO,
            "Version": RACK_INFO_VERSION,
            "Attributes": [
                { "Name": "RackLocation", "DataType": "STRING" },
                { "Name": "Units", "DataType": "NUMBER" },
            ],
        }]),
        "case the custom type of the first Region: a custom schema is what its items say, got \
         {narrowed}"
    );
    // The same name in the Region that holds no item of it reports nothing.
    let elsewhere = emulator
        .schema_types(
            "the first Region's type read in the second",
            OTHER_REGION,
            &json!({ "TypeName": RACK_INFO }),
        )
        .await;
    assert_eq!(
        elsewhere,
        Vec::<String>::new(),
        "case the first Region's type read in the second: a schema is held by the Region its \
         items were written in, got {elsewhere:?}"
    );
}
