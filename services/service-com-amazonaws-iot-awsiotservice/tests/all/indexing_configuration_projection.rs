// SPDX-License-Identifier: CC0-1.0

//! What a READER of the fleet-indexing configuration reports on top of what the writer
//! stored, and the cross-member precondition the writer enforces.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over the
//! restJson1 `GET /indexing/config` and `POST /indexing/config` bindings — so what is
//! asserted is what an AWS SDK client would see.
//!
//! The rules under test:
//!
//! * `managedFields` are the SERVICE's: a section whose indexing is on reports the
//!   fields the Fleet Indexing service knows the types of, one group per enabled data
//!   source, and a section whose indexing is off reports none, because nothing is
//!   indexed for it;
//! * the thing section's `filter` is always reported: an enabled index reports each of
//!   its three selections, with a selection nothing was added to reported as the empty
//!   one, so a client reading the configuration to edit it and write it back finds the
//!   member rather than having to tell "no selections" from "not modelled";
//! * a `namedShadowNames` selection is admissible only while `namedShadowIndexingMode`
//!   is ON — "To add named shadows to your fleet indexing configuration, set
//!   `namedShadowIndexingMode` to be ON and specify your shadow names in
//!   `namedShadowNames` filter"
//!   (`smithy_query(query="doc",shape="ThingIndexingConfiguration",member="filter")`) —
//!   and an update that breaks it is refused with the modelled
//!   `InvalidRequestException` (400) and records nothing.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::ops::types::{
    FLEET_INDEXING_API_VALUES, NAMED_SHADOW_INDEXING_MODE_VALUES, THING_INDEXING_MODE_VALUES,
};
use service_com_amazonaws_iot_awsiotservice::state::{
    INDEXING_MODE_OFF, NAMED_SHADOW_FILTER_WITHOUT_INDEXING_MESSAGE, NAMED_SHADOW_INDEXING_MODE_ON,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite is signed for. The configuration is
/// account-wide, so no assertion depends on it.
const REGION: &str = "ap-south-1";

/// The `x-amzn-errortype` this service names a request its validation layer will not
/// accept under: the shape name qualified with AWS IoT's API doc namespace.
const QUALIFIED_INVALID_REQUEST_EXCEPTION: &str =
    "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// The managed fields of the THING index the REGISTRY data source contributes, as the
/// Fleet Indexing service's managed-field table lists them: the name and the
/// `FieldType` of each, in the order the service reports them.
const REGISTRY_MANAGED_FIELDS: &[(&str, &str)] = &[
    ("registry.version", "Number"),
    ("thingName", "String"),
    ("thingId", "String"),
    ("registry.thingTypeName", "String"),
    ("registry.thingGroupNames", "String"),
];

/// The managed fields the CONNECTIVITY data source adds to the thing index while
/// connectivity indexing is on — the `connectivity.` members this emulator already
/// knows as indexed fields (`state::fleet_metrics::MANAGED_THING_FIELDS`).
const CONNECTIVITY_MANAGED_FIELDS: &[(&str, &str)] = &[
    ("connectivity.connected", "Boolean"),
    ("connectivity.timestamp", "Number"),
    ("connectivity.disconnectReason", "String"),
];

/// The managed fields of the THING GROUP index.
const THING_GROUP_MANAGED_FIELDS: &[(&str, &str)] = &[
    ("version", "Number"),
    ("thingGroupId", "String"),
    ("parentGroupNames", "String"),
    ("description", "String"),
    ("thingGroupName", "String"),
];

/// One emulator: the shared engine plus a router with this service mounted.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn dispatch(&self, method: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}/indexing/config"))
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
            .expect("the indexing-configuration request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    async fn update(&self, body: &Value) -> Reply {
        self.dispatch("POST", Some(body)).await
    }

    /// The configuration a successful read reports.
    async fn reported(&self, case: &str) -> Value {
        let reply = self.dispatch("GET", None).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected GetIndexingConfiguration to answer 200, got {reply:?}"
        );
        reply.body
    }
}

/// A dispatched response, reduced to what the assertions read.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// A managed-field catalog as the `Fields` list a reader reports.
fn catalog(entries: &[(&str, &str)]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|(name, field_type)| json!({ "name": name, "type": field_type }))
            .collect(),
    )
}

/// The managed fields a read must report for the thing section under `connectivity`.
fn expected_thing_fields(connectivity: &str) -> Value {
    let mut entries: Vec<(&str, &str)> = REGISTRY_MANAGED_FIELDS.to_vec();
    if connectivity != INDEXING_MODE_OFF {
        entries.extend_from_slice(CONNECTIVITY_MANAGED_FIELDS);
    }
    catalog(&entries)
}

/// The three selections the `filter` of an enabled thing index reports, each as the
/// empty selection when nothing was added to it.
fn empty_selections() -> Value {
    json!({
        "namedShadowNames": [],
        "geoLocations": [],
        "connectivity": { "includeSocketInformation": [] },
    })
}

/// A section whose indexing is ON reports the service's managed fields for each
/// enabled data source; one whose indexing is OFF reports none.
#[tokio::test]
async fn managed_fields_are_reported_for_every_enabled_data_source() {
    // The rows span the thing section's modes — every value its mode catalog declares,
    // against connectivity indexing off and on — and the group section's two.
    for thing_mode in THING_INDEXING_MODE_VALUES {
        for connectivity in [INDEXING_MODE_OFF, "STATUS"] {
            // Connectivity indexing needs a thing index that is not OFF (the
            // documented cross-member rule), so that pair is not a configuration a
            // caller can ask for.
            if *thing_mode == INDEXING_MODE_OFF && connectivity != INDEXING_MODE_OFF {
                continue;
            }
            let emulator = Emulator::new();
            let case = format!("thingIndexingMode {thing_mode}, connectivity {connectivity}");
            let update = emulator
                .update(&json!({
                    "thingIndexingConfiguration": {
                        "thingIndexingMode": thing_mode,
                        "thingConnectivityIndexingMode": connectivity,
                    },
                }))
                .await;
            assert_eq!(
                update.status, 200,
                "case {case}: expected UpdateIndexingConfiguration to answer 200, got {update:?}"
            );

            let reported = emulator.reported(&case).await;
            let section = &reported["thingIndexingConfiguration"];
            if *thing_mode == INDEXING_MODE_OFF {
                assert!(
                    section.get("managedFields").is_none(),
                    "case {case}: a disabled thing index indexes nothing, got {section}"
                );
            } else {
                assert_eq!(
                    section["managedFields"],
                    expected_thing_fields(connectivity),
                    "case {case}: got {section}"
                );
            }
        }
    }

    for group_mode in ["ON", INDEXING_MODE_OFF] {
        let emulator = Emulator::new();
        let case = format!("thingGroupIndexingMode {group_mode}");
        let update = emulator
            .update(&json!({
                "thingGroupIndexingConfiguration": { "thingGroupIndexingMode": group_mode },
            }))
            .await;
        assert_eq!(
            update.status, 200,
            "case {case}: expected UpdateIndexingConfiguration to answer 200, got {update:?}"
        );
        let reported = emulator.reported(&case).await;
        let section = &reported["thingGroupIndexingConfiguration"];
        if group_mode == INDEXING_MODE_OFF {
            assert!(
                section.get("managedFields").is_none(),
                "case {case}: a disabled group index indexes nothing, got {section}"
            );
        } else {
            assert_eq!(
                section["managedFields"],
                catalog(THING_GROUP_MANAGED_FIELDS),
                "case {case}: got {section}"
            );
        }
    }
}

/// The thing section's `filter` is reported for every configuration: as the structure
/// with no selections while indexing is off, and with each selection — the empty one
/// where nothing was added — once it is on.
#[tokio::test]
async fn the_filter_structure_is_reported_for_every_configuration() {
    // One row per selection a caller can make, plus the configurations that make none.
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "an account that has never updated its configuration",
            Value::Null,
            json!({}),
        ),
        (
            "fleet indexing turned off",
            json!({ "thingIndexingConfiguration": { "thingIndexingMode": INDEXING_MODE_OFF } }),
            json!({}),
        ),
        (
            "registry indexing with no selections",
            json!({ "thingIndexingConfiguration": { "thingIndexingMode": "REGISTRY" } }),
            empty_selections(),
        ),
        (
            "a named shadow selection",
            json!({
                "thingIndexingConfiguration": {
                    "thingIndexingMode": "REGISTRY_AND_SHADOW",
                    "namedShadowIndexingMode": NAMED_SHADOW_INDEXING_MODE_ON,
                    "filter": { "namedShadowNames": ["$package", "telemetry-1"] },
                },
            }),
            json!({
                "namedShadowNames": ["$package", "telemetry-1"],
                "geoLocations": [],
                "connectivity": { "includeSocketInformation": [] },
            }),
        ),
        (
            "a geolocation selection",
            json!({
                "thingIndexingConfiguration": {
                    "thingIndexingMode": "REGISTRY_AND_SHADOW",
                    "filter": {
                        "geoLocations": [{ "name": "attributes.where", "order": "LonLat" }],
                    },
                },
            }),
            json!({
                "namedShadowNames": [],
                "geoLocations": [{ "name": "attributes.where", "order": "LonLat" }],
                "connectivity": { "includeSocketInformation": [] },
            }),
        ),
        (
            "a connectivity selection naming every modelled fleet indexing api",
            json!({
                "thingIndexingConfiguration": {
                    "thingIndexingMode": "REGISTRY",
                    "thingConnectivityIndexingMode": "STATUS",
                    "filter": {
                        "connectivity": { "includeSocketInformation": FLEET_INDEXING_API_VALUES },
                    },
                },
            }),
            json!({
                "namedShadowNames": [],
                "geoLocations": [],
                "connectivity": { "includeSocketInformation": FLEET_INDEXING_API_VALUES },
            }),
        ),
    ];

    for (case, request, expected) in rows {
        let emulator = Emulator::new();
        if let Some(request) = request.as_object() {
            let update = emulator.update(&Value::Object(request.clone())).await;
            assert_eq!(
                update.status, 200,
                "case {case:?}: expected UpdateIndexingConfiguration to answer 200, got {update:?}"
            );
        }
        let reported = emulator.reported(case).await;
        let section = &reported["thingIndexingConfiguration"];
        assert_eq!(
            section.get("filter"),
            Some(&expected),
            "case {case:?}: got {section}"
        );
        // Reading again reports the same thing: the projection reads the store and
        // changes nothing in it.
        let again = emulator.reported(case).await;
        assert_eq!(
            again, reported,
            "case {case:?}: a second read reported {again} instead of {reported}"
        );
    }
}

/// A `namedShadowNames` selection is refused unless named-shadow indexing is ON, and
/// the refused update records nothing.
#[tokio::test]
async fn a_named_shadow_selection_needs_named_shadow_indexing_on() {
    // The rows span every value the mode's own catalog declares, against a selection
    // of one name and of several, with the mode left out of the request entirely as
    // its own row.
    let selections: &[(&str, Value)] = &[
        ("one shadow name", json!(["telemetry-1"])),
        (
            "several shadow names",
            json!(["$package", "telemetry-1", "telemetry-2"]),
        ),
    ];

    let mut modes: Vec<Option<&str>> = NAMED_SHADOW_INDEXING_MODE_VALUES
        .iter()
        .map(|mode| Some(*mode))
        .collect();
    modes.push(None);

    for mode in modes {
        for (selection_label, names) in selections {
            let emulator = Emulator::new();
            let case = format!("namedShadowIndexingMode {mode:?} with {selection_label}");
            let mut section = json!({
                "thingIndexingMode": "REGISTRY_AND_SHADOW",
                "filter": { "namedShadowNames": names },
            });
            if let Some(mode) = mode {
                section["namedShadowIndexingMode"] = json!(mode);
            }
            let request = json!({ "thingIndexingConfiguration": section });

            let reply = emulator.update(&request).await;
            if mode == Some(NAMED_SHADOW_INDEXING_MODE_ON) {
                assert_eq!(
                    reply.status, 200,
                    "case {case}: a selection under the enabling mode must be recorded, got \
                     {reply:?}"
                );
                let reported = emulator.reported(&case).await;
                assert_eq!(
                    reported["thingIndexingConfiguration"]["filter"]["namedShadowNames"],
                    *names,
                    "case {case}: got {}",
                    reported["thingIndexingConfiguration"]
                );
            } else {
                assert_eq!(
                    reply.status, 400,
                    "case {case}: expected the selection to be refused 400, got {reply:?}"
                );
                assert_eq!(
                    reply.error_code.as_deref(),
                    Some(QUALIFIED_INVALID_REQUEST_EXCEPTION),
                    "case {case}: expected the qualified InvalidRequestException, got {reply:?}"
                );
                assert_eq!(
                    reply.message(),
                    NAMED_SHADOW_FILTER_WITHOUT_INDEXING_MESSAGE,
                    "case {case}: got {:?}",
                    reply.message()
                );
                // The refused update recorded nothing: the account still reads as the
                // one that never configured fleet indexing.
                let reported = emulator.reported(&case).await;
                assert_eq!(
                    reported["thingIndexingConfiguration"]["thingIndexingMode"],
                    json!(INDEXING_MODE_OFF),
                    "case {case}: a refused update changed the configuration to {reported}"
                );
            }
        }
    }
}

/// The filter's OTHER selections are not gated on named-shadow indexing: a
/// geolocation or connectivity selection is recorded whatever that mode is.
#[tokio::test]
async fn the_other_selections_are_not_gated_on_named_shadow_indexing() {
    for mode in NAMED_SHADOW_INDEXING_MODE_VALUES {
        let emulator = Emulator::new();
        let case = format!("namedShadowIndexingMode {mode} with a geolocation selection");
        let reply = emulator
            .update(&json!({
                "thingIndexingConfiguration": {
                    "thingIndexingMode": "REGISTRY_AND_SHADOW",
                    "namedShadowIndexingMode": mode,
                    "filter": {
                        "geoLocations": [{ "name": "attributes.where", "order": "LatLon" }],
                    },
                },
            }))
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected 200, got {reply:?}"
        );
        let reported = emulator.reported(&case).await;
        assert_eq!(
            reported["thingIndexingConfiguration"]["filter"]["geoLocations"][0]["name"],
            json!("attributes.where"),
            "case {case}: got {}",
            reported["thingIndexingConfiguration"]
        );
    }
}
