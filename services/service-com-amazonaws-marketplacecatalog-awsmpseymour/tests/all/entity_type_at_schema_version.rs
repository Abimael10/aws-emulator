// SPDX-License-Identifier: CC0-1.0

//! The SCHEMA VERSION an entity's type is reported at, end to end over HTTP.
//!
//! The rule under test: the two members the model documents as carrying the type "in the
//! format of `EntityType@Version`" — `DescribeEntityResponse$EntityType` and
//! `EntityDetail$EntityType` (`smithy_query(query="shape",
//! shape="DescribeEntityResponse,EntityDetail")`) — report the entity's type at the
//! schema version the applied change addressed it through, while every projection whose
//! member documents the type alone (`EntitySummary$EntityType`, and the type segment of
//! the entity ARN) keeps reporting the bare type.
//!
//! The rows differ along what the rule generalises over: the entity type (products and
//! types with no per-type shapes of their own, including types the issue's recording
//! never named) and the FORM of the version (a dotted one, a bare major, a three-part
//! one, a calendar one and one carrying letters), plus a second change that addresses the
//! same entity at another version.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_marketplacecatalog_awsmpseymour::state::PRODUCT_ENTITY_TYPES;
use service_com_amazonaws_marketplacecatalog_awsmpseymour::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The catalog every operation's `Catalog` member documents as its fixed value.
const CATALOG: &str = "AWSMarketplace";

/// A SigV4 credential scope naming this service's signing name (`aws-marketplace`).
const SIGNED_FOR_EU_WEST_1: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/aws-marketplace/aws4_request, \
     SignedHeaders=host, Signature=abc";

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

async fn describe_entity(router: &MultiServiceRouter, entity_id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/DescribeEntity?catalog={CATALOG}&entityId={entity_id}"),
        None,
    )
    .await
}

async fn batch_describe_entities(router: &MultiServiceRouter, entity_id: &str) -> Answer {
    call(
        router,
        "POST",
        "/BatchDescribeEntities",
        Some(json!({
            "EntityRequestList": [{"Catalog": CATALOG, "EntityId": entity_id}],
        })),
    )
    .await
}

async fn list_entities(router: &MultiServiceRouter, entity_type: &str) -> Answer {
    call(
        router,
        "POST",
        "/ListEntities",
        Some(json!({"Catalog": CATALOG, "EntityType": entity_type})),
    )
    .await
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

/// The details member an entity of `entity_type` carries its name in: the five product
/// types name it `ProductTitle` (`EntitySummary`'s `*ProductSummary` members), every
/// other type `Name`.
fn name_member(entity_type: &str) -> &'static str {
    if PRODUCT_ENTITY_TYPES.contains(&entity_type) {
        "ProductTitle"
    } else {
        "Name"
    }
}

/// Start one change of `change_type` against `reference` (an `EntityType@Version`),
/// naming `identifier` when the change acts on an entity that already exists, wait for
/// the clock to apply it, and answer the id of the entity it left.
async fn applied_change(
    router: &MultiServiceRouter,
    case: &str,
    change_type: &str,
    reference: &str,
    identifier: Option<&str>,
    title: &str,
) -> String {
    let entity_type = reference.split('@').next().unwrap_or_default();
    let mut entity = json!({"Type": reference});
    if let Some(identifier) = identifier {
        entity["Identifier"] = json!(identifier);
    }
    let mut details = serde_json::Map::new();
    details.insert(name_member(entity_type).to_string(), json!(title));
    let started = call(
        router,
        "POST",
        "/StartChangeSet",
        Some(json!({
            "Catalog": CATALOG,
            "ChangeSet": [{
                "ChangeType": change_type,
                "Entity": entity,
                "DetailsDocument": Value::Object(details),
            }],
        })),
    )
    .await;
    assert_eq!(
        started.status, 200,
        "case {case:?}: a change naming {reference:?} must be accepted, got {} with {:?}",
        started.status, started.body
    );
    let change_set_id = started.text("/ChangeSetId");
    let status = poll_until_terminal(router, &change_set_id).await;
    assert_eq!(
        status, "SUCCEEDED",
        "case {case:?}: the change set must succeed, got {status:?}"
    );

    let listed = call(
        router,
        "POST",
        "/ListChangeSets",
        Some(json!({"Catalog": CATALOG})),
    )
    .await;
    let ids: Vec<String> = listed
        .list("/ChangeSetSummaryList")
        .into_iter()
        .find(|summary| summary["ChangeSetId"] == json!(change_set_id))
        .and_then(|summary| summary["EntityIdList"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|id| id.as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        ids.len(),
        1,
        "case {case:?}: the change set must report the one entity it acted on, got {ids:?}"
    );
    ids[0].clone()
}

/// The two entity reads report the type at the schema version the change addressed it
/// through, and the projections documented as the type alone keep the bare type.
#[tokio::test]
async fn both_entity_reads_report_the_type_at_its_schema_version() {
    // (case, entity type, the version the change addresses it at)
    let rows = [
        ("a dotted version", "AmiProduct", "1.0"),
        ("a bare major version", "DataProduct", "2"),
        ("a three-part version", "SaaSProduct", "1.2.3"),
        // Types the issue's recording never named, one with per-type shapes of its own
        // and one with none.
        ("a calendar version", "ProcurementPolicy", "2024.01"),
        ("a version carrying letters", "OfferSet", "2beta"),
    ];

    for (case, entity_type, version) in rows {
        let router = emulator();
        let reference = format!("{entity_type}@{version}");
        let entity_id = applied_change(
            &router,
            case,
            "CreateProduct",
            &reference,
            None,
            "an entity at a schema version",
        )
        .await;

        let described = describe_entity(&router, &entity_id).await;
        assert_eq!(
            (described.status, described.text("/EntityType")),
            (200, reference.clone()),
            "case {case:?}: the single read must report the type at its schema version, \
             got {} with {:?}",
            described.status,
            described.body
        );

        let batch = batch_describe_entities(&router, &entity_id).await;
        assert_eq!(
            (
                batch.status,
                batch.text(&format!("/EntityDetails/{entity_id}/EntityType"))
            ),
            (200, reference.clone()),
            "case {case:?}: the batch read must report the same versioned type, got {} \
             with {:?}",
            batch.status,
            batch.body
        );

        // The ARN names the type alone: `ARN`'s `@pattern` (`^[a-zA-Z0-9:*/-]+$`) admits
        // no separator, and the member documents no version.
        let arn = described.text("/EntityArn");
        assert!(
            arn.ends_with(&format!("{CATALOG}/{entity_type}/{entity_id}")),
            "case {case:?}: the entity ARN must name the type alone, got {arn:?}"
        );

        // `EntitySummary$EntityType` documents "the type of the entity" with no version,
        // so the listed summary keeps the bare type — and the entity is listed under it.
        let listed = list_entities(&router, entity_type).await;
        let summaries = listed.list("/EntitySummaryList");
        assert_eq!(
            (listed.status, summaries.len()),
            (200, 1),
            "case {case:?}: the entity must be listed under {entity_type:?}, got {} with \
             {:?}",
            listed.status,
            listed.body
        );
        assert_eq!(
            (
                summaries[0]["EntityType"].as_str().unwrap_or_default(),
                summaries[0]["EntityId"].as_str().unwrap_or_default(),
            ),
            (entity_type, entity_id.as_str()),
            "case {case:?}: the summary must report the bare type, got {:?}",
            summaries[0]
        );
    }
}

/// The reported schema version follows the changes: an entity addressed again at another
/// version is read at THAT version, while its id, ARN and listed type are untouched.
#[tokio::test]
async fn a_later_change_moves_the_reported_schema_version() {
    // (case, entity type, the version the create names, the version the update names)
    let rows = [
        ("a major step", "AmiProduct", "1.0", "2.0"),
        ("a dotted step", "DataProduct", "1", "1.1"),
        ("back to an older version", "SaaSProduct", "2.0", "1.0"),
    ];

    for (case, entity_type, created_at, updated_at) in rows {
        let router = emulator();
        let entity_id = applied_change(
            &router,
            case,
            "CreateProduct",
            &format!("{entity_type}@{created_at}"),
            None,
            "an entity before the update",
        )
        .await;

        let described = describe_entity(&router, &entity_id).await;
        assert_eq!(
            described.text("/EntityType"),
            format!("{entity_type}@{created_at}"),
            "case {case:?}: the created entity must read at the version it was created \
             at, got {:?}",
            described.body
        );
        let arn = described.text("/EntityArn");

        let updated = applied_change(
            &router,
            case,
            "AddRevisions",
            &format!("{entity_type}@{updated_at}"),
            Some(&format!("{entity_id}@1")),
            "an entity after the update",
        )
        .await;
        assert_eq!(
            updated, entity_id,
            "case {case:?}: the update must act on the same entity, got {updated:?}"
        );

        let reread = describe_entity(&router, &entity_id).await;
        assert_eq!(
            (
                reread.text("/EntityType"),
                reread.text("/EntityIdentifier"),
                reread.text("/EntityArn"),
            ),
            (
                format!("{entity_type}@{updated_at}"),
                format!("{entity_id}@2"),
                arn.clone(),
            ),
            "case {case:?}: the entity must read at the version the later change named, \
             at its new revision and under the same ARN, got {:?}",
            reread.body
        );

        let batch = batch_describe_entities(&router, &entity_id).await;
        assert_eq!(
            batch.text(&format!("/EntityDetails/{entity_id}/EntityType")),
            format!("{entity_type}@{updated_at}"),
            "case {case:?}: the batch read must move with the single one, got {:?}",
            batch.body
        );

        let listed = list_entities(&router, entity_type).await;
        let summaries = listed.list("/EntitySummaryList");
        assert_eq!(
            summaries
                .first()
                .and_then(|summary| summary["EntityType"].as_str())
                .unwrap_or_default(),
            entity_type,
            "case {case:?}: the listed summary must keep the bare type, got {:?}",
            listed.body
        );
    }
}
