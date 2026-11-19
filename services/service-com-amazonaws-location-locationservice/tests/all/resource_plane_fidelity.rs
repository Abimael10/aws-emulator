// SPDX-License-Identifier: CC0-1.0

//! The resource plane's wire fidelity: how a read projects a tag map, how a name
//! the service cannot resolve (or already holds) is refused, and what a delete
//! retires.
//!
//! Every call goes through the shared harness (`crate::harness`), so each one is a
//! separate HTTP request against one shared engine: what a read reports, and what it
//! stops reporting after a delete, comes from PERSISTED state rather than from a
//! handler echoing its own input.
//!
//! The tables span the resource planes this crate serves (geofence collection,
//! tracker, route calculator, map, place index, and the geofence held under a
//! collection) and,
//! for the tag map, the sizes and shapes `TagMap` admits
//! (`smithy_query(query="shape",shape="TagMap")`: map `@length max 50`, `TagKey`
//! `@length 1..128`, `TagValue` `@length 0..256`). The collection the issue report
//! replays is one row of each table.

use serde_json::{json, Map, Value};

// The one HTTP harness this crate's suites share, declared from `tests/all.rs`.
use crate::harness::{call, call_in_region, emulator, millis_of, Answer, ACCOUNT, REGION};

/// One resource plane: how a resource of that kind is created and read, and the
/// phrase the service names it by in a refusal about it.
struct Plane {
    case: &'static str,
    /// The fixed part of this service's refusal sentences for the resource.
    phrase: &'static str,
    create_path: &'static str,
    /// The create body for a name, with every optional member absent.
    create_body: fn(&str) -> Value,
    /// The read path for a name.
    read_path: fn(&str) -> String,
}

/// The four control-plane resources this crate creates, reads and tags.
fn tagged_planes() -> Vec<Plane> {
    vec![
        Plane {
            case: "map",
            phrase: "Map",
            create_path: "/maps/v0/maps",
            // `Configuration` is required, and `VectorEsriStreets` is one of the
            // styles the `MapConfiguration$Style` catalog publishes.
            create_body: |name| {
                json!({ "MapName": name, "Configuration": { "Style": "VectorEsriStreets" } })
            },
            read_path: |name| format!("/maps/v0/maps/{name}"),
        },
        Plane {
            case: "geofence collection",
            phrase: "Geofence collection",
            create_path: "/geofencing/v0/collections",
            create_body: |name| json!({ "CollectionName": name }),
            read_path: |name| format!("/geofencing/v0/collections/{name}"),
        },
        Plane {
            case: "tracker",
            phrase: "Tracker",
            create_path: "/tracking/v0/trackers",
            create_body: |name| json!({ "TrackerName": name }),
            read_path: |name| format!("/tracking/v0/trackers/{name}"),
        },
        Plane {
            case: "route calculator",
            phrase: "Route calculator",
            create_path: "/routes/v0/calculators",
            // `DataSource` is required, and `Esri` is one of the three the member
            // documents.
            create_body: |name| json!({ "CalculatorName": name, "DataSource": "Esri" }),
            read_path: |name| format!("/routes/v0/calculators/{name}"),
        },
    ]
}

/// One row of the tag table: the tag map a resource is created with.
struct TagCase {
    case: &'static str,
    tags: Vec<(String, String)>,
}

/// The tag table. Rows differ along the SIZE of the map (none, one, a few, and the
/// `TagMap` maximum of 50) and the lengths its key and value admit (a 128-character
/// key, an empty value), so what a read projects comes from the stored map in each
/// shape rather than from one.
fn tag_cases() -> Vec<TagCase> {
    vec![
        TagCase {
            case: "untagged",
            tags: Vec::new(),
        },
        TagCase {
            case: "one tag whose value is empty",
            // `TagValue` is `@length 0..256`, so the empty string is a value.
            tags: vec![("owner".to_string(), String::new())],
        },
        TagCase {
            case: "three tags",
            tags: vec![
                ("cost-center".to_string(), "42".to_string()),
                ("env".to_string(), "staging".to_string()),
                ("team".to_string(), "logistics".to_string()),
            ],
        },
        TagCase {
            case: "a key at the longest length TagKey admits",
            // `TagKey` is `@length 1..128`: the published bound itself.
            tags: vec![("k".repeat(128), "long-key".to_string())],
        },
        TagCase {
            case: "the largest map TagMap admits",
            // `TagMap` is `@length max 50`: the published bound itself.
            tags: (0..50)
                .map(|i| (format!("key-{i:02}"), format!("value-{i:02}")))
                .collect(),
        },
    ]
}

/// The tag map a row sends, as a JSON object.
fn tags_body(tags: &[(String, String)]) -> Value {
    Value::Object(
        tags.iter()
            .map(|(key, value)| (key.clone(), json!(value)))
            .collect::<Map<String, Value>>(),
    )
}

/// The `Tags` member of a response, or `None` when the response omits it.
fn reported_tags(body: &Value) -> Option<Vec<(String, String)>> {
    let object = body.get("Tags")?.as_object()?;
    let mut tags: Vec<(String, String)> = object
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value.as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    tags.sort();
    Some(tags)
}

/// RULE: a resource read projects the resource's tag map unconditionally — an
/// untagged resource reports an EMPTY map, never an absent member.
#[tokio::test]
async fn every_resource_read_projects_its_tag_map_as_a_map() {
    for plane in tagged_planes() {
        for row in tag_cases() {
            let router = emulator();
            let name = "tagged";
            let mut body = (plane.create_body)(name);
            if !row.tags.is_empty() {
                body["Tags"] = tags_body(&row.tags);
            }
            let created = call(&router, "POST", plane.create_path, Some(body)).await;
            assert_eq!(
                created.status, 200,
                "case {:?}/{:?}: the create must succeed, got {} {:?}",
                plane.case, row.case, created.status, created.body
            );

            let read = call(&router, "GET", &(plane.read_path)(name), None).await;
            assert_eq!(
                read.status, 200,
                "case {:?}/{:?}: the read must succeed, got {} {:?}",
                plane.case, row.case, read.status, read.body
            );
            let body = read.json();
            let mut expected = row.tags.clone();
            expected.sort();
            assert_eq!(
                reported_tags(&body),
                Some(expected.clone()),
                "case {:?}/{:?}: expected the Tags member to be the map {expected:?}, got {:?}",
                plane.case,
                row.case,
                body.get("Tags")
            );
        }
    }
}

/// RULE: a name no resource of that kind is stored under is refused with
/// `ResourceNotFoundException` (404, `x-amzn-errortype` naming the shape) whose
/// message is the resource's own phrase, then the name the request named it by, then
/// a period.
#[tokio::test]
async fn an_unresolvable_name_is_refused_with_the_services_own_sentence() {
    for plane in tagged_planes() {
        let router = emulator();
        // One resource exists, so the refusals below are about the requested name
        // rather than about an empty store.
        let created = call(
            &router,
            "POST",
            plane.create_path,
            Some((plane.create_body)("Exists")),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: the positive control must be created, got {} {:?}",
            plane.case, created.status, created.body
        );

        // Names that satisfy `ResourceName`'s `^[-._\w]+$` but name nothing,
        // including case variants of the one that does exist: the lookup is exact.
        for name in ["missing", "exists", "EXISTS", "Exists.2", "x", "a_b-c.1"] {
            let read = call(&router, "GET", &(plane.read_path)(name), None).await;
            assert_refused(
                &read,
                404,
                "ResourceNotFoundException",
                &format!("{} not found: {name}.", plane.phrase),
                &format!("{}/{name}", plane.case),
            );
        }
    }
}

/// RULE: a create whose name a resource of that kind already holds is refused with
/// `ConflictException` (409) worded from the same two parts.
#[tokio::test]
async fn a_name_already_held_is_refused_with_the_services_own_sentence() {
    for plane in tagged_planes() {
        let router = emulator();
        for name in ["taken", "Taken_1.name-2"] {
            let created = call(
                &router,
                "POST",
                plane.create_path,
                Some((plane.create_body)(name)),
            )
            .await;
            assert_eq!(
                created.status, 200,
                "case {:?}/{name}: the first create must succeed, got {} {:?}",
                plane.case, created.status, created.body
            );

            let again = call(
                &router,
                "POST",
                plane.create_path,
                Some((plane.create_body)(name)),
            )
            .await;
            assert_refused(
                &again,
                409,
                "ConflictException",
                &format!("{} already exists: {name}.", plane.phrase),
                &format!("{}/{name}", plane.case),
            );
        }
    }
}

/// RULE: a delete retires the named resource permanently — it leaves the store with
/// everything held on it — so every later read or delete of that name resolves
/// nothing and is refused with `ResourceNotFoundException` (404, `x-amzn-errortype`
/// naming the shape), while the resources the delete did not name are untouched and
/// the name itself becomes free to create again.
///
/// The table is every resource plane of this crate whose delete is implemented
/// (map, geofence collection, tracker, route calculator); the names differ along the
/// punctuation `ResourceName`'s `^[-._\w]+$` admits. The calculator the issue report
/// replays is one row.
#[tokio::test]
async fn a_delete_retires_the_resource_it_names_on_every_plane() {
    for plane in tagged_planes() {
        let router = emulator();
        // Three resources, so a delete is shown to retire the one it names and
        // nothing else.
        let names = ["retired", "kept.1", "kept_2"];
        for name in names {
            let mut body = (plane.create_body)(name);
            body["Tags"] = json!({ "generation": "first" });
            let created = call(&router, "POST", plane.create_path, Some(body)).await;
            assert_eq!(
                created.status, 200,
                "case {:?}/{name}: the create must succeed, got {} {:?}",
                plane.case, created.status, created.body
            );
        }

        // A name nothing is stored under is refused rather than reported as deleted.
        for missing in ["never-created", "RETIRED", "kept.3"] {
            assert_refused(
                &call(&router, "DELETE", &(plane.read_path)(missing), None).await,
                404,
                "ResourceNotFoundException",
                &format!("{} not found: {missing}.", plane.phrase),
                &format!("{}: delete a name nothing is stored under", plane.case),
            );
        }

        let deleted = call(&router, "DELETE", &(plane.read_path)("retired"), None).await;
        assert_eq!(
            deleted.status, 200,
            "case {:?}: the delete must succeed, got {} {:?}",
            plane.case, deleted.status, deleted.body
        );

        // Every way back into the retired resource is refused, including a second
        // delete of the same name.
        assert_refused(
            &call(&router, "GET", &(plane.read_path)("retired"), None).await,
            404,
            "ResourceNotFoundException",
            &format!("{} not found: retired.", plane.phrase),
            &format!("{}: read after delete", plane.case),
        );
        assert_refused(
            &call(&router, "DELETE", &(plane.read_path)("retired"), None).await,
            404,
            "ResourceNotFoundException",
            &format!("{} not found: retired.", plane.phrase),
            &format!("{}: delete after delete", plane.case),
        );

        // The resources the delete did not name keep everything they held.
        for name in ["kept.1", "kept_2"] {
            let read = call(&router, "GET", &(plane.read_path)(name), None).await;
            assert_eq!(
                read.status, 200,
                "case {:?}/{name}: a resource the delete did not name must still be readable, got {} {:?}",
                plane.case, read.status, read.body
            );
            assert_eq!(
                reported_tags(&read.json()),
                Some(vec![("generation".to_string(), "first".to_string())]),
                "case {:?}/{name}: a resource the delete did not name must keep its tags, got {:?}",
                plane.case,
                read.json().get("Tags")
            );
        }

        // The retired name is free again, and what it reads back is the NEW
        // resource: the tag map the deleted one held went with it.
        let recreated = call(
            &router,
            "POST",
            plane.create_path,
            Some((plane.create_body)("retired")),
        )
        .await;
        assert_eq!(
            recreated.status, 200,
            "case {:?}: re-creating a retired name must succeed, got {} {:?}",
            plane.case, recreated.status, recreated.body
        );
        let read = call(&router, "GET", &(plane.read_path)("retired"), None).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: the re-created resource must be readable, got {} {:?}",
            plane.case, read.status, read.body
        );
        assert_eq!(
            reported_tags(&read.json()),
            Some(Vec::new()),
            "case {:?}: the re-created resource must hold none of the retired one's tags, got {:?}",
            plane.case,
            read.json().get("Tags")
        );
    }
}

/// RULE: a delete retires the resource permanently, so every later read of that name
/// resolves nothing and is refused — and the resources the deleted one owned go with
/// it.
#[tokio::test]
async fn deleting_a_geofence_collection_retires_it_and_the_geofences_it_held() {
    let router = emulator();
    // Three collections, so a delete is shown to retire the one it names and
    // nothing else.
    for name in ["retired", "kept.1", "kept_2"] {
        let created = call(
            &router,
            "POST",
            "/geofencing/v0/collections",
            Some(json!({ "CollectionName": name, "Tags": { "generation": "first" } })),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {name:?}: CreateGeofenceCollection must succeed, got {} {:?}",
            created.status, created.body
        );
    }

    let stored = call(
        &router,
        "POST",
        "/geofencing/v0/collections/retired/put-geofences",
        Some(json!({
            "Entries": [{
                "GeofenceId": "gf1",
                "Geometry": { "Circle": { "Center": [-122.33, 47.60], "Radius": 500.0 } },
            }],
        })),
    )
    .await;
    assert_eq!(
        stored.status, 200,
        "the geofence write must succeed, got {} {:?}",
        stored.status, stored.body
    );

    let deleted = call(&router, "DELETE", "/geofencing/v0/collections/retired", None).await;
    assert_eq!(
        deleted.status, 200,
        "DeleteGeofenceCollection must succeed, got {} {:?}",
        deleted.status, deleted.body
    );

    // Every way back into the retired collection is refused, including a second
    // delete, and the geofence it held is gone with it.
    assert_refused(
        &call(&router, "GET", "/geofencing/v0/collections/retired", None).await,
        404,
        "ResourceNotFoundException",
        "Geofence collection not found: retired.",
        "describe after delete",
    );
    assert_refused(
        &call(&router, "DELETE", "/geofencing/v0/collections/retired", None).await,
        404,
        "ResourceNotFoundException",
        "Geofence collection not found: retired.",
        "delete after delete",
    );
    assert_refused(
        &call(
            &router,
            "GET",
            "/geofencing/v0/collections/retired/geofences/gf1",
            None,
        )
        .await,
        404,
        "ResourceNotFoundException",
        "Geofence collection not found: retired.",
        "read a geofence of the deleted collection",
    );
    assert_refused(
        &call(
            &router,
            "POST",
            "/geofencing/v0/collections/retired/put-geofences",
            Some(json!({
                "Entries": [{
                    "GeofenceId": "gf2",
                    "Geometry": { "Circle": { "Center": [0.0, 0.0], "Radius": 10.0 } },
                }],
            })),
        )
        .await,
        404,
        "ResourceNotFoundException",
        "Geofence collection not found: retired.",
        "write a geofence to the deleted collection",
    );

    // The collections the delete did not name are untouched.
    for name in ["kept.1", "kept_2"] {
        let read = call(
            &router,
            "GET",
            &format!("/geofencing/v0/collections/{name}"),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {name:?}: a collection the delete did not name must still be readable, got {} {:?}",
            read.status, read.body
        );
    }

    // "Retired" means gone from the account's own listing too, not only from the
    // read by name.
    let listed = call(
        &router,
        "POST",
        "/geofencing/v0/list-collections",
        Some(json!({})),
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "ListGeofenceCollections must succeed, got {} {:?}",
        listed.status, listed.body
    );
    let mut names: Vec<String> = listed
        .json()
        .get("Entries")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("Entries must be a list, got {:?}", listed.body))
        .iter()
        .map(|entry| {
            entry
                .get("CollectionName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["kept.1".to_string(), "kept_2".to_string()],
        "the listing must report the collections the delete did not name, and only those, got {names:?}"
    );

    // Re-creating the retired name is accepted, and what it reads back is the NEW
    // resource: no tags, no geofences.
    let recreated = call(
        &router,
        "POST",
        "/geofencing/v0/collections",
        Some(json!({ "CollectionName": "retired" })),
    )
    .await;
    assert_eq!(
        recreated.status, 200,
        "re-creating a retired name must succeed, got {} {:?}",
        recreated.status, recreated.body
    );
    let read = call(&router, "GET", "/geofencing/v0/collections/retired", None).await;
    assert_eq!(
        read.status, 200,
        "the re-created collection must be readable, got {} {:?}",
        read.status, read.body
    );
    let body = read.json();
    assert_eq!(
        reported_tags(&body),
        Some(Vec::new()),
        "the re-created collection must report an empty tag map, got {:?}",
        body.get("Tags")
    );
    assert_eq!(
        body.get("GeofenceCount").and_then(Value::as_i64),
        Some(0),
        "the re-created collection must hold none of the retired one's geofences, got {body}"
    );
}

/// A refusal's status, wire error code, `x-amzn-errortype` header and message.
fn assert_refused(
    answer: &Answer,
    status: u16,
    error_code: &str,
    message: &str,
    case: &str,
) {
    assert_eq!(
        answer.status, status,
        "case {case:?}: expected HTTP {status}, got {} {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        error_code,
        "case {case:?}: expected error code {error_code}, got {:?} ({:?})",
        answer.error_code(),
        answer.body
    );
    assert!(
        answer.error_type.is_some(),
        "case {case:?}: the refusal must carry the x-amzn-errortype header, got {:?}",
        answer.error_type
    );
    assert_eq!(
        answer.message(),
        message,
        "case {case:?}: expected message {message:?}, got {:?}",
        answer.message()
    );
}

// ---------------------------------------------------------------------------
// The place index plane
// ---------------------------------------------------------------------------

/// `CreatePlaceIndex`'s path, as the model binds it (`POST /places/v0/indexes`).
const CREATE_INDEX_PATH: &str = "/places/v0/indexes";

/// `DescribePlaceIndex`'s path, as the model binds it
/// (`GET /places/v0/indexes/{IndexName}`).
fn index_path(name: &str) -> String {
    format!("/places/v0/indexes/{name}")
}

/// The storage option a place index reports when its create named none: the
/// `DataSourceConfiguration$IntendedUse` doc's "Default value: SingleUse".
const DEFAULT_INTENDED_USE: &str = "SingleUse";

/// The `PricingPlan` a place index read reports: the
/// `DescribePlaceIndexResponse$PricingPlan` doc's "Always returns
/// RequestBasedUsage".
const INDEX_PRICING_PLAN: &str = "RequestBasedUsage";

/// One row of the place index table: the name and the optional members one create
/// carries, so what the index's own read reports can be compared against what the
/// create STORED rather than against the shape of one request.
struct IndexCase {
    case: &'static str,
    name: &'static str,
    /// `DataSource`, one of the three values the member doc publishes.
    data_source: &'static str,
    /// `Description` as the create sent it, or `None` when the request omits it.
    description: Option<String>,
    /// `DataSourceConfiguration$IntendedUse` as the create sent it, or `None` when
    /// the request omits the configuration entirely.
    intended_use: Option<&'static str>,
    /// `PricingPlan` as the create sent it: the one value the deprecated member
    /// still admits ("No longer used. If included, the only allowed value is
    /// RequestBasedUsage"), or absent.
    pricing_plan: Option<&'static str>,
    tags: Vec<(String, String)>,
}

/// The place index table. Rows differ along everything the rule generalises over:
/// the data provider (every value the `CreatePlaceIndexRequest$DataSource` doc
/// publishes), whether the request carries a description (absent, empty, ordinary
/// text, and the 1000-character bound `ResourceDescription` publishes), the storage
/// option (absent, and both members of the `IntendedUse` enum), whether the retired
/// `PricingPlan` is sent, how many tags the create carries, and the punctuation
/// `ResourceName`'s `^[-._\w]+$` admits in the name.
fn index_cases() -> Vec<IndexCase> {
    vec![
        IndexCase {
            case: "every optional member absent",
            name: "bare",
            data_source: "Esri",
            description: None,
            intended_use: None,
            pricing_plan: None,
            tags: Vec::new(),
        },
        IndexCase {
            case: "an explicit SingleUse configuration and one tag",
            name: "single.use_index-1",
            data_source: "Grab",
            description: Some("south-east asia geocoding".to_string()),
            intended_use: Some("SingleUse"),
            pricing_plan: None,
            tags: vec![("env".to_string(), "dev".to_string())],
        },
        IndexCase {
            case: "a Storage configuration, the retired pricing plan and three tags",
            name: "Storage_Index.2",
            data_source: "Here",
            // `ResourceDescription` is `len 0..1000`, so the empty string is a
            // description.
            description: Some(String::new()),
            intended_use: Some("Storage"),
            pricing_plan: Some("RequestBasedUsage"),
            tags: vec![
                ("cost.center".to_string(), "42".to_string()),
                ("owner".to_string(), String::new()),
                ("team".to_string(), "logistics".to_string()),
            ],
        },
        IndexCase {
            case: "a description at the longest length ResourceDescription admits",
            name: "d",
            data_source: "Esri",
            // `ResourceDescription` is `len 0..1000`: the published bound itself.
            description: Some("d".repeat(1000)),
            intended_use: Some("Storage"),
            pricing_plan: None,
            tags: Vec::new(),
        },
    ]
}

/// The create body one row sends: the two required members, plus exactly the
/// optional members the row names.
fn index_body(row: &IndexCase) -> Value {
    let mut body = json!({ "IndexName": row.name, "DataSource": row.data_source });
    if let Some(description) = &row.description {
        body["Description"] = json!(description);
    }
    if let Some(intended_use) = row.intended_use {
        body["DataSourceConfiguration"] = json!({ "IntendedUse": intended_use });
    }
    if let Some(plan) = row.pricing_plan {
        body["PricingPlan"] = json!(plan);
    }
    if !row.tags.is_empty() {
        body["Tags"] = tags_body(&row.tags);
    }
    body
}

/// RULE: a create persists the resource the request describes under the name it
/// names, and the resource's own read reports that STORED resource — the members the
/// request carried as sent, the members it omitted at their documented defaults, the
/// ARN the create minted, and the instant the create stamped — so the read answers
/// from state rather than from a request.
#[tokio::test]
async fn a_place_index_create_persists_what_its_describe_reports() {
    let router = emulator();
    let rows = index_cases();

    // Every index is created FIRST, so each read below happens on an engine holding
    // all of them: what a read reports is this index's stored resource, not the last
    // request's input.
    let mut created_bodies: Vec<Value> = Vec::new();
    for row in &rows {
        let created = call(&router, "POST", CREATE_INDEX_PATH, Some(index_body(row))).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: the create must succeed, got {} {:?}",
            row.case, created.status, created.body
        );
        let body = created.json();
        assert_eq!(
            body.get("IndexName").and_then(Value::as_str),
            Some(row.name),
            "case {:?}: expected the create to report IndexName {:?}, got {body}",
            row.case,
            row.name
        );
        let expected_arn = format!("arn:aws:geo:{REGION}:{ACCOUNT}:place-index/{}", row.name);
        assert_eq!(
            body.get("IndexArn").and_then(Value::as_str),
            Some(expected_arn.as_str()),
            "case {:?}: expected the create to report IndexArn {expected_arn}, got {body}",
            row.case
        );
        created_bodies.push(body);
    }

    for (row, created) in rows.iter().zip(&created_bodies) {
        let read = call(&router, "GET", &index_path(row.name), None).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: the read must succeed, got {} {:?}",
            row.case, read.status, read.body
        );
        let body = read.json();

        assert_eq!(
            body.get("IndexName").and_then(Value::as_str),
            Some(row.name),
            "case {:?}: expected the read to report IndexName {:?}, got {body}",
            row.case,
            row.name
        );
        assert_eq!(
            body.get("IndexArn"),
            created.get("IndexArn"),
            "case {:?}: expected the read to report the ARN the create minted ({:?}), got {body}",
            row.case,
            created.get("IndexArn")
        );
        assert_eq!(
            body.get("DataSource").and_then(Value::as_str),
            Some(row.data_source),
            "case {:?}: expected the read to report DataSource {:?}, got {body}",
            row.case,
            row.data_source
        );
        // `Description` is required on the read and `len 0..1000`, so an index
        // created without one reports the empty string rather than omitting it.
        let expected_description = row.description.clone().unwrap_or_default();
        assert_eq!(
            body.get("Description").and_then(Value::as_str),
            Some(expected_description.as_str()),
            "case {:?}: expected the read to report the stored Description ({} characters), got {:?}",
            row.case,
            expected_description.chars().count(),
            body.get("Description")
        );
        let expected_intended_use = row.intended_use.unwrap_or(DEFAULT_INTENDED_USE);
        assert_eq!(
            body.pointer("/DataSourceConfiguration/IntendedUse")
                .and_then(Value::as_str),
            Some(expected_intended_use),
            "case {:?}: expected IntendedUse {expected_intended_use:?}, got {body}",
            row.case
        );
        assert_eq!(
            body.get("PricingPlan").and_then(Value::as_str),
            Some(INDEX_PRICING_PLAN),
            "case {:?}: expected PricingPlan {INDEX_PRICING_PLAN:?}, got {body}",
            row.case
        );
        let mut expected_tags = row.tags.clone();
        expected_tags.sort();
        assert_eq!(
            reported_tags(&body),
            Some(expected_tags.clone()),
            "case {:?}: expected the Tags member to be the map {expected_tags:?}, got {:?}",
            row.case,
            body.get("Tags")
        );
        // The instants are the ones the create stamped, so a read reports stored
        // time rather than the time it was read at, and an index nobody has updated
        // reports the same instant twice.
        let created_at = millis_of(created, "CreateTime");
        assert_eq!(
            millis_of(&body, "CreateTime"),
            created_at,
            "case {:?}: expected the read to report the create's CreateTime, got {body}",
            row.case
        );
        assert_eq!(
            millis_of(&body, "UpdateTime"),
            created_at,
            "case {:?}: an index nobody has updated must report UpdateTime equal to CreateTime, got {body}",
            row.case
        );
    }
}

/// RULE: a generated ARN carries the region the request was addressed to, in the
/// `place-index/<name>` form the `IndexArn` member documents.
#[tokio::test]
async fn a_place_index_arn_carries_the_region_the_request_was_addressed_to() {
    for region in ["us-west-2", "eu-west-1", "ap-southeast-1"] {
        let router = emulator();
        let body = bytes::Bytes::from(
            serde_json::to_vec(&json!({ "IndexName": "regional", "DataSource": "Here" }))
                .expect("the test body serializes"),
        );
        let created = call_in_region(&router, "POST", CREATE_INDEX_PATH, body, region).await;
        assert_eq!(
            created.status, 200,
            "case {region:?}: the create must succeed, got {} {:?}",
            created.status, created.body
        );
        let expected = format!("arn:aws:geo:{region}:{ACCOUNT}:place-index/regional");
        assert_eq!(
            created.json().get("IndexArn").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {region:?}: expected IndexArn {expected}, got {:?}",
            created.body
        );

        let read = call_in_region(
            &router,
            "GET",
            &index_path("regional"),
            bytes::Bytes::new(),
            region,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {region:?}: the read must succeed, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.json().get("IndexArn").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {region:?}: expected the read to report the stored ARN {expected}, got {:?}",
            read.body
        );
    }
}

/// RULE: an index name is resolved exactly and is unique within the account, so a
/// name nothing is stored under is a 404 and a create of a name already held is a
/// 409 — and the index the account does hold is left exactly as it was.
#[tokio::test]
async fn a_place_index_name_is_resolved_exactly_and_held_once() {
    let router = emulator();
    let held = IndexCase {
        case: "the index that exists",
        name: "Exists",
        data_source: "Esri",
        description: Some("the original".to_string()),
        intended_use: Some("Storage"),
        pricing_plan: None,
        tags: vec![("generation".to_string(), "first".to_string())],
    };
    let created = call(&router, "POST", CREATE_INDEX_PATH, Some(index_body(&held))).await;
    assert_eq!(
        created.status, 200,
        "the positive control must be created, got {} {:?}",
        created.status, created.body
    );

    // Names that satisfy `ResourceName`'s `^[-._\w]+$` but name nothing, including
    // case variants of the one that does exist: the lookup is exact.
    for name in ["missing", "exists", "EXISTS", "Exists.2", "x", "a_b-c.1"] {
        assert_refused(
            &call(&router, "GET", &index_path(name), None).await,
            404,
            "ResourceNotFoundException",
            &format!("Place index not found: {name}."),
            &format!("read {name}"),
        );
    }

    // A second create of the held name is refused, whatever else it carries, and
    // changes nothing: the stored index still reports what the first create stored.
    let conflicting = IndexCase {
        case: "a second create of a held name",
        name: "Exists",
        data_source: "Grab",
        description: Some("the replacement".to_string()),
        intended_use: Some("SingleUse"),
        pricing_plan: None,
        tags: vec![("generation".to_string(), "second".to_string())],
    };
    assert_refused(
        &call(
            &router,
            "POST",
            CREATE_INDEX_PATH,
            Some(index_body(&conflicting)),
        )
        .await,
        409,
        "ConflictException",
        "Place index already exists: Exists.",
        "create a held name",
    );

    let read = call(&router, "GET", &index_path("Exists"), None).await;
    assert_eq!(
        read.status, 200,
        "the held index must still be readable, got {} {:?}",
        read.status, read.body
    );
    let body = read.json();
    assert_eq!(
        body.get("DataSource").and_then(Value::as_str),
        Some("Esri"),
        "a refused create must leave the stored DataSource alone, got {body}"
    );
    assert_eq!(
        body.get("Description").and_then(Value::as_str),
        Some("the original"),
        "a refused create must leave the stored Description alone, got {body}"
    );
    assert_eq!(
        reported_tags(&body),
        Some(vec![("generation".to_string(), "first".to_string())]),
        "a refused create must leave the stored tags alone, got {:?}",
        body.get("Tags")
    );
}
