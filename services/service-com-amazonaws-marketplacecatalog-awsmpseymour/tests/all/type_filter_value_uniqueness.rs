// SPDX-License-Identifier: CC0-1.0

//! A `ListEntities` filter `ValueList` the model documents as a list of UNIQUE values
//! refuses a request that names one of its values twice, on BOTH filter surfaces.
//!
//! `ListEntities` filters on an entity's attributes two ways — the generic `FilterList`
//! and the listed type's own `EntityTypeFilters` — and each `ValueList` says for itself
//! whether its values are unique. `Filter$ValueList` states it for the generic surface
//! ("`ListEntities` - This is a list of unique `EntityId`s"), and the per-type filter
//! shapes state it field by field: `AmiProductEntityIdFilter$ValueList`,
//! `AmiProductTitleFilter$ValueList` and `AmiProductVisibilityFilter$ValueList` are each
//! "A string array of unique … values to be filtered on", and so are the same three
//! fields of `ContainerProductFilters`, `DataProductFilters` and `SaaSProductFilters`
//! (`smithy_query(query="shape", shape="AmiProductEntityIdFilter")`). So a value repeated
//! in one of those lists is a request the documentation forbids, whichever surface it
//! arrived on, and the modelled `ValidationException` (422) refuses it.
//!
//! The other half of the rule is what the documentation does NOT state: the
//! `MachineLearningProduct` fields are documented "A list of entity IDs to filter by",
//! and the `Offer`, `OfferSet`, `ResaleAuthorization` and
//! `ExpressPrivateOfferConfiguration` fields "… with list input" / "A list of
//! `ProductId` values to filter on", with no uniqueness stated and no `@uniqueItems` on
//! any `ValueList` shape — so a repetition in one of those is served, as the generic
//! surface serves one in the list documented non-unique.
//!
//! Every case goes through the router over HTTP, against entities a change set really
//! created, so an accepted filter is answered from persisted state and a refused one
//! serves no page at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_marketplacecatalog_awsmpseymour::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The catalog every operation's `Catalog` member documents as its fixed value.
const CATALOG: &str = "AWSMarketplace";

/// A SigV4 credential scope naming this service's signing name (`aws-marketplace`), which
/// is what routes a request here.
const SIGNED_FOR_EU_WEST_1: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/aws-marketplace/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// A well-formed entity id (`EntityId`'s `^[a-zA-Z0-9][.a-zA-Z0-9/-]+[a-zA-Z0-9]$`) that
/// names no entity this fixture created, so a filter carrying it selects nothing.
const UNHELD_ENTITY_ID: &str = "ent-000000000000";

/// A product title no fixture entity carries.
const UNHELD_TITLE: &str = "no entity is titled this";

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
    /// The wire error code (`__type`), or the empty string on a success.
    fn error_code(&self) -> &str {
        let raw = self.body["__type"].as_str().unwrap_or_default();
        raw.rsplit('#').next().unwrap_or(raw)
    }

    /// The fault this answer carries: `ValidationException$Message`.
    fn message(&self) -> String {
        self.body["Message"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

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

/// One `ListEntities` call, with `EntityTypeFilters` when the case carries one.
async fn list_entities(
    router: &MultiServiceRouter,
    entity_type: &str,
    filters: Option<Value>,
    filter_list: Option<Value>,
) -> Answer {
    let mut request = json!({"Catalog": CATALOG, "EntityType": entity_type});
    if let Some(filters) = filters {
        request["EntityTypeFilters"] = filters;
    }
    if let Some(filter_list) = filter_list {
        request["FilterList"] = filter_list;
    }
    call(router, "POST", "/ListEntities", Some(request)).await
}

/// Read change set `id` until the clock has carried it to a terminal status, and answer
/// the status it reached. The wait is for the CONDITION, bounded by a deadline generous
/// enough for a loaded machine.
async fn poll_until_terminal(router: &MultiServiceRouter, id: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(60);
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
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Create one entity per `(entity_type, details)` through its own change set, and answer
/// the id of each, in the order asked for.
///
/// Every change set is started before any is waited on, so the clock carries them all
/// through the one span.
async fn created_entities(router: &MultiServiceRouter, specs: &[(&str, Value)]) -> Vec<String> {
    let mut change_sets: Vec<String> = Vec::new();
    for (entity_type, details) in specs {
        let started = call(
            router,
            "POST",
            "/StartChangeSet",
            Some(json!({
                "Catalog": CATALOG,
                "ChangeSet": [{
                    "ChangeType": "CreateProduct",
                    "Entity": {"Type": format!("{entity_type}@1.0")},
                    "DetailsDocument": details,
                }],
            })),
        )
        .await;
        assert_eq!(
            started.status, 200,
            "the fixture must start a change set for {entity_type}, got {} with {:?}",
            started.status, started.body
        );
        change_sets.push(started.text("/ChangeSetId"));
    }

    for change_set in &change_sets {
        let status = poll_until_terminal(router, change_set).await;
        assert_eq!(
            status, "SUCCEEDED",
            "the fixture's change set {change_set} must succeed, got {status:?}"
        );
    }

    let listed = call(
        router,
        "POST",
        "/ListChangeSets",
        Some(json!({"Catalog": CATALOG, "MaxResults": 20})),
    )
    .await;
    let summaries = listed.list("/ChangeSetSummaryList");
    change_sets
        .iter()
        .map(|change_set| {
            let summary = summaries
                .iter()
                .find(|item| item["ChangeSetId"] == json!(change_set))
                .unwrap_or_else(|| {
                    panic!(
                        "the fixture's change set {change_set} must be listed, got {:?}",
                        listed.body
                    )
                });
            summary["EntityIdList"][0]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("change set {change_set} must name its entity, got {summary:?}")
                })
                .to_string()
        })
        .collect()
}

/// The entity types whose `*Filters` fields the model documents as lists of unique
/// values, with the title each fixture entity of that type carries.
const UNIQUE_TYPES: &[(&str, &str)] = &[
    ("AmiProduct", "ami one"),
    ("ContainerProduct", "container one"),
    ("DataProduct", "data one"),
    ("SaaSProduct", "saas one"),
];

/// One entity of each type whose filter fields are documented unique. `Limited` and
/// `Draft`, the two visibilities the cases filter on, are published by every one of those
/// types' own `*VisibilityString` enums.
fn unique_type_fixtures() -> Vec<(&'static str, Value)> {
    UNIQUE_TYPES
        .iter()
        .map(|(entity_type, title)| {
            (
                *entity_type,
                json!({"ProductTitle": title, "Visibility": "Limited"}),
            )
        })
        .collect()
}

/// A value repeated in a `*Filters` field documented "A string array of unique … values
/// to be filtered on" is refused, wherever in the list the repetition falls and over
/// every one of the four types and three fields the model states it for.
#[tokio::test]
async fn a_repeated_value_is_refused_in_every_type_filter_documented_unique() {
    let router = emulator();
    let ids = created_entities(&router, &unique_type_fixtures()).await;

    for (at, (entity_type, title)) in UNIQUE_TYPES.iter().enumerate() {
        let held = ids[at].clone();
        // Rows differ along the field, where the repetition falls and how many distinct
        // values surround it.
        let rows: Vec<(&str, &str, Vec<String>, String)> = vec![
            (
                "the listed entity's own id twice",
                "EntityId",
                vec![held.clone(), held.clone()],
                held.clone(),
            ),
            (
                "an id naming nothing, twice behind a held one",
                "EntityId",
                vec![
                    held.clone(),
                    UNHELD_ENTITY_ID.to_string(),
                    UNHELD_ENTITY_ID.to_string(),
                ],
                UNHELD_ENTITY_ID.to_string(),
            ),
            (
                "a title repeated across a distinct one",
                "ProductTitle",
                vec![
                    (*title).to_string(),
                    UNHELD_TITLE.to_string(),
                    (*title).to_string(),
                ],
                (*title).to_string(),
            ),
            (
                "the same visibility twice",
                "Visibility",
                vec!["Limited".to_string(), "Limited".to_string()],
                "Limited".to_string(),
            ),
            (
                "a visibility repeated across a distinct one",
                "Visibility",
                vec![
                    "Draft".to_string(),
                    "Limited".to_string(),
                    "Draft".to_string(),
                ],
                "Draft".to_string(),
            ),
        ];

        for (row, member, values, offending) in rows {
            let case = format!("{entity_type}.{member}: {row}");
            let filters = json!({
                format!("{entity_type}Filters"): {member: {"ValueList": values}},
            });
            let answer = list_entities(&router, entity_type, Some(filters), None).await;
            assert_eq!(
                answer.status, 422,
                "case {case:?}: a repeated value in a list documented unique must be refused \
                 with 422, got {} with {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code(),
                "ValidationException",
                "case {case:?}: must answer ValidationException, got {:?}",
                answer.body
            );
            let message = answer.message();
            let names_field = message.contains("EntityTypeFilters")
                && message.contains(&format!("{entity_type}Filters.{member}"));
            assert!(
                names_field && message.contains(&offending),
                "case {case:?}: the fault must name EntityTypeFilters, the field \
                 {entity_type}Filters.{member} and quote the repeated value {offending:?}, \
                 got {message:?}"
            );
            let served = answer.list("/EntitySummaryList").len();
            assert_eq!(
                served, 0,
                "case {case:?}: a refused call serves no page, got {served} in {:?}",
                answer.body
            );
        }
    }
}

/// The accepted side of the same rule: a list of DISTINCT values in one of those fields
/// is served, as the page it selects from the persisted entities.
#[tokio::test]
async fn distinct_values_in_a_type_filter_documented_unique_select_from_state() {
    let router = emulator();
    let ids = created_entities(&router, &unique_type_fixtures()).await;

    // (case, entity type, field, the values, how many entities the page must carry)
    let rows: Vec<(&str, &str, &str, Vec<String>, usize)> = vec![
        (
            "a held id beside one naming nothing",
            "AmiProduct",
            "EntityId",
            vec![ids[0].clone(), UNHELD_ENTITY_ID.to_string()],
            1,
        ),
        (
            "an id naming nothing, alone",
            "SaaSProduct",
            "EntityId",
            vec![UNHELD_ENTITY_ID.to_string()],
            0,
        ),
        (
            "a held title beside one no entity carries",
            "ContainerProduct",
            "ProductTitle",
            vec!["container one".to_string(), UNHELD_TITLE.to_string()],
            1,
        ),
        (
            "both visibilities",
            "DataProduct",
            "Visibility",
            vec!["Limited".to_string(), "Draft".to_string()],
            1,
        ),
        (
            "the visibility no fixture entity carries",
            "DataProduct",
            "Visibility",
            vec!["Draft".to_string()],
            0,
        ),
    ];

    for (row, entity_type, member, values, expected) in rows {
        let case = format!("{entity_type}.{member}: {row}");
        let filters = json!({
            format!("{entity_type}Filters"): {member: {"ValueList": values}},
        });
        let answer = list_entities(&router, entity_type, Some(filters), None).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: distinct values must be served, got {} with {:?}",
            answer.status, answer.body
        );
        let served = answer.list("/EntitySummaryList").len();
        assert_eq!(
            served, expected,
            "case {case:?}: the filter must select {expected} entity(ies) from state, got \
             {served} in {:?}",
            answer.body
        );
    }
}

/// A `*Filters` field whose `ValueList` the model states nothing about takes a repeated
/// value, and still selects the entity it names — the same decision the generic surface
/// makes for the list documented non-unique.
#[tokio::test]
async fn a_repeated_value_is_served_where_the_model_states_no_uniqueness() {
    let router = emulator();
    let fixtures: Vec<(&str, Value)> = vec![
        (
            "MachineLearningProduct",
            json!({"ProductTitle": "ml one", "Visibility": "Limited"}),
        ),
        (
            "Offer",
            json!({"Name": "offer one", "ProductId": "prod-1", "State": "Released"}),
        ),
        (
            "OfferSet",
            json!({
                "Name": "set one",
                "State": "Released",
                "AssociatedOfferIds": ["offer-one"],
                "SolutionId": "solution-1",
            }),
        ),
        (
            "ResaleAuthorization",
            json!({
                "Name": "resale one",
                "ProductId": "prod-1",
                "Status": "Active",
                "ResellerAccountID": "444455556666",
            }),
        ),
        (
            "ExpressPrivateOfferConfiguration",
            json!({"ProductId": "prod-9", "State": "ACTIVE"}),
        ),
    ];
    let ids = created_entities(&router, &fixtures).await;

    // (case, entity type, field, the repeated values, how many entities the page carries)
    let rows: Vec<(&str, &str, &str, Vec<String>, usize)> = vec![
        (
            "a machine learning product's id twice",
            "MachineLearningProduct",
            "EntityId",
            vec![ids[0].clone(), ids[0].clone()],
            1,
        ),
        (
            "a machine learning product's title twice",
            "MachineLearningProduct",
            "ProductTitle",
            vec!["ml one".to_string(), "ml one".to_string()],
            1,
        ),
        (
            "a machine learning product's visibility twice",
            "MachineLearningProduct",
            "Visibility",
            vec!["Limited".to_string(), "Limited".to_string()],
            1,
        ),
        (
            "an offer's id twice",
            "Offer",
            "EntityId",
            vec![ids[1].clone(), ids[1].clone()],
            1,
        ),
        (
            "an offer's product id twice",
            "Offer",
            "ProductId",
            vec!["prod-1".to_string(), "prod-1".to_string()],
            1,
        ),
        (
            "an offer's state twice",
            "Offer",
            "State",
            vec!["Released".to_string(), "Released".to_string()],
            1,
        ),
        (
            "an offer set's name twice",
            "OfferSet",
            "Name",
            vec!["set one".to_string(), "set one".to_string()],
            1,
        ),
        (
            "an offer set's associated offer id twice",
            "OfferSet",
            "AssociatedOfferIds",
            vec!["offer-one".to_string(), "offer-one".to_string()],
            1,
        ),
        (
            "a resale authorization's status twice",
            "ResaleAuthorization",
            "Status",
            vec!["Active".to_string(), "Active".to_string()],
            1,
        ),
        (
            "an express private offer configuration's state twice",
            "ExpressPrivateOfferConfiguration",
            "State",
            vec!["ACTIVE".to_string(), "ACTIVE".to_string()],
            1,
        ),
    ];

    for (row, entity_type, member, values, expected) in rows {
        let case = format!("{entity_type}.{member}: {row}");
        let filters = json!({
            format!("{entity_type}Filters"): {member: {"ValueList": values}},
        });
        let answer = list_entities(&router, entity_type, Some(filters), None).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: a field the documentation states no uniqueness for must take the \
             repetition, got {} with {:?}",
            answer.status, answer.body
        );
        let served = answer.list("/EntitySummaryList").len();
        assert_eq!(
            served, expected,
            "case {case:?}: the filter must select {expected} entity(ies) from state, got \
             {served} in {:?}",
            answer.body
        );
    }
}

/// The two surfaces that filter one operation on an entity id answer the same repetition
/// the same way, and both serve the page when the ids are distinct.
#[tokio::test]
async fn both_filter_surfaces_judge_a_repeated_entity_id_alike() {
    let router = emulator();
    let ids = created_entities(
        &router,
        &[(
            "AmiProduct",
            json!({"ProductTitle": "ami one", "Visibility": "Limited"}),
        )],
    )
    .await;
    let held = ids[0].clone();

    // (case, EntityTypeFilters, FilterList) — the same value list on each surface.
    let repeated: Vec<(&str, Option<Value>, Option<Value>)> = vec![
        (
            "the generic FilterList",
            None,
            Some(json!([{"Name": "EntityId", "ValueList": [held.clone(), held.clone()]}])),
        ),
        (
            "the per-type EntityTypeFilters",
            Some(json!({"AmiProductFilters": {
                "EntityId": {"ValueList": [held.clone(), held.clone()]},
            }})),
            None,
        ),
    ];

    for (case, filters, filter_list) in repeated {
        let answer = list_entities(&router, "AmiProduct", filters, filter_list).await;
        assert_eq!(
            answer.status, 422,
            "case {case:?}: the same repeated entity id must be refused with 422 on either \
             surface, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "ValidationException",
            "case {case:?}: must answer ValidationException, got {:?}",
            answer.body
        );
        assert!(
            answer.message().contains(&held),
            "case {case:?}: the fault must quote the repeated id {held:?}, got {:?}",
            answer.message()
        );
    }

    // (case, EntityTypeFilters, FilterList) — distinct ids, which both surfaces serve.
    let distinct: Vec<(&str, Option<Value>, Option<Value>)> = vec![
        (
            "the generic FilterList",
            None,
            Some(json!([{"Name": "EntityId", "ValueList": [held.clone(), UNHELD_ENTITY_ID]}])),
        ),
        (
            "the per-type EntityTypeFilters",
            Some(json!({"AmiProductFilters": {
                "EntityId": {"ValueList": [held.clone(), UNHELD_ENTITY_ID]},
            }})),
            None,
        ),
    ];

    for (case, filters, filter_list) in distinct {
        let answer = list_entities(&router, "AmiProduct", filters, filter_list).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: distinct ids must be served on either surface, got {} with {:?}",
            answer.status, answer.body
        );
        let served = answer.list("/EntitySummaryList").len();
        assert_eq!(
            served, 1,
            "case {case:?}: the page must carry the one entity the held id names, got {served} \
             in {:?}",
            answer.body
        );
    }
}
