// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the FLEET INDEX's MANAGED fields and of the bucket
//! aggregation computed over them: `CreateFleetMetric` / `UpdateFleetMetric` over a
//! managed field, and `GetBucketsAggregation` over the things a query selects.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what an
//! AWS SDK client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * a field the fleet-indexing service MANAGES is aggregatable as soon as the index
//!   that holds it exists: only a field whose values a CALLER supplies
//!   (`attributes.<name>`) has to be declared through `UpdateIndexingConfiguration`
//!   before an aggregation may name it;
//! * a bucket aggregation is COMPUTED over the account's own registry: the query selects
//!   the things, `totalCount` counts them and each bucket counts the things that carry
//!   one value of the aggregated field, ranked by that count;
//! * an aggregation over an index the account does not have is
//!   `ResourceNotFoundException` (404) and one over an index still building is
//!   `IndexNotReadyException` (400);
//! * what ONE aggregation may cost is bounded in both dimensions a request chooses —
//!   how many documents the scan examines and how much character matching the query
//!   performs over them — and past either bound the request is REFUSED rather than
//!   answered from a partial scan;
//! * `GetStatistics` summarises one field of the things a query selects: the COUNT of
//!   the selected things that carry a valid value for the aggregation field, plus the
//!   arithmetic summary of those values where the index carries the field as a NUMBER
//!   and nothing more where it does not;
//! * `GetCardinality` counts the DISTINCT values one field holds across the things a
//!   query selects, so a repeated value counts once and a request naming no field counts
//!   the selected records themselves;
//! * `GetPercentiles` positions the percentile groups it is asked for — the default
//!   groups where the request names none — in the distribution of ONE numeric field's
//!   values across the things a query selects, interpolating between the two neighbouring
//!   values where a group's position falls between them.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    documents_matching, fleet_metrics::MANAGED_THING_FIELDS, ComAmazonawsIotAwsiotserviceState,
    IndexedDocument, ScanRefusal, INDEX_BUILD_SPAN, MAX_SCANNED_DOCUMENTS, MAX_WALK_DEPTH,
    THING_INDEX_NAME,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for.
const REGION: &str = "eu-north-1";

/// A custom field of type `String` the account declares, so a case can show that a
/// CALLER-supplied field still needs the declaration a managed one does not.
const DECLARED_ATTRIBUTE_FIELD: &str = "attributes.zone";

/// An attribute field the account declares nothing for.
const UNDECLARED_ATTRIBUTE_FIELD: &str = "attributes.undeclared";

/// How far out [`Emulator::hold_index_build`] pushes an index build's due instant.
///
/// A case about the BUILDING state asserts what a caller finds while the build is still
/// owed, so the window has to be wider than anything the host's scheduling can consume
/// between two calls — generous on purpose, since no case waits for it to pass.
const HELD_BUILD: Duration = Duration::from_secs(3_600);

/// One emulator: the shared engine plus a router with this service mounted.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
    }

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the fleet-indexing request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    /// Turn REGISTRY thing indexing on, declaring one custom attribute field.
    async fn enable_thing_index(&self, case: &str) {
        let reply = self
            .dispatch(
                "POST",
                "/indexing/config",
                Some(&json!({
                    "thingIndexingConfiguration": {
                        "thingIndexingMode": "REGISTRY",
                        "customFields": [{ "name": DECLARED_ATTRIBUTE_FIELD, "type": "String" }],
                    }
                })),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected UpdateIndexingConfiguration to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    /// Finish the build the clock owes the thing index, by moving the recorded due
    /// instant into the past — the state a caller that waited out [`INDEX_BUILD_SPAN`]
    /// would find, without the test waiting for it.
    fn finish_index_build(&self) {
        let state = self.engine.state::<ComAmazonawsIotAwsiotserviceState>();
        let mut store = state.lock();
        let build = store
            .index_builds
            .get_mut(THING_INDEX_NAME)
            .expect("enabling the index records a build");
        build.active_at = SystemTime::now() - INDEX_BUILD_SPAN;
    }

    /// Hold the thing index in the build the clock owes it, by pushing the recorded due
    /// instant out by [`HELD_BUILD`] — the BUILDING state a caller finds straight after
    /// the configuration change, with no case depending on how fast the host got from
    /// one call to the next.
    fn hold_index_build(&self) {
        let state = self.engine.state::<ComAmazonawsIotAwsiotserviceState>();
        let mut store = state.lock();
        let build = store
            .index_builds
            .get_mut(THING_INDEX_NAME)
            .expect("enabling the index records a build");
        build.active_at = SystemTime::now() + HELD_BUILD;
    }

    /// An emulator whose thing index exists and is ACTIVE.
    async fn with_active_index(case: &str) -> Self {
        let emulator = Self::new();
        emulator.enable_thing_index(case).await;
        emulator.finish_index_build();
        emulator
    }

    async fn create_thing_type(&self, case: &str, thing_type_name: &str) {
        let reply = self
            .dispatch(
                "POST",
                &format!("/thing-types/{thing_type_name}"),
                Some(&json!({})),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected CreateThingType to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    /// Register one thing of the type given, carrying the attributes given as
    /// `(name, value)` pairs.
    async fn create_thing(
        &self,
        case: &str,
        thing_name: &str,
        thing_type_name: Option<&str>,
        attributes: &[(&str, &str)],
    ) {
        let mut body = json!({});
        if let Some(thing_type_name) = thing_type_name {
            body["thingTypeName"] = json!(thing_type_name);
        }
        if !attributes.is_empty() {
            let mut held = serde_json::Map::new();
            for (name, value) in attributes {
                held.insert((*name).to_string(), json!(value));
            }
            body["attributePayload"] = json!({ "attributes": Value::Object(held) });
        }
        let reply = self
            .dispatch("POST", &format!("/things/{thing_name}"), Some(&body))
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected CreateThing to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    async fn create_metric(&self, metric_name: &str, body: &Value) -> Reply {
        self.dispatch("PUT", &format!("/fleet-metric/{metric_name}"), Some(body))
            .await
    }

    async fn update_metric(&self, metric_name: &str, body: &Value) -> Reply {
        self.dispatch("PATCH", &format!("/fleet-metric/{metric_name}"), Some(body))
            .await
    }

    async fn describe_metric(&self, metric_name: &str) -> Reply {
        self.dispatch("GET", &format!("/fleet-metric/{metric_name}"), None)
            .await
    }

    async fn buckets(&self, body: &Value) -> Reply {
        self.dispatch("POST", "/indices/buckets", Some(body)).await
    }

    async fn list_metrics(&self, query: &str) -> Reply {
        self.dispatch("GET", &format!("/fleet-metrics?{query}"), None)
            .await
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: response.into_body(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.text()))
    }

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    /// The `(metricName, metricArn)` pairs a `ListFleetMetrics` page carries.
    fn fleet_metrics(&self) -> Vec<(String, String)> {
        self.json()["fleetMetrics"]
            .as_array()
            .unwrap_or_else(|| panic!("no fleetMetrics in {}", self.text()))
            .iter()
            .map(|metric| {
                (
                    metric["metricName"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    metric["metricArn"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    /// The `(keyValue, count)` pairs a bucket aggregation answered with, in the order it
    /// ranked them.
    fn buckets(&self) -> Vec<(String, i64)> {
        self.json()["buckets"]
            .as_array()
            .unwrap_or_else(|| panic!("no buckets in {}", self.text()))
            .iter()
            .map(|bucket| {
                (
                    bucket["keyValue"].as_str().unwrap_or_default().to_string(),
                    bucket["count"].as_i64().unwrap_or_default(),
                )
            })
            .collect()
    }
}

/// RULE: every field the fleet-indexing service manages is aggregatable without the
/// account declaring anything, and only a caller-supplied `attributes.<name>` field
/// needs a `customFields` declaration.
#[tokio::test]
async fn a_managed_index_field_is_aggregatable_without_being_declared() {
    let emulator = Emulator::with_active_index("managed fields").await;

    // The rows are the index's OWN managed-field catalog
    // (`service_com_amazonaws_iot_awsiotservice::state::fleet_metrics::MANAGED_THING_FIELDS`,
    // the managed-field table AWS IoT fleet indexing publishes), so a field added to or
    // dropped from it is covered here without this table being rewritten. The `count`
    // statistic is the one aggregation every field type admits — the arithmetic
    // summaries are of numbers and a cardinality is not asked of a flag — so what each
    // row varies is the FIELD and nothing else.
    for (index, (field, _field_type)) in MANAGED_THING_FIELDS.iter().enumerate() {
        let metric_name = format!("managed-{index}");
        let created = emulator
            .create_metric(
                &metric_name,
                &json!({
                    "queryString": "thingName:*",
                    "aggregationType": { "name": "Statistics", "values": ["count"] },
                    "aggregationField": field,
                    "period": 300,
                }),
            )
            .await;
        assert_eq!(
            created.status,
            200,
            "case managed field {field}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
        // The created metric reads back with the field it aggregates, so the accept is
        // backed by a record and not by a bare 200.
        let read = emulator.describe_metric(&metric_name).await;
        assert_eq!(
            read.json()["aggregationField"],
            json!(field),
            "case managed field {field}: the read must report the stored field, got {}",
            read.text()
        );
    }

    // A field the account DECLARED is aggregatable too...
    let declared = emulator
        .create_metric(
            "declared",
            &json!({
                "queryString": "thingName:*",
                "aggregationType": { "name": "Cardinality" },
                "aggregationField": DECLARED_ATTRIBUTE_FIELD,
                "period": 300,
            }),
        )
        .await;
    assert_eq!(
        declared.status,
        200,
        "case a declared custom field: expected 200, got {} body {}",
        declared.status,
        declared.text()
    );
    // ...and one it declared nothing for is refused, which is what makes the accepts
    // above about the managed catalog rather than about every field being allowed.
    let undeclared = emulator
        .create_metric(
            "undeclared",
            &json!({
                "queryString": "thingName:*",
                "aggregationType": { "name": "Cardinality" },
                "aggregationField": UNDECLARED_ATTRIBUTE_FIELD,
                "period": 300,
            }),
        )
        .await;
    assert_eq!(
        undeclared.status,
        400,
        "case an undeclared attribute field: expected 400, got {} body {}",
        undeclared.status,
        undeclared.text()
    );
    assert_eq!(
        undeclared.error_code(),
        "InvalidRequestException",
        "case an undeclared attribute field: got {}",
        undeclared.text()
    );

    // The same rule holds for an UPDATE of a metric's aggregation: the managed field is
    // accepted and the undeclared one refused, with the record left at what it was.
    let updated = emulator
        .update_metric(
            "managed-0",
            &json!({
                "indexName": THING_INDEX_NAME,
                "aggregationField": "registry.thingTypeName",
                "aggregationType": { "name": "Cardinality" },
            }),
        )
        .await;
    assert_eq!(
        updated.status,
        200,
        "case an update to a managed field: expected 200, got {} body {}",
        updated.status,
        updated.text()
    );
    let read = emulator.describe_metric("managed-0").await;
    assert_eq!(
        read.json()["aggregationField"],
        json!("registry.thingTypeName"),
        "case an update to a managed field: the read must report it, got {}",
        read.text()
    );
    let refused = emulator
        .update_metric(
            "managed-0",
            &json!({
                "indexName": THING_INDEX_NAME,
                "aggregationField": UNDECLARED_ATTRIBUTE_FIELD,
                "aggregationType": { "name": "Cardinality" },
            }),
        )
        .await;
    assert_eq!(
        refused.status,
        400,
        "case an update to an undeclared attribute field: expected 400, got {} body {}",
        refused.status,
        refused.text()
    );
    let read = emulator.describe_metric("managed-0").await;
    assert_eq!(
        read.json()["aggregationField"],
        json!("registry.thingTypeName"),
        "case an update to an undeclared attribute field: the record must be unchanged, got {}",
        read.text()
    );

    // Every metric the creates above recorded is reported by the listing, with the ARN
    // its own create minted — and the listing pages over them, a page at a time.
    let listed = emulator.list_metrics("").await;
    assert_eq!(
        listed.status,
        200,
        "case the listing: expected 200, got {} body {}",
        listed.status,
        listed.text()
    );
    let expected_count = MANAGED_THING_FIELDS.len() + 1;
    assert_eq!(
        listed.fleet_metrics().len(),
        expected_count,
        "case the listing: expected {expected_count} metrics, got {:?}",
        listed.fleet_metrics()
    );
    for (metric_name, metric_arn) in listed.fleet_metrics() {
        let read = emulator.describe_metric(&metric_name).await;
        assert_eq!(
            read.json()["metricArn"],
            json!(metric_arn),
            "case the listing: expected the ARN the create minted for {metric_name}, got {}",
            read.text()
        );
    }
    let mut paged: Vec<String> = Vec::new();
    let mut query = "maxResults=2".to_string();
    loop {
        let page = emulator.list_metrics(&query).await;
        let names: Vec<String> = page
            .fleet_metrics()
            .into_iter()
            .map(|(metric_name, _arn)| metric_name)
            .collect();
        assert!(
            names.len() <= 2,
            "case the paged listing: a page must hold at most 2, got {names:?}"
        );
        paged.extend(names);
        match page.json()["nextToken"].as_str() {
            Some(token) => query = format!("maxResults=2&nextToken={token}"),
            None => break,
        }
    }
    assert_eq!(
        paged.len(),
        expected_count,
        "case the paged listing: expected every metric once, got {paged:?}"
    );
}

/// RULE: a bucket aggregation is computed over the things the query selects, counting
/// the things that carry each value of the aggregated field.
#[tokio::test]
async fn a_bucket_aggregation_counts_the_registry_the_query_selects() {
    let emulator = Emulator::with_active_index("buckets").await;
    for thing_type_name in ["sensor", "gateway"] {
        emulator.create_thing_type("buckets", thing_type_name).await;
    }
    // Three things of one type, two of another and one of none: the counts the
    // aggregation has to report are a property of this registry.
    emulator
        .create_thing("buckets", "sensor-1", Some("sensor"), &[("zone", "north")])
        .await;
    emulator
        .create_thing("buckets", "sensor-2", Some("sensor"), &[("zone", "north")])
        .await;
    emulator
        .create_thing("buckets", "sensor-3", Some("sensor"), &[("zone", "south")])
        .await;
    emulator
        .create_thing("buckets", "gateway-1", Some("gateway"), &[("zone", "south")])
        .await;
    emulator
        .create_thing("buckets", "gateway-2", Some("gateway"), &[])
        .await;
    emulator.create_thing("buckets", "spare-1", None, &[]).await;

    // The rows span what the rule generalises over: which field is aggregated (a managed
    // registry field and a declared attribute field), which things the query selects
    // (all of them, a wildcard over the name, a value match, and one that selects
    // none), and how many buckets the request asks for.
    struct Case {
        label: &'static str,
        query: &'static str,
        field: &'static str,
        max_buckets: Option<i64>,
        total: i64,
        buckets: Vec<(&'static str, i64)>,
    }
    let cases = vec![
        Case {
            label: "the type of every thing",
            query: "thingName:*",
            field: "registry.thingTypeName",
            max_buckets: None,
            total: 6,
            buckets: vec![("sensor", 3), ("gateway", 2)],
        },
        Case {
            label: "the type of the things one name wildcard selects",
            query: "thingName:sensor-*",
            field: "registry.thingTypeName",
            max_buckets: None,
            total: 3,
            buckets: vec![("sensor", 3)],
        },
        Case {
            label: "a declared attribute field over every thing",
            query: "thingName:*",
            field: DECLARED_ATTRIBUTE_FIELD,
            max_buckets: None,
            total: 6,
            buckets: vec![("north", 2), ("south", 2)],
        },
        Case {
            label: "the type of the things one attribute value selects",
            query: "attributes.zone:south",
            field: "registry.thingTypeName",
            max_buckets: None,
            total: 2,
            buckets: vec![("gateway", 1), ("sensor", 1)],
        },
        Case {
            label: "only the highest-ranked bucket",
            query: "thingName:*",
            field: "registry.thingTypeName",
            max_buckets: Some(1),
            total: 6,
            buckets: vec![("sensor", 3)],
        },
        Case {
            label: "a query that selects no thing at all",
            query: "thingName:absent-*",
            field: "registry.thingTypeName",
            max_buckets: None,
            total: 0,
            buckets: vec![],
        },
        Case {
            label: "the name of every thing, one bucket each",
            query: "thingName:* AND NOT thingName:spare-1",
            field: "thingName",
            max_buckets: Some(10),
            total: 5,
            buckets: vec![
                ("gateway-1", 1),
                ("gateway-2", 1),
                ("sensor-1", 1),
                ("sensor-2", 1),
                ("sensor-3", 1),
            ],
        },
    ];

    for case in &cases {
        let mut terms = json!({});
        if let Some(max_buckets) = case.max_buckets {
            terms["maxBuckets"] = json!(max_buckets);
        }
        let answer = emulator
            .buckets(&json!({
                "queryString": case.query,
                "aggregationField": case.field,
                "bucketsAggregationType": { "termsAggregation": terms },
            }))
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {}: expected 200, got {} body {}",
            case.label,
            answer.status,
            answer.text()
        );
        assert_eq!(
            answer.json()["totalCount"],
            json!(case.total),
            "case {}: expected {} things selected, got {}",
            case.label,
            case.total,
            answer.text()
        );
        let expected: Vec<(String, i64)> = case
            .buckets
            .iter()
            .map(|(value, count)| ((*value).to_string(), *count))
            .collect();
        assert_eq!(
            answer.buckets(),
            expected,
            "case {}: expected the buckets {expected:?}, got {:?}",
            case.label,
            answer.buckets()
        );
    }
}

/// RULE: an aggregation is answered out of the INDEX, so an account that has none is a
/// 404 and an index still building is a 400 — and the same calls succeed once it is
/// ACTIVE.
#[tokio::test]
async fn an_aggregation_over_an_absent_or_building_index_is_refused() {
    let request = json!({
        "queryString": "thingName:*",
        "aggregationField": "registry.version",
        "bucketsAggregationType": { "termsAggregation": { "maxBuckets": 5 } },
    });

    // No indexing configured at all: the index does not exist, so there is nothing to
    // aggregate.
    let emulator = Emulator::new();
    let refused = emulator.buckets(&request).await;
    assert_eq!(
        refused.status,
        404,
        "case thing indexing OFF: expected 404, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_code(),
        "ResourceNotFoundException",
        "case thing indexing OFF: got {}",
        refused.text()
    );

    // Turned on, the index exists but is still building until the clock makes it active.
    emulator.enable_thing_index("building").await;
    emulator.hold_index_build();
    let building = emulator.buckets(&request).await;
    assert_eq!(
        building.status,
        400,
        "case the index still building: expected 400, got {} body {}",
        building.status,
        building.text()
    );
    assert_eq!(
        building.error_code(),
        "IndexNotReadyException",
        "case the index still building: got {}",
        building.text()
    );

    // Once the build the clock owed is due, the same request is answered.
    emulator.finish_index_build();
    let answered = emulator.buckets(&request).await;
    assert_eq!(
        answered.status,
        200,
        "case the index ACTIVE: expected 200, got {} body {}",
        answered.status,
        answered.text()
    );

    // The index that is no fleet index of this service at all, and a request naming no
    // bucket aggregation, are each refused on their own terms.
    let unknown_index = emulator
        .buckets(&json!({
            "indexName": "AWS_Nothing",
            "queryString": "thingName:*",
            "aggregationField": "registry.version",
            "bucketsAggregationType": { "termsAggregation": { "maxBuckets": 5 } },
        }))
        .await;
    assert_eq!(
        unknown_index.error_code(),
        "InvalidRequestException",
        "case an index name this service does not serve: got {}",
        unknown_index.text()
    );
    let no_aggregation = emulator
        .buckets(&json!({
            "queryString": "thingName:*",
            "aggregationField": "registry.version",
            "bucketsAggregationType": {},
        }))
        .await;
    assert_eq!(
        no_aggregation.error_code(),
        "InvalidAggregationException",
        "case a request naming no bucket aggregation: got {}",
        no_aggregation.text()
    );
    let bad_query = emulator
        .buckets(&json!({
            "queryString": "thingName:(",
            "aggregationField": "registry.version",
            "bucketsAggregationType": { "termsAggregation": { "maxBuckets": 5 } },
        }))
        .await;
    assert_eq!(
        bad_query.error_code(),
        "InvalidQueryException",
        "case a query string that is not a query: got {}",
        bad_query.text()
    );
}

/// RULE: what ONE aggregation may cost is bounded in both dimensions a request chooses,
/// and past either bound the scan is refused rather than answered from a partial read.
///
/// The bounds are exercised on the scan itself: an index holding more documents than one
/// scan examines, and a query whose matching costs more than one aggregation evaluates,
/// are each far too large to drive through the registry's own API in a test.
#[test]
fn an_aggregation_past_either_bound_is_refused() {
    /// One document carrying a long value, which is what a wildcard term is matched
    /// against character by character.
    fn document(index: usize) -> IndexedDocument {
        let mut document = IndexedDocument::new();
        document.insert(
            "thingName".to_string(),
            vec![format!("thing-{index}-{}", "v".repeat(800))],
        );
        document
    }

    // One document past the number a scan examines is refused naming how many there are.
    let too_many: Vec<IndexedDocument> = (0..=MAX_SCANNED_DOCUMENTS)
        .map(|index| {
            let mut document = IndexedDocument::new();
            document.insert("thingName".to_string(), vec![format!("thing-{index}")]);
            document
        })
        .collect();
    let refused = documents_matching(too_many, "thingName:*");
    assert_eq!(
        refused.err(),
        Some(ScanRefusal::TooManyDocuments(MAX_SCANNED_DOCUMENTS + 1)),
        "case one document past the scan's ceiling: expected the documents refusal"
    );

    // A query whose matching costs more than one aggregation evaluates is refused too: a
    // wildcard pattern of the length the query reader admits, against long values.
    let wide: Vec<IndexedDocument> = (0..200).map(document).collect();
    let pattern = format!("thingName:{}*", "a".repeat(2_000));
    let refused = documents_matching(wide.clone(), &pattern);
    assert_eq!(
        refused.err(),
        Some(ScanRefusal::TooMuchMatching),
        "case a long wildcard term over many documents: expected the matching refusal"
    );

    // The same documents under an ordinary query are scanned, so the bound above is
    // about the cost the request chose and not about the store's size.
    let selected = documents_matching(wide.clone(), "thingName:thing-7-*");
    assert_eq!(
        selected.map(|selected| selected.len()),
        Ok(1),
        "case an ordinary query over the same documents: expected the one thing it selects"
    );

    // A query that nests deeper than the walk descends is refused before any document is
    // examined, so the recursion cannot run the scan thread's stack out. A run of
    // negations is the form that reaches the walk's ceiling: the query reader caps how
    // deeply PARENTHESES nest, well below it, so that form never gets this far.
    let negations = format!("{}thingName:*", "NOT ".repeat(MAX_WALK_DEPTH as usize + 2));
    let refused = documents_matching(wide.clone(), &negations);
    assert_eq!(
        refused.err(),
        Some(ScanRefusal::TooDeeplyNested),
        "case a run of negations past the walk's ceiling: expected the nesting refusal"
    );
    // The parenthesised form is not a query this service reads at all, so it selects
    // nothing rather than descending: every caller refuses it before the scan.
    let groups = format!(
        "{}thingName:*{}",
        "(".repeat(MAX_WALK_DEPTH as usize + 2),
        ")".repeat(MAX_WALK_DEPTH as usize + 2)
    );
    assert_eq!(
        documents_matching(wide.clone(), &groups).map(|selected| selected.len()),
        Ok(0),
        "case parentheses past the query reader's ceiling: expected nothing selected"
    );

    // A query that nests within the ceiling is evaluated, so the bound is on the depth
    // the request chose rather than on nesting at all.
    let nested = documents_matching(wide, "NOT (NOT (thingName:thing-7-*))");
    assert_eq!(
        nested.map(|selected| selected.len()),
        Ok(1),
        "case a query nesting within the ceiling: expected the one thing it selects"
    );
}

/// A custom field the account declares as type `Number`, so a case can show the
/// arithmetic statistics a NUMERIC field has and a text one does not. `Number` is one of
/// the modelled `FieldType` values
/// (`smithy_query(query="shape",shape="FieldType")`: `Number`, `String`, `Boolean`).
const NUMERIC_ATTRIBUTE_FIELD: &str = "attributes.temperature";

/// The attribute whose values [`NUMERIC_ATTRIBUTE_FIELD`] indexes, as `CreateThing`
/// names it: the field is the attribute under the index's `attributes.` prefix.
const NUMERIC_ATTRIBUTE: &str = "temperature";

/// How far a computed statistic may be from the expected one: the arithmetic runs in
/// `f64`, so a sum of squares or a variance is compared to within a rounding error of
/// the figure's own size rather than bit for bit.
const TOLERANCE: f64 = 1e-9;

impl Emulator {
    /// An emulator whose thing index exists, is ACTIVE and carries BOTH a text and a
    /// numeric custom field, so one registry can be asked for the statistics of each.
    async fn with_numeric_custom_field(case: &str) -> Self {
        let emulator = Self::new();
        let reply = emulator
            .dispatch(
                "POST",
                "/indexing/config",
                Some(&json!({
                    "thingIndexingConfiguration": {
                        "thingIndexingMode": "REGISTRY",
                        "customFields": [
                            { "name": DECLARED_ATTRIBUTE_FIELD, "type": "String" },
                            { "name": NUMERIC_ATTRIBUTE_FIELD, "type": "Number" },
                        ],
                    }
                })),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected UpdateIndexingConfiguration to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        emulator.finish_index_build();
        emulator
    }

    async fn statistics(&self, body: &Value) -> Reply {
        self.dispatch("POST", "/indices/statistics", Some(body))
            .await
    }
}

impl Reply {
    /// The `statistics` a `GetStatistics` answer carries.
    fn statistics(&self) -> Value {
        self.json()["statistics"].clone()
    }
}

/// The arithmetic members of `Statistics` a NUMERIC field is summarised by, each named as
/// the wire names it.
const ARITHMETIC_STATISTICS: &[&str] = &[
    "average",
    "sum",
    "minimum",
    "maximum",
    "sumOfSquares",
    "variance",
    "stdDeviation",
];

/// What the arithmetic summary of one case's values is, member for member.
struct ExpectedSummary {
    sum: f64,
    average: f64,
    minimum: f64,
    maximum: f64,
    sum_of_squares: f64,
    variance: f64,
    std_deviation: f64,
}

impl ExpectedSummary {
    /// The summary as `(wire member, expected value)` pairs, in the order
    /// [`ARITHMETIC_STATISTICS`] names them.
    fn members(&self) -> Vec<(&'static str, f64)> {
        vec![
            ("average", self.average),
            ("sum", self.sum),
            ("minimum", self.minimum),
            ("maximum", self.maximum),
            ("sumOfSquares", self.sum_of_squares),
            ("variance", self.variance),
            ("stdDeviation", self.std_deviation),
        ]
    }
}

/// RULE: `GetStatistics` is computed over the things the query selects — the count of
/// those carrying a valid value for the aggregation field, and the arithmetic summary of
/// those values exactly where the index carries the field as a number.
#[tokio::test]
async fn statistics_summarise_the_field_of_the_things_the_query_selects() {
    let emulator = Emulator::with_numeric_custom_field("statistics").await;
    for thing_type_name in ["sensor", "gateway"] {
        emulator.create_thing_type("statistics", thing_type_name).await;
    }
    // Four things carrying a numeric attribute, one carrying none and one with no type
    // and no attributes: every figure asserted below is a property of THIS registry.
    for (thing_name, thing_type_name, zone, temperature) in [
        ("sensor-1", Some("sensor"), "north", Some("10")),
        ("sensor-2", Some("sensor"), "north", Some("20")),
        ("sensor-3", Some("sensor"), "south", Some("30")),
        ("gateway-1", Some("gateway"), "south", Some("40")),
    ] {
        let mut attributes = vec![("zone", zone)];
        if let Some(temperature) = temperature {
            attributes.push((NUMERIC_ATTRIBUTE, temperature));
        }
        emulator
            .create_thing("statistics", thing_name, thing_type_name, &attributes)
            .await;
    }
    emulator
        .create_thing("statistics", "gateway-2", Some("gateway"), &[])
        .await;
    emulator
        .create_thing("statistics", "spare-1", None, &[])
        .await;

    /// One request and what this registry answers it with.
    struct Case {
        label: &'static str,
        query: &'static str,
        /// The `aggregationField`, or `None` for a request that names none.
        field: Option<&'static str>,
        count: i64,
        /// The arithmetic summary, or `None` for a field that has none.
        summary: Option<ExpectedSummary>,
    }
    // The rows differ along what the rule generalises over: which field is summarised (a
    // numeric custom field, a numeric MANAGED field, a text managed field, a text custom
    // field, and no field at all), which things the query selects (all of them, a name
    // wildcard, an attribute value, exactly one, and none), and how many values the
    // selection holds (several, one, none).
    let cases = vec![
        Case {
            label: "a numeric custom field over every thing",
            query: "thingName:*",
            field: Some(NUMERIC_ATTRIBUTE_FIELD),
            // Only the four things that CARRY the field are counted; 10, 20, 30, 40.
            count: 4,
            summary: Some(ExpectedSummary {
                sum: 100.0,
                average: 25.0,
                minimum: 10.0,
                maximum: 40.0,
                sum_of_squares: 3_000.0,
                variance: 125.0,
                std_deviation: 125.0_f64.sqrt(),
            }),
        },
        Case {
            label: "a numeric custom field over the things one name wildcard selects",
            query: "thingName:sensor-*",
            field: Some(NUMERIC_ATTRIBUTE_FIELD),
            // 10, 20, 30.
            count: 3,
            summary: Some(ExpectedSummary {
                sum: 60.0,
                average: 20.0,
                minimum: 10.0,
                maximum: 30.0,
                sum_of_squares: 1_400.0,
                variance: 1_400.0 / 3.0 - 400.0,
                std_deviation: (1_400.0 / 3.0 - 400.0_f64).sqrt(),
            }),
        },
        Case {
            label: "a numeric custom field over the things one attribute value selects",
            query: "attributes.zone:south",
            field: Some(NUMERIC_ATTRIBUTE_FIELD),
            // 30, 40.
            count: 2,
            summary: Some(ExpectedSummary {
                sum: 70.0,
                average: 35.0,
                minimum: 30.0,
                maximum: 40.0,
                sum_of_squares: 2_500.0,
                variance: 25.0,
                std_deviation: 5.0,
            }),
        },
        Case {
            label: "a numeric custom field over exactly one thing",
            query: "thingName:sensor-1",
            field: Some(NUMERIC_ATTRIBUTE_FIELD),
            count: 1,
            summary: Some(ExpectedSummary {
                sum: 10.0,
                average: 10.0,
                minimum: 10.0,
                maximum: 10.0,
                sum_of_squares: 100.0,
                variance: 0.0,
                std_deviation: 0.0,
            }),
        },
        Case {
            label: "a numeric MANAGED field over every thing",
            query: "thingName:*",
            field: Some("registry.version"),
            // Every thing carries its registry version, which a create leaves at 1.
            count: 6,
            summary: Some(ExpectedSummary {
                sum: 6.0,
                average: 1.0,
                minimum: 1.0,
                maximum: 1.0,
                sum_of_squares: 6.0,
                variance: 0.0,
                std_deviation: 0.0,
            }),
        },
        Case {
            label: "a numeric custom field no selected thing carries",
            query: "thingName:spare-1",
            field: Some(NUMERIC_ATTRIBUTE_FIELD),
            count: 0,
            summary: None,
        },
        Case {
            label: "a numeric custom field over a query that selects no thing",
            query: "thingName:absent-*",
            field: Some(NUMERIC_ATTRIBUTE_FIELD),
            count: 0,
            summary: None,
        },
        Case {
            label: "a TEXT managed field over every thing",
            query: "thingName:*",
            // Five things have a thing type; the sixth carries no value for the field.
            field: Some("registry.thingTypeName"),
            count: 5,
            summary: None,
        },
        Case {
            label: "a TEXT custom field over every thing",
            query: "thingName:*",
            field: Some(DECLARED_ATTRIBUTE_FIELD),
            count: 4,
            summary: None,
        },
        Case {
            label: "a request naming no aggregation field",
            query: "thingName:*",
            field: None,
            count: 6,
            summary: None,
        },
    ];

    for case in &cases {
        let mut request = json!({ "queryString": case.query });
        if let Some(field) = case.field {
            request["aggregationField"] = json!(field);
        }
        let answer = emulator.statistics(&request).await;
        assert_eq!(
            answer.status,
            200,
            "case {}: expected 200, got {} body {}",
            case.label,
            answer.status,
            answer.text()
        );
        let statistics = answer.statistics();
        assert_eq!(
            statistics["count"],
            json!(case.count),
            "case {}: expected count {}, got {statistics}",
            case.label,
            case.count
        );
        match case.summary.as_ref() {
            Some(expected) => {
                for (member, value) in expected.members() {
                    let actual = statistics[member].as_f64();
                    let within = actual.is_some_and(|actual| {
                        (actual - value).abs() <= TOLERANCE * value.abs().max(1.0)
                    });
                    assert!(
                        within,
                        "case {}: expected {member} {value}, got {:?}",
                        case.label, statistics[member]
                    );
                }
            }
            // A field with no numbers to summarise reports its count ALONE: a zero
            // reported for a sum nothing added up would read as a computed figure.
            None => {
                for member in ARITHMETIC_STATISTICS {
                    assert!(
                        statistics[member].is_null(),
                        "case {}: expected no {member}, got {}",
                        case.label,
                        statistics[member]
                    );
                }
            }
        }
    }
}

/// RULE: the statistics are read out of the INDEX and over a field it carries, so the
/// index's own state and the request's field decide the refusal — and the same request is
/// answered once the index is ACTIVE.
#[tokio::test]
async fn statistics_over_an_absent_index_or_an_unindexed_field_are_refused() {
    let request = json!({
        "queryString": "thingName:*",
        "aggregationField": "registry.version",
    });

    // No indexing configured at all: there is no index to read statistics out of.
    let emulator = Emulator::new();
    let refused = emulator.statistics(&request).await;
    assert_eq!(
        refused.status,
        404,
        "case thing indexing OFF: expected 404, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_code(),
        "ResourceNotFoundException",
        "case thing indexing OFF: got {}",
        refused.text()
    );

    // Turned on, the index exists but is still building until the clock makes it active.
    emulator.enable_thing_index("statistics while building").await;
    emulator.hold_index_build();
    let building = emulator.statistics(&request).await;
    assert_eq!(
        building.status,
        400,
        "case the index still building: expected 400, got {} body {}",
        building.status,
        building.text()
    );
    assert_eq!(
        building.error_code(),
        "IndexNotReadyException",
        "case the index still building: got {}",
        building.text()
    );

    // Once the build the clock owed is due, the same request is answered — over an empty
    // registry, which is a count of zero rather than a refusal.
    emulator.finish_index_build();
    let answered = emulator.statistics(&request).await;
    assert_eq!(
        answered.status,
        200,
        "case the index ACTIVE: expected 200, got {} body {}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.statistics()["count"],
        json!(0),
        "case the index ACTIVE over an empty registry: expected count 0, got {}",
        answered.text()
    );

    // Each remaining row is one rule the request broke: a name that is no fleet index of
    // this service, a field the index does not carry, and a string that is not a query.
    let rows = vec![
        (
            "an index name this service does not serve",
            json!({
                "indexName": "AWS_Nothing",
                "queryString": "thingName:*",
                "aggregationField": "registry.version",
            }),
            "InvalidRequestException",
        ),
        (
            "a field the index does not carry",
            json!({
                "queryString": "thingName:*",
                "aggregationField": UNDECLARED_ATTRIBUTE_FIELD,
            }),
            "InvalidRequestException",
        ),
        (
            "a query string that is not a query",
            json!({
                "queryString": "thingName:(",
                "aggregationField": "registry.version",
            }),
            "InvalidQueryException",
        ),
    ];
    for (label, request, expected) in &rows {
        let refused = emulator.statistics(request).await;
        assert_eq!(
            refused.status,
            400,
            "case {label}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            &refused.error_code(),
            expected,
            "case {label}: expected {expected}, got {}",
            refused.text()
        );
    }
}

/// A managed field the index carries as a FLAG, so a case can show which field types an
/// aggregation is computed over: the distinct VALUES of a field are not counted for a
/// flag, and a position in a distribution is read only off numbers. `Boolean` is one of
/// the modelled `FieldType` values (`smithy_query(query="shape",shape="FieldType")`).
const BOOLEAN_MANAGED_FIELD: &str = "connectivity.connected";

impl Emulator {
    async fn percentiles(&self, body: &Value) -> Reply {
        self.dispatch("POST", "/indices/percentiles", Some(body))
            .await
    }
}

impl Reply {
    /// The `(percent, value)` pairs a `GetPercentiles` answer carries, in the order it
    /// reported them.
    ///
    /// A distribution with no values has no groups in it, which the answer carries as no
    /// pairs at all.
    fn percentiles(&self) -> Vec<(f64, f64)> {
        let body = self.json();
        body["percentiles"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|pair| {
                let percent = pair["percent"].as_f64().unwrap_or_else(|| {
                    panic!("each percentile pair carries a percent, got {pair}")
                });
                let value = pair["value"]
                    .as_f64()
                    .unwrap_or_else(|| panic!("each percentile pair carries a value, got {pair}"));
                (percent, value)
            })
            .collect()
    }
}

/// The percentile groups a request that names none is answered for: "The default
/// percentile groupings are: 1,5,25,50,75,95,99"
/// (`smithy_query(query="doc",shape="GetPercentiles")`).
const DEFAULT_PERCENT_GROUPS: &[f64] = &[1.0, 5.0, 25.0, 50.0, 75.0, 95.0, 99.0];

/// One `GetPercentiles` case: which things the query selects, which field is positioned,
/// which groups are asked for, and where each group stands.
struct PercentileCase {
    label: &'static str,
    query: &'static str,
    field: &'static str,
    /// The `percents` asked for, or `None` for a request that names none.
    percents: Option<Vec<f64>>,
    /// The `(percent, value)` pairs the answer must carry, in order.
    expected: Vec<(f64, f64)>,
}

/// RULE: `GetPercentiles` positions the groups it is asked for in the distribution of ONE
/// numeric field's values across the things the query selects — the group `p` standing `p`
/// percent of the way through those values in order, interpolated between the two
/// neighbouring values where the position falls between them — so every figure is computed
/// over the account's own registry.
#[tokio::test]
async fn percentiles_position_the_groups_in_the_field_the_query_selects() {
    let emulator = Emulator::with_numeric_custom_field("percentiles").await;
    for thing_type_name in ["sensor", "gateway"] {
        emulator
            .create_thing_type("percentiles", thing_type_name)
            .await;
    }
    // Five things carrying a numeric attribute and one carrying none: every figure
    // asserted below is a property of THESE values (10, 20, 30, 40 on the sensors and 100
    // on the gateway), so a wrong distribution cannot answer the table.
    for (thing_name, thing_type_name, temperature) in [
        ("sensor-1", "sensor", "10"),
        ("sensor-2", "sensor", "20"),
        ("sensor-3", "sensor", "30"),
        ("sensor-4", "sensor", "40"),
        ("gateway-1", "gateway", "100"),
    ] {
        emulator
            .create_thing(
                "percentiles",
                thing_name,
                Some(thing_type_name),
                &[(NUMERIC_ATTRIBUTE, temperature)],
            )
            .await;
    }
    emulator
        .create_thing("percentiles", "spare-1", None, &[])
        .await;

    // The rows differ along what the rule generalises over: which groups are asked for
    // (the defaults, an explicit set, the published bounds of a group, a set given out of
    // order and twice), how many values the query selects (the whole registry, one type,
    // one thing, none), and which numeric field is positioned (a custom attribute and a
    // managed registry field).
    let cases = vec![
        PercentileCase {
            label: "the default groups over the whole registry",
            query: "thingName:*",
            field: NUMERIC_ATTRIBUTE_FIELD,
            percents: None,
            // 10, 20, 30, 40, 100: the group p stands at p/100 of the way from the first
            // value to the last, so 1 is a fortieth of the step 10 -> 20 and 95 is four
            // fifths of the step 40 -> 100.
            expected: vec![
                (1.0, 10.4),
                (5.0, 12.0),
                (25.0, 20.0),
                (50.0, 30.0),
                (75.0, 40.0),
                (95.0, 88.0),
                (99.0, 97.6),
            ],
        },
        PercentileCase {
            label: "the groups the request names",
            query: "thingName:*",
            field: NUMERIC_ATTRIBUTE_FIELD,
            percents: Some(vec![25.0, 50.0, 75.0]),
            expected: vec![(25.0, 20.0), (50.0, 30.0), (75.0, 40.0)],
        },
        PercentileCase {
            label: "the published bounds of a percentile group",
            query: "thingName:*",
            field: NUMERIC_ATTRIBUTE_FIELD,
            // `Percent` is modelled `range 0..100`
            // (`smithy_query(query="shape",shape="Percent")`), so the whole of the
            // distribution is asked for at its bounds: its lowest and its highest value.
            percents: Some(vec![0.0, 100.0]),
            expected: vec![(0.0, 10.0), (100.0, 100.0)],
        },
        PercentileCase {
            label: "groups named out of order and twice",
            query: "thingName:*",
            field: NUMERIC_ATTRIBUTE_FIELD,
            percents: Some(vec![50.0, 25.0, 50.0]),
            expected: vec![(25.0, 20.0), (50.0, 30.0)],
        },
        PercentileCase {
            label: "a query selecting part of the registry",
            query: "thingTypeName:sensor",
            field: NUMERIC_ATTRIBUTE_FIELD,
            percents: Some(vec![0.0, 25.0, 50.0, 75.0, 100.0]),
            // 10, 20, 30, 40: the median falls BETWEEN two values and is read off the
            // line between them.
            expected: vec![
                (0.0, 10.0),
                (25.0, 17.5),
                (50.0, 25.0),
                (75.0, 32.5),
                (100.0, 40.0),
            ],
        },
        PercentileCase {
            label: "a query selecting one thing",
            query: "thingName:gateway-1",
            field: NUMERIC_ATTRIBUTE_FIELD,
            percents: Some(vec![1.0, 50.0, 99.0]),
            // One value is the whole distribution, so every group stands at it.
            expected: vec![(1.0, 100.0), (50.0, 100.0), (99.0, 100.0)],
        },
        PercentileCase {
            label: "a managed field the index carries as a number",
            query: "thingName:*",
            field: "registry.version",
            percents: Some(vec![0.0, 50.0, 100.0]),
            // A create leaves every thing at registry version 1.
            expected: vec![(0.0, 1.0), (50.0, 1.0), (100.0, 1.0)],
        },
        PercentileCase {
            label: "a query selecting a thing that carries no value for the field",
            query: "thingName:spare-1",
            field: NUMERIC_ATTRIBUTE_FIELD,
            percents: Some(vec![50.0]),
            expected: Vec::new(),
        },
        PercentileCase {
            label: "a query selecting nothing at all",
            query: "thingName:absent-*",
            field: NUMERIC_ATTRIBUTE_FIELD,
            percents: None,
            expected: Vec::new(),
        },
    ];
    for case in &cases {
        let mut request = json!({
            "queryString": case.query,
            "aggregationField": case.field,
        });
        if let Some(percents) = case.percents.as_ref() {
            request["percents"] = json!(percents);
        }
        let answer = emulator.percentiles(&request).await;
        assert_eq!(
            answer.status,
            200,
            "case {}: expected 200, got {} body {}",
            case.label,
            answer.status,
            answer.text()
        );
        let reported = answer.percentiles();
        assert_eq!(
            reported.len(),
            case.expected.len(),
            "case {}: expected {} groups {:?}, got {reported:?}",
            case.label,
            case.expected.len(),
            case.expected
        );
        for (expected, actual) in case.expected.iter().zip(reported.iter()) {
            let (expected_percent, expected_value) = *expected;
            let (actual_percent, actual_value) = *actual;
            assert!(
                (actual_percent - expected_percent).abs() <= TOLERANCE,
                "case {}: expected group {expected_percent}, got {actual_percent}",
                case.label
            );
            assert!(
                (actual_value - expected_value).abs()
                    <= TOLERANCE * expected_value.abs().max(1.0),
                "case {}: expected group {expected_percent} at {expected_value}, got \
                 {actual_value}",
                case.label
            );
        }
    }

    // A request naming no groups is answered for the DEFAULT groups, which is what the
    // first row asserted the values of: the groups themselves are asserted here against
    // the published list, so the default cannot drift.
    let defaulted = emulator
        .percentiles(&json!({
            "queryString": "thingName:*",
            "aggregationField": NUMERIC_ATTRIBUTE_FIELD,
        }))
        .await;
    let groups: Vec<f64> = defaulted
        .percentiles()
        .into_iter()
        .map(|(percent, _value)| percent)
        .collect();
    assert_eq!(
        groups,
        DEFAULT_PERCENT_GROUPS.to_vec(),
        "case a request naming no percents: expected the default groups {:?}, got {:?}",
        DEFAULT_PERCENT_GROUPS,
        defaulted.text()
    );

    // One more thing carrying a HIGHER value moves the distribution, which is what makes
    // the figures computed over the account's own records rather than fixed.
    emulator
        .create_thing(
            "percentiles",
            "sensor-5",
            Some("sensor"),
            &[(NUMERIC_ATTRIBUTE, "200")],
        )
        .await;
    let after = emulator
        .percentiles(&json!({
            "queryString": "thingName:*",
            "aggregationField": NUMERIC_ATTRIBUTE_FIELD,
            "percents": [50.0, 100.0],
        }))
        .await;
    // 10, 20, 30, 40, 100, 200: the median now falls between 30 and 40.
    assert_eq!(
        after.percentiles(),
        vec![(50.0, 35.0), (100.0, 200.0)],
        "case a thing carrying a new highest value: expected the groups to move, got {}",
        after.text()
    );
}

/// RULE: the groups are positioned in a distribution the INDEX holds, over a field it
/// carries as numbers, so the index's own state and the request's field decide the
/// refusal — and the same request is answered once the index is ACTIVE.
#[tokio::test]
async fn percentiles_over_an_absent_index_or_an_unpositionable_field_are_refused() {
    let request = json!({
        "queryString": "thingName:*",
        "aggregationField": "registry.version",
    });

    // No indexing configured at all: there is no index to position values in.
    let emulator = Emulator::new();
    let refused = emulator.percentiles(&request).await;
    assert_eq!(
        refused.status,
        404,
        "case no indexing configured: expected 404, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_code(),
        "ResourceNotFoundException",
        "case no indexing configured: expected ResourceNotFoundException, got {}",
        refused.text()
    );

    // Turned on, the index exists but is still building until the clock makes it active.
    emulator
        .enable_thing_index("percentiles while building")
        .await;
    emulator.hold_index_build();
    let building = emulator.percentiles(&request).await;
    assert_eq!(
        building.status,
        400,
        "case the index still building: expected 400, got {} body {}",
        building.status,
        building.text()
    );
    assert_eq!(
        building.error_code(),
        "IndexNotReadyException",
        "case the index still building: expected IndexNotReadyException, got {}",
        building.text()
    );

    // ACTIVE, the same request is answered — over a registry holding no thing at all,
    // which is a distribution with no values in it rather than a refusal.
    emulator.finish_index_build();
    let answered = emulator.percentiles(&request).await;
    assert_eq!(
        answered.status,
        200,
        "case the index ACTIVE over an empty registry: expected 200, got {} body {}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.percentiles(),
        Vec::new(),
        "case the index ACTIVE over an empty registry: expected no groups, got {}",
        answered.text()
    );

    // Each remaining row is one rule the request broke: a name that is no fleet index of
    // this service, a field the index does not carry, a field the index carries as text
    // rather than as the numbers a position is read off, a request naming no field at all,
    // a string that is not a query, and a group past the published bound of a percentile.
    let rows = vec![
        (
            "an index name this service does not serve",
            json!({
                "indexName": "AWS_Nothing",
                "queryString": "thingName:*",
                "aggregationField": "registry.version",
            }),
            400,
            "InvalidRequestException",
        ),
        (
            "a field the index does not carry",
            json!({
                "queryString": "thingName:*",
                "aggregationField": UNDECLARED_ATTRIBUTE_FIELD,
            }),
            400,
            "InvalidRequestException",
        ),
        (
            "a field the index carries as text",
            json!({
                "queryString": "thingName:*",
                "aggregationField": DECLARED_ATTRIBUTE_FIELD,
            }),
            400,
            "InvalidRequestException",
        ),
        (
            "a field the index carries as a flag",
            json!({
                "queryString": "thingName:*",
                "aggregationField": BOOLEAN_MANAGED_FIELD,
            }),
            400,
            "InvalidRequestException",
        ),
        (
            "a request naming no aggregation field",
            json!({ "queryString": "thingName:*" }),
            400,
            "InvalidRequestException",
        ),
        (
            "a query string that is not a query",
            json!({
                "queryString": "thingName:(",
                "aggregationField": "registry.version",
            }),
            400,
            "InvalidQueryException",
        ),
        (
            "a percentile group past the published bound",
            json!({
                "queryString": "thingName:*",
                "aggregationField": "registry.version",
                "percents": [101.0],
            }),
            400,
            "InvalidRequestException",
        ),
    ];
    for (label, request, status, expected) in &rows {
        let refused = emulator.percentiles(request).await;
        assert_eq!(
            refused.status,
            *status,
            "case {label}: expected {status}, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            &refused.error_code(),
            expected,
            "case {label}: expected {expected}, got {}",
            refused.text()
        );
    }
}

impl Emulator {
    async fn cardinality(&self, body: &Value) -> Reply {
        self.dispatch("POST", "/indices/cardinality", Some(body))
            .await
    }
}

impl Reply {
    /// The `cardinality` a `GetCardinality` answer carries.
    fn cardinality(&self) -> Value {
        self.json()["cardinality"].clone()
    }
}

/// RULE: `GetCardinality` counts the DISTINCT values one field holds across the things a
/// query selects — repeated values count once, a thing carrying no value for the field
/// contributes none, and a request naming no field counts the selected records
/// themselves.
#[tokio::test]
async fn cardinality_counts_the_distinct_values_of_the_things_the_query_selects() {
    let emulator = Emulator::with_numeric_custom_field("cardinality").await;
    for thing_type_name in ["sensor", "gateway"] {
        emulator.create_thing_type("cardinality", thing_type_name).await;
    }
    // Six things whose attribute values REPEAT: every figure asserted below is a property
    // of THIS registry, and the repeats are what separate a count of distinct values from
    // a count of the things that carry one.
    for (thing_name, thing_type_name, attributes) in [
        (
            "sensor-1",
            Some("sensor"),
            vec![("zone", "north"), (NUMERIC_ATTRIBUTE, "10")],
        ),
        (
            "sensor-2",
            Some("sensor"),
            vec![("zone", "north"), (NUMERIC_ATTRIBUTE, "10")],
        ),
        (
            "sensor-3",
            Some("sensor"),
            vec![("zone", "south"), (NUMERIC_ATTRIBUTE, "30")],
        ),
        (
            "gateway-1",
            Some("gateway"),
            vec![("zone", "south"), (NUMERIC_ATTRIBUTE, "40")],
        ),
        ("gateway-2", Some("gateway"), vec![]),
        ("spare-1", None, vec![]),
    ] {
        emulator
            .create_thing("cardinality", thing_name, thing_type_name, &attributes)
            .await;
    }

    /// One request and the number of distinct values this registry answers it with.
    struct Case {
        label: &'static str,
        query: &'static str,
        /// The `aggregationField`, or `None` for a request that names none.
        field: Option<&'static str>,
        cardinality: i64,
    }
    // The rows differ along what the rule generalises over: which field is counted (a text
    // custom field, a numeric custom field, a text MANAGED field, an identity field, a
    // numeric managed field, and no field at all), which things the query selects (all of
    // them, a name wildcard, an attribute value, exactly one, and none), and how many
    // values the selection holds (repeats, one, none).
    let cases = vec![
        Case {
            label: "a text custom field whose values repeat",
            query: "thingName:*",
            field: Some(DECLARED_ATTRIBUTE_FIELD),
            // north, north, south, south.
            cardinality: 2,
        },
        Case {
            label: "a numeric custom field whose values repeat",
            query: "thingName:*",
            field: Some(NUMERIC_ATTRIBUTE_FIELD),
            // 10, 10, 30, 40.
            cardinality: 3,
        },
        Case {
            label: "a text MANAGED field over every thing",
            query: "thingName:*",
            field: Some("registry.thingTypeName"),
            // sensor on three things, gateway on two, and one thing with no type at all.
            cardinality: 2,
        },
        Case {
            label: "an identity field, which every thing carries its own value of",
            query: "thingName:*",
            field: Some("thingName"),
            cardinality: 6,
        },
        Case {
            label: "a numeric MANAGED field every thing holds the same value of",
            query: "thingName:*",
            field: Some("registry.version"),
            // A create leaves every thing at registry version 1.
            cardinality: 1,
        },
        Case {
            label: "a numeric custom field over the things one name wildcard selects",
            query: "thingName:sensor-*",
            field: Some(NUMERIC_ATTRIBUTE_FIELD),
            // 10, 10, 30.
            cardinality: 2,
        },
        Case {
            label: "an identity field over the things one attribute value selects",
            query: "attributes.zone:south",
            field: Some("thingName"),
            cardinality: 2,
        },
        Case {
            label: "a field the one selected thing carries no value for",
            query: "thingName:spare-1",
            field: Some(NUMERIC_ATTRIBUTE_FIELD),
            cardinality: 0,
        },
        Case {
            label: "a query that selects no thing",
            query: "thingName:absent-*",
            field: Some("thingName"),
            cardinality: 0,
        },
        Case {
            label: "a request naming no aggregation field",
            query: "thingName:*",
            field: None,
            cardinality: 6,
        },
        Case {
            label: "a request naming no aggregation field over a narrower query",
            query: "attributes.zone:north",
            field: None,
            cardinality: 2,
        },
    ];

    for case in &cases {
        let mut request = json!({ "queryString": case.query });
        if let Some(field) = case.field {
            request["aggregationField"] = json!(field);
        }
        let answer = emulator.cardinality(&request).await;
        assert_eq!(
            answer.status,
            200,
            "case {}: expected 200, got {} body {}",
            case.label,
            answer.status,
            answer.text()
        );
        assert_eq!(
            answer.cardinality(),
            json!(case.cardinality),
            "case {}: expected cardinality {}, got {}",
            case.label,
            case.cardinality,
            answer.text()
        );
    }

    // The count is read out of the REGISTRY, so a thing carrying a value the registry did
    // not hold before raises it — which is what makes the figures above computed over the
    // account's own records rather than fixed.
    emulator
        .create_thing("cardinality", "sensor-4", Some("sensor"), &[("zone", "west")])
        .await;
    let after = emulator
        .cardinality(&json!({
            "queryString": "thingName:*",
            "aggregationField": DECLARED_ATTRIBUTE_FIELD,
        }))
        .await;
    assert_eq!(
        after.cardinality(),
        json!(3),
        "case a thing carrying a new value of the counted field: expected cardinality 3, got {}",
        after.text()
    );
}

/// RULE: the count is read out of the INDEX and over a field whose values it can be asked
/// of, so the index's own state and the request's field decide the refusal — and the same
/// request is answered once the index is ACTIVE.
#[tokio::test]
async fn cardinality_over_an_absent_index_or_an_uncountable_field_is_refused() {
    let request = json!({
        "queryString": "thingName:*",
        "aggregationField": "thingName",
    });

    // No indexing configured at all: there is no index to count values out of.
    let emulator = Emulator::new();
    let refused = emulator.cardinality(&request).await;
    assert_eq!(
        refused.status,
        404,
        "case thing indexing OFF: expected 404, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_code(),
        "ResourceNotFoundException",
        "case thing indexing OFF: got {}",
        refused.text()
    );

    // Turned on, the index exists but is still building until the clock makes it active.
    emulator
        .enable_thing_index("cardinality while building")
        .await;
    emulator.hold_index_build();
    let building = emulator.cardinality(&request).await;
    assert_eq!(
        building.status,
        400,
        "case the index still building: expected 400, got {} body {}",
        building.status,
        building.text()
    );
    assert_eq!(
        building.error_code(),
        "IndexNotReadyException",
        "case the index still building: got {}",
        building.text()
    );

    // Once the build the clock owed is due, the same request is answered — over an empty
    // registry, which is a count of zero rather than a refusal.
    emulator.finish_index_build();
    let answered = emulator.cardinality(&request).await;
    assert_eq!(
        answered.status,
        200,
        "case the index ACTIVE: expected 200, got {} body {}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.cardinality(),
        json!(0),
        "case the index ACTIVE over an empty registry: expected cardinality 0, got {}",
        answered.text()
    );

    // Each remaining row is one rule the request broke: a name that is no fleet index of
    // this service, a field the index does not carry, a field whose type is a flag rather
    // than values a cardinality is asked of, and a string that is not a query.
    let rows = vec![
        (
            "an index name this service does not serve",
            json!({
                "indexName": "AWS_Nothing",
                "queryString": "thingName:*",
                "aggregationField": "thingName",
            }),
            "InvalidRequestException",
        ),
        (
            "a field the index does not carry",
            json!({
                "queryString": "thingName:*",
                "aggregationField": UNDECLARED_ATTRIBUTE_FIELD,
            }),
            "InvalidRequestException",
        ),
        (
            "a field the index carries as a flag",
            json!({
                "queryString": "thingName:*",
                "aggregationField": BOOLEAN_MANAGED_FIELD,
            }),
            "InvalidRequestException",
        ),
        (
            "a query string that is not a query",
            json!({
                "queryString": "thingName:(",
                "aggregationField": "thingName",
            }),
            "InvalidQueryException",
        ),
    ];
    for (label, request, expected) in &rows {
        let refused = emulator.cardinality(request).await;
        assert_eq!(
            refused.status,
            400,
            "case {label}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            &refused.error_code(),
            expected,
            "case {label}: expected {expected}, got {}",
            refused.text()
        );
    }
}
