// SPDX-License-Identifier: CC0-1.0

//! The details member an entity's `Name` is read from, end to end over HTTP.
//!
//! One rule is under test: an entity carries its name in the ONE details member its own
//! entity type names that value under — `ProductTitle` for a type whose per-type summary
//! is a product summary, `Name` for every other type — so the name `ListEntities` reports,
//! the name that type's own summary reports, the attribute its own filters select on and
//! the attribute its own sorts order by are all the SAME value.
//!
//! Every entity in these fixtures carries BOTH members, with a different value at each, so
//! a read that took the wrong one is visible: the member belonging to another type is
//! persisted in the entity's details and reported by no name.
//!
//! Every assertion goes through the router, so each case exercises routing,
//! deserialization, the handler, the persisted state and serialization.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_com_amazonaws_marketplacecatalog_awsmpseymour::state::PRODUCT_ENTITY_TYPES;
use service_com_amazonaws_marketplacecatalog_awsmpseymour::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The catalog every operation's `Catalog` member documents as its fixed value.
const CATALOG: &str = "AWSMarketplace";

/// A SigV4 credential scope naming this service's signing name (`aws-marketplace`).
const SIGNED_FOR_EU_WEST_1: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/aws-marketplace/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The details member the five product types name an entity's name under, read off
/// `AmiProductSummary$ProductTitle` and the four product summaries beside it
/// (`smithy_query(query="shape", shape="AmiProductSummary")`).
const PRODUCT_TITLE: &str = "ProductTitle";

/// The details member every other type names it under, read off `OfferSummary$Name`,
/// `OfferSetSummary$Name` and `ResaleAuthorizationSummary$Name`
/// (`smithy_query(query="shape", shape="OfferSummary,OfferSetSummary,\
/// ResaleAuthorizationSummary")`) — the member `EntitySummary$Name` itself carries.
const NAME: &str = "Name";

/// The entity types whose own summary names the entity's name `Name`: the three per-type
/// summaries of `EntitySummary` that declare a `Name` member (`OfferSummary`,
/// `OfferSetSummary`, `ResaleAuthorizationSummary`).
const NAME_SUMMARY_TYPES: &[&str] = &["Offer", "OfferSet", "ResaleAuthorization"];

/// How long a poll may keep looking before the test gives up. Generous on purpose: the
/// condition is what is waited for, never a span.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks. Only ever slept INSIDE a poll loop.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// The details member an entity of `entity_type` carries its name in, under the rule the
/// model's summaries state.
fn name_member(entity_type: &str) -> &'static str {
    if PRODUCT_ENTITY_TYPES.contains(&entity_type) {
        PRODUCT_TITLE
    } else {
        NAME
    }
}

/// The member of the per-type summary of `entity_type` that reports the entity's name, and
/// `None` for a type whose summary names it under no member of its own.
fn summary_name_pointer(entity_type: &str) -> Option<String> {
    if PRODUCT_ENTITY_TYPES.contains(&entity_type) {
        Some(format!("/{entity_type}Summary/{PRODUCT_TITLE}"))
    } else if NAME_SUMMARY_TYPES.contains(&entity_type) {
        Some(format!("/{entity_type}Summary/{NAME}"))
    } else {
        None
    }
}

/// The change type that creates an entity of `entity_type`, as this catalog's change-type
/// catalogue names them.
fn create_change_type(entity_type: &str) -> &'static str {
    match entity_type {
        "Offer" => "CreateOffer",
        "ResaleAuthorization" => "CreateResaleAuthorization",
        _ => "CreateProduct",
    }
}

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The string at `pointer`, or the empty string.
    fn text(&self, pointer: &str) -> String {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The array at `pointer`, or an empty one.
    fn list(&self, pointer: &str) -> Vec<Value> {
        self.body
            .pointer(pointer)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "catalog.marketplace.eu-west-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_EU_WEST_1);
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

async fn list_entities(router: &MultiServiceRouter, request: Value) -> Answer {
    call(router, "POST", "/ListEntities", Some(request)).await
}

async fn describe_entity(router: &MultiServiceRouter, entity_id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/DescribeEntity?catalog={CATALOG}&entityId={entity_id}"),
        None,
    )
    .await
}

/// Read change set `id` until the clock has carried it to a terminal status, and answer
/// the status it reached. The wait is for the CONDITION, bounded by a deadline generous
/// enough for a loaded machine.
async fn poll_until_terminal(router: &MultiServiceRouter, id: &str) -> String {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let described = call(
            router,
            "GET",
            &format!("/DescribeChangeSet?catalog={CATALOG}&changeSetId={id}"),
            None,
        )
        .await;
        let status = described.text("/Status");
        if ["SUCCEEDED", "FAILED", "CANCELLED"].contains(&status.as_str()) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "change set {id} must reach a terminal status, last reported {status:?}"
        );
        std::thread::sleep(POLL_PAUSE);
    }
}

/// The details one fixture submits: both name members, each with its own value.
fn both_members(product_title: &str, name: &str) -> Value {
    let mut details = Map::new();
    details.insert(PRODUCT_TITLE.to_string(), json!(product_title));
    details.insert(NAME.to_string(), json!(name));
    Value::Object(details)
}

/// Create one entity of `entity_type` per `(product_title, name)` pair, each through its
/// own change set, and wait for each change set to succeed.
async fn created(router: &MultiServiceRouter, case: &str, entity_type: &str, pairs: &[(&str, &str)]) {
    let mut change_sets: Vec<String> = Vec::new();
    for (product_title, name) in pairs {
        let started = call(
            router,
            "POST",
            "/StartChangeSet",
            Some(json!({
                "Catalog": CATALOG,
                "ChangeSet": [{
                    "ChangeType": create_change_type(entity_type),
                    "Entity": {"Type": format!("{entity_type}@1.0")},
                    "DetailsDocument": both_members(product_title, name),
                }],
            })),
        )
        .await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: a change creating an entity carrying both name members must be \
             accepted, got {} with {:?}",
            started.status, started.body
        );
        change_sets.push(started.text("/ChangeSetId"));
    }
    for change_set in &change_sets {
        let status = poll_until_terminal(router, change_set).await;
        assert_eq!(
            status, "SUCCEEDED",
            "case {case:?}: the fixture's change set {change_set} must succeed, got {status:?}"
        );
    }
}

/// The one summary a page of `entity_type` serves.
async fn only_summary(router: &MultiServiceRouter, case: &str, entity_type: &str) -> Value {
    let listed = list_entities(
        router,
        json!({"Catalog": CATALOG, "EntityType": entity_type, "MaxResults": 50}),
    )
    .await;
    let summaries = listed.list("/EntitySummaryList");
    assert_eq!(
        (listed.status, summaries.len()),
        (200, 1),
        "case {case:?}: the one entity of the type must be served, got {} with {:?}",
        listed.status,
        listed.body
    );
    summaries[0].clone()
}

/// An entity whose details carry BOTH name members reports the value at the member its own
/// type names, through `EntitySummary$Name` and through its own summary's name member
/// alike; the other member stays in the entity's details and is reported as no name.
///
/// The rows span the member the type names (both of them), the five product types and the
/// types whose summary names `Name`, and two types whose summary names neither member —
/// one with per-type shapes of its own and one with none.
#[tokio::test]
async fn an_entity_reports_the_name_member_its_own_type_names() {
    // (entity type, the ProductTitle the details carry, the Name they carry)
    let rows: &[(&str, &str, &str)] = &[
        ("AmiProduct", "ami title", "ami name"),
        ("ContainerProduct", "container title", "container name"),
        ("DataProduct", "data title", "data name"),
        ("SaaSProduct", "saas title", "saas name"),
        ("MachineLearningProduct", "ml title", "ml name"),
        ("Offer", "offer title", "offer name"),
        ("OfferSet", "set title", "set name"),
        ("ResaleAuthorization", "resale title", "resale name"),
        (
            "ExpressPrivateOfferConfiguration",
            "configuration title",
            "configuration name",
        ),
        ("Solution", "solution title", "solution name"),
    ];

    for (entity_type, product_title, name) in rows {
        let case = format!("{entity_type} naming {product_title:?} and {name:?}");
        let router = emulator();
        created(&router, &case, entity_type, &[(product_title, name)]).await;

        let (expected, other) = if name_member(entity_type) == PRODUCT_TITLE {
            (*product_title, *name)
        } else {
            (*name, *product_title)
        };
        let summary = only_summary(&router, &case, entity_type).await;
        assert_eq!(
            summary["Name"],
            json!(expected),
            "case {case:?}: Name must be the {} the details carry, got {summary:?}",
            name_member(entity_type)
        );
        if let Some(pointer) = summary_name_pointer(entity_type) {
            assert_eq!(
                summary.pointer(&pointer),
                Some(&json!(expected)),
                "case {case:?}: {pointer} must carry the same name, got {summary:?}"
            );
        }
        // The member another type names a name under is reported by no name of this one.
        assert_ne!(
            summary["Name"],
            json!(other),
            "case {case:?}: the member another type names must be reported by no name, got \
             {summary:?}"
        );

        // Both members are persisted: only which one the name is READ from follows the
        // type, so the entity's details still carry what the change submitted.
        let entity_id = summary["EntityId"].as_str().unwrap_or_default().to_string();
        let described = describe_entity(&router, &entity_id).await;
        let details: Value = serde_json::from_str(&described.text("/Details"))
            .unwrap_or_else(|error| panic!("case {case:?}: Details must be JSON: {error}"));
        assert_eq!(
            (&details[PRODUCT_TITLE], &details[NAME]),
            (&json!(product_title), &json!(name)),
            "case {case:?}: both submitted members must be stored, got {details:?}"
        );
    }
}

/// A type's own `Name`-shaped filter selects on the value the page REPORTS: filtering by
/// the reported name serves the entity, and filtering by the value held at the member
/// another type names serves an empty page.
///
/// The rows span the five types whose own filter shape declares the name member
/// (`AmiProductFilters$ProductTitle` and the four product filters beside it,
/// `OfferFilters$Name`, `OfferSetFilters$Name`, `ResaleAuthorizationFilters$Name`), in
/// both directions: the reported value matches, the other member's value does not.
#[tokio::test]
async fn a_type_name_filter_selects_on_the_value_the_page_reports() {
    // (entity type, the ProductTitle the details carry, the Name they carry)
    let rows: &[(&str, &str, &str)] = &[
        ("AmiProduct", "ami title", "ami name"),
        ("ContainerProduct", "container title", "container name"),
        ("DataProduct", "data title", "data name"),
        ("SaaSProduct", "saas title", "saas name"),
        ("MachineLearningProduct", "ml title", "ml name"),
        ("Offer", "offer title", "offer name"),
        ("OfferSet", "set title", "set name"),
        ("ResaleAuthorization", "resale title", "resale name"),
    ];

    for (entity_type, product_title, name) in rows {
        let case = format!("{entity_type} filtered on its own name member");
        let router = emulator();
        created(&router, &case, entity_type, &[(product_title, name)]).await;

        let member = name_member(entity_type);
        let (reported, unreported) = if member == PRODUCT_TITLE {
            (*product_title, *name)
        } else {
            (*name, *product_title)
        };

        let served = list_entities(
            &router,
            json!({
                "Catalog": CATALOG,
                "EntityType": entity_type,
                "EntityTypeFilters": {
                    format!("{entity_type}Filters"): {member: {"ValueList": [reported]}},
                },
            }),
        )
        .await;
        let summaries = served.list("/EntitySummaryList");
        assert_eq!(
            (served.status, summaries.len()),
            (200, 1),
            "case {case:?}: filtering by the reported name {reported:?} must serve the \
             entity, got {} with {:?}",
            served.status,
            served.body
        );
        assert_eq!(
            summaries[0]["Name"],
            json!(reported),
            "case {case:?}: the selected entity must report the value selected on, got {:?}",
            summaries[0]
        );

        let empty = list_entities(
            &router,
            json!({
                "Catalog": CATALOG,
                "EntityType": entity_type,
                "EntityTypeFilters": {
                    format!("{entity_type}Filters"): {member: {"ValueList": [unreported]}},
                },
            }),
        )
        .await;
        assert_eq!(
            (empty.status, empty.list("/EntitySummaryList").len()),
            (200, 0),
            "case {case:?}: filtering by {unreported:?}, which the member of another type \
             holds, must serve an empty page, got {} with {:?}",
            empty.status,
            empty.body
        );
    }
}

/// A page ordered by a type's own name attribute is ordered by the name it REPORTS.
///
/// Each type's two entities are seeded so the two members order them oppositely: ordering
/// by the member another type names would serve the reverse sequence. The rows span the
/// member the type names, ascending and descending, and the two surfaces that ask for an
/// ordering (`EntityTypeSort`, and the generic `Sort`, whose `SortBy` documents that "each
/// `EntityType` might support additional fields").
#[tokio::test]
async fn a_name_ordering_follows_the_name_the_page_reports() {
    // (entity type, the two entities' (ProductTitle, Name) pairs)
    let rows: &[(&str, [(&str, &str); 2])] = &[
        (
            "AmiProduct",
            [("alpha title", "zulu name"), ("zulu title", "alpha name")],
        ),
        (
            "DataProduct",
            [("zulu title", "alpha name"), ("alpha title", "zulu name")],
        ),
        (
            "Offer",
            [("alpha title", "zulu name"), ("zulu title", "alpha name")],
        ),
        (
            "ResaleAuthorization",
            [("zulu title", "alpha name"), ("alpha title", "zulu name")],
        ),
    ];

    for (entity_type, pairs) in rows {
        let router = emulator();
        let case = format!("{entity_type} ordered by its own name member");
        created(&router, &case, entity_type, pairs).await;

        let member = name_member(entity_type);
        let mut ascending: Vec<String> = pairs
            .iter()
            .map(|(product_title, name)| {
                if member == PRODUCT_TITLE {
                    (*product_title).to_string()
                } else {
                    (*name).to_string()
                }
            })
            .collect();
        ascending.sort();
        let mut descending = ascending.clone();
        descending.reverse();

        for (order, expected) in [("ASCENDING", &ascending), ("DESCENDING", &descending)] {
            let surfaces = [
                (
                    "EntityTypeSort",
                    json!({
                        "Catalog": CATALOG,
                        "EntityType": entity_type,
                        "EntityTypeSort": {
                            format!("{entity_type}Sort"): {"SortBy": member, "SortOrder": order},
                        },
                    }),
                ),
                (
                    "Sort",
                    json!({
                        "Catalog": CATALOG,
                        "EntityType": entity_type,
                        "Sort": {"SortBy": member, "SortOrder": order},
                    }),
                ),
            ];
            for (surface, request) in surfaces {
                let answer = list_entities(&router, request).await;
                let served: Vec<String> = answer
                    .list("/EntitySummaryList")
                    .iter()
                    .map(|item| item["Name"].as_str().unwrap_or_default().to_string())
                    .collect();
                assert_eq!(
                    (answer.status, &served),
                    (200, expected),
                    "case {case:?}/{surface}/{order}: the page must report its names in the \
                     order asked for, got {} with {:?}",
                    answer.status,
                    answer.body
                );
            }
        }
    }
}
