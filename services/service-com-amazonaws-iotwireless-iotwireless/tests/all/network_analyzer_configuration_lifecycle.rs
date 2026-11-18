// SPDX-License-Identifier: CC0-1.0

//! The network analyzer configuration lifecycle, end to end over HTTP.
//!
//! Every case drives the real `restJson1` wire path — `MultiServiceRouter::dispatch`
//! with a signed `http::Request` — so what is asserted is what an SDK client sees, not
//! a handler return value.
//!
//! The rules under test, each asserted over a SET of inputs rather than one recorded
//! request:
//!
//! * `CreateNetworkAnalyzerConfiguration` persists the configuration it was given,
//!   keyed by `Name`, and mints its ARN in the region the request names, so
//!   `GetNetworkAnalyzerConfiguration` reports back every member that was stored —
//!   over the modelled `LogLevel`, `WirelessDeviceFrameInfo` and `MulticastFrameInfo`
//!   catalogs, both presence states of every optional member, and a name at its
//!   published length ceiling;
//! * a stored configuration is FULLY SPECIFIED: the create settles a description the
//!   request left out at the empty string and every trace setting it left out at that
//!   setting's default, so the reader reports all of them, at create and after an update
//!   that mentions neither;
//! * `DeleteNetworkAnalyzerConfiguration` removes the configuration, so every later
//!   operation that addresses the name is the modelled 404 and the name is free to claim
//!   again;
//! * a configuration holds a SET of tracked resources: an id given twice is tracked
//!   once, `UpdateNetworkAnalyzerConfiguration` adds the ids it is told to add and drops
//!   the ids it is told to remove, and a configuration that tracks nothing of a kind
//!   reports that kind as an empty list;
//! * a tracked id is a reference resolved against real state, so an id no resource holds
//!   is a 404 and the refused call persists nothing;
//! * `Name` identifies at most one configuration, so a second claimant is a 409, while an
//!   `@idempotencyToken` replay with the same parameters returns the first call's
//!   resource;
//! * the stored set cannot pass the ceiling the model publishes for the member that
//!   reports it, and the store itself is bounded and refuses once full.
//!
//! The wire harness is the one the sibling destination/device suite already has:
//! `tests/all/*.rs` are modules of ONE test target, so `router`, `call`, `error_code`
//! and `call_signed_for` are shared from there rather than copied.

use http::{Method, StatusCode};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The bounds the service enforces, so this suite asserts the limits the implementation
/// declares rather than numbers copied by hand.
use service_com_amazonaws_iotwireless_iotwireless::state::{
    DEFAULT_REGION, MAX_NETWORK_ANALYZER_CONFIGURATIONS, MAX_TRACKED_MULTICAST_GROUPS,
    MAX_TRACKED_WIRELESS_DEVICES,
};

use super::destination_and_wireless_device_lifecycle::{call, error_code, router, ACCOUNT, REGION};
use super::multicast_group_lifecycle::call_signed_for;
use super::multicast_group_update_and_delete::{call_with_headers, Answer};

/// `LogLevel`'s modelled values — the model-generated constant, so the suite and the
/// service cannot drift apart.
const LOG_LEVELS: &[&str] =
    service_com_amazonaws_iotwireless_iotwireless::ops::types::LOG_LEVEL_VALUES;

/// `WirelessDeviceFrameInfo`'s modelled values, from the same generated source.
const WIRELESS_DEVICE_FRAME_INFOS: &[&str] =
    service_com_amazonaws_iotwireless_iotwireless::ops::types::WIRELESS_DEVICE_FRAME_INFO_VALUES;

/// `MulticastFrameInfo`'s modelled values, from the same generated source.
const MULTICAST_FRAME_INFOS: &[&str] =
    service_com_amazonaws_iotwireless_iotwireless::ops::types::MULTICAST_FRAME_INFO_VALUES;

/// `NetworkAnalyzerConfigurationName`'s published length ceiling.
/// Source: `smithy_query(query="operation",
/// operation="CreateNetworkAnalyzerConfiguration")`.
const NAME_MAX_LEN: usize = 1024;

/// The setting a trace-content member carries when the create left it unset: tracing off.
/// Source: `smithy_query(query="shape", shape="TraceContent")` — `DISABLED` is the one
/// value `WirelessDeviceFrameInfo` (ENABLED | DISABLED), `LogLevel`
/// (INFO | ERROR | DISABLED) and `MulticastFrameInfo` (ENABLED | DISABLED) all carry.
const UNSET_TRACE_SETTING: &str = "DISABLED";

/// The trace content a stored configuration must report for the `TraceContent` a create
/// carried: every member the request set, and [`UNSET_TRACE_SETTING`] for every member it
/// left unset — including a create that carried no `TraceContent` at all.
fn settled_trace_content(requested: Option<&Value>) -> Value {
    let member = |name: &str| {
        requested
            .and_then(|content| content.get(name).cloned())
            .unwrap_or_else(|| json!(UNSET_TRACE_SETTING))
    };
    json!({
        "WirelessDeviceFrameInfo": member("WirelessDeviceFrameInfo"),
        "LogLevel": member("LogLevel"),
        "MulticastFrameInfo": member("MulticastFrameInfo"),
    })
}

/// The sentence this service words the configuration not-found refusal with, around the
/// `ConfigurationName` the refused request addressed.
fn not_found_message(name: &str) -> String {
    format!("Configuration {name} is not exist")
}

async fn delete_config(router: &MultiServiceRouter, name: &str) -> Answer {
    call_with_headers(
        router,
        Method::DELETE,
        &format!("/network-analyzer-configurations/{name}"),
        None,
    )
    .await
}

/// The refusal every configuration-addressing operation gives for a `Name` the account
/// does not hold: 404 `ResourceNotFoundException`, the error shape named in
/// `x-amzn-errortype`, and the service's sentence for this error class around the
/// requested name.
fn assert_not_found(case: &str, operation: &str, name: &str, answered: &Answer) {
    assert_eq!(
        answered.status,
        StatusCode::NOT_FOUND,
        "case {case:?}: expected 404 from {operation}, got {} {}",
        answered.status,
        answered.body
    );
    assert_eq!(
        error_code(&answered.body),
        "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException from {operation}, got {}",
        answered.body
    );
    assert_eq!(
        answered.error_type, "ResourceNotFoundException",
        "case {case:?}: expected {operation} to name the error shape in x-amzn-errortype, \
         got {:?}",
        answered.error_type
    );
    let expected = not_found_message(name);
    assert_eq!(
        answered.body["Message"].as_str(),
        Some(expected.as_str()),
        "case {case:?}: expected the not-found sentence {expected:?} from {operation}, got {}",
        answered.body
    );
}

async fn create_config(router: &MultiServiceRouter, body: Value) -> (StatusCode, Value) {
    call(
        router,
        Method::POST,
        "/network-analyzer-configurations",
        Some(body),
    )
    .await
}

async fn get_config(router: &MultiServiceRouter, name: &str) -> (StatusCode, Value) {
    call(
        router,
        Method::GET,
        &format!("/network-analyzer-configurations/{name}"),
        None,
    )
    .await
}

async fn update_config(
    router: &MultiServiceRouter,
    name: &str,
    body: Value,
) -> (StatusCode, Value) {
    call(
        router,
        Method::PATCH,
        &format!("/network-analyzer-configurations/{name}"),
        Some(body),
    )
    .await
}

/// Create one destination, so the devices seeded below have something to register
/// against.
async fn seed_destination(router: &MultiServiceRouter, name: &str) {
    let (status, body) = call(
        router,
        Method::POST,
        "/destinations",
        Some(json!({
            "Name": name,
            "ExpressionType": "RuleName",
            "Expression": format!("rule-for-{name}"),
            "RoleArn": "arn:aws:iam::000000000000:role/iotwireless-destination",
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "seeding destination {name}: expected 201, got {status} {body}"
    );
}

/// Create `count` wireless devices and return their ids.
async fn seed_devices(router: &MultiServiceRouter, destination: &str, count: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        let (status, created) = call(
            router,
            Method::POST,
            "/wireless-devices",
            Some(json!({
                "Type": "LoRaWAN",
                "Name": format!("device-{index}"),
                "DestinationName": destination,
            })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "seeding wireless device {index}: expected 201, got {status} {created}"
        );
        ids.push(
            created["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("seeding wireless device {index}: no Id in {created}"))
                .to_string(),
        );
    }
    ids
}

/// Create `count` wireless gateways and return their ids.
async fn seed_gateways(router: &MultiServiceRouter, count: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        let (status, created) = call(
            router,
            Method::POST,
            "/wireless-gateways",
            Some(json!({ "LoRaWAN": { "RfRegion": "EU868" } })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "seeding wireless gateway {index}: expected 201, got {status} {created}"
        );
        ids.push(
            created["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("seeding wireless gateway {index}: no Id in {created}"))
                .to_string(),
        );
    }
    ids
}

/// Create `count` multicast groups and return their ids.
async fn seed_groups(router: &MultiServiceRouter, count: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        let (status, created) = call(
            router,
            Method::POST,
            "/multicast-groups",
            Some(json!({ "LoRaWAN": { "RfRegion": "US915" } })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "seeding multicast group {index}: expected 201, got {status} {created}"
        );
        ids.push(
            created["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("seeding multicast group {index}: no Id in {created}"))
                .to_string(),
        );
    }
    ids
}

/// The ids in the order they first appear, each one once — what a configuration tracks
/// after being told to track `sent`.
fn tracked_set(sent: &[String]) -> Vec<String> {
    let mut set: Vec<String> = Vec::new();
    for id in sent {
        if !set.contains(id) {
            set.push(id.clone());
        }
    }
    set
}

/// The reader must report `member` as exactly `expected` — including as an empty list
/// when the configuration tracks nothing of that kind, which is an answer about state
/// rather than a member to omit.
fn assert_tracked(case: &str, member: &str, expected: &[String], fetched: &Value) {
    let expected: Value = json!(expected);
    let actual = fetched.get(member);
    assert_eq!(
        actual,
        Some(&expected),
        "case {case:?}: expected {member} = {expected}, got {actual:?}"
    );
}

/// One create and the read that must report exactly what it stored.
struct Case {
    case: &'static str,
    name: String,
    description: Option<String>,
    trace_content: Option<Value>,
    tags: Option<Value>,
    /// The tracked-resource ids as SENT, repeats included.
    devices: Vec<String>,
    gateways: Vec<String>,
    groups: Vec<String>,
    /// Whether an empty tracked-resource list is sent as `[]` rather than omitted.
    send_empty_lists: bool,
}

/// A create persists the configuration and `GetNetworkAnalyzerConfiguration` reports
/// exactly what was stored, across both presence states of every optional member, the
/// modelled trace-content catalogs, a name at its published ceiling, and ids repeated in
/// one request.
#[tokio::test]
async fn a_network_analyzer_configuration_round_trips_through_get() {
    let router = router();
    seed_destination(&router, "NacRoundTripDest").await;
    let devices = seed_devices(&router, "NacRoundTripDest", 3).await;
    let gateways = seed_gateways(&router, 2).await;
    let groups = seed_groups(&router, 2).await;

    let mut cases = vec![
        // Nothing but the required member: the stored configuration is still fully
        // specified, so the reader reports a description and all three trace settings.
        Case {
            case: "name-only",
            name: "NacNameOnly".to_string(),
            description: None,
            trace_content: None,
            tags: None,
            devices: Vec::new(),
            gateways: Vec::new(),
            groups: Vec::new(),
            send_empty_lists: false,
        },
        // The whole member set, tracking every kind of resource at once.
        Case {
            case: "full-member-set",
            name: "Nac_full-member-set".to_string(),
            description: Some("the whole member set".to_string()),
            trace_content: Some(json!({
                "LogLevel": LOG_LEVELS[0],
                "WirelessDeviceFrameInfo": WIRELESS_DEVICE_FRAME_INFOS[0],
                "MulticastFrameInfo": MULTICAST_FRAME_INFOS[0],
            })),
            tags: Some(json!([{ "Key": "stage", "Value": "test" }])),
            devices: devices.clone(),
            gateways: gateways.clone(),
            groups: groups.clone(),
            send_empty_lists: false,
        },
        // The lists sent EMPTY, and an empty description: present-and-empty is not the
        // same request as absent, and both must read back as the service stored them.
        Case {
            case: "empty-lists-and-description",
            name: "NacEmptyLists".to_string(),
            description: Some(String::new()),
            trace_content: Some(json!({ "LogLevel": LOG_LEVELS[1] })),
            tags: Some(json!([])),
            devices: Vec::new(),
            gateways: Vec::new(),
            groups: Vec::new(),
            send_empty_lists: true,
        },
        // Ids repeated within one request: a configuration tracks a SET, so each id is
        // tracked once, in the order it first appeared.
        Case {
            case: "repeated-ids",
            name: "NacRepeatedIds".to_string(),
            description: None,
            trace_content: None,
            tags: None,
            devices: vec![
                devices[1].clone(),
                devices[0].clone(),
                devices[1].clone(),
                devices[0].clone(),
            ],
            gateways: vec![gateways[0].clone(), gateways[0].clone()],
            groups: vec![groups[1].clone(), groups[1].clone()],
            send_empty_lists: false,
        },
        // A name at its published length ceiling, tracking one device.
        Case {
            case: "name-at-ceiling",
            name: "x".repeat(NAME_MAX_LEN),
            description: Some("y".repeat(64)),
            trace_content: None,
            tags: None,
            devices: vec![devices[2].clone()],
            gateways: Vec::new(),
            groups: Vec::new(),
            send_empty_lists: false,
        },
    ];

    // One case per entry of each modelled trace-content catalog, so no value in them is
    // left untested — including the entries the issue's unit never cites.
    for (index, log_level) in LOG_LEVELS.iter().enumerate() {
        let device_frame_info = WIRELESS_DEVICE_FRAME_INFOS[index % WIRELESS_DEVICE_FRAME_INFOS.len()];
        let multicast_frame_info = MULTICAST_FRAME_INFOS[index % MULTICAST_FRAME_INFOS.len()];
        cases.push(Case {
            case: "trace-content-catalog",
            name: format!("NacTrace_{log_level}"),
            description: None,
            trace_content: Some(json!({
                "LogLevel": log_level,
                "WirelessDeviceFrameInfo": device_frame_info,
                "MulticastFrameInfo": multicast_frame_info,
            })),
            tags: None,
            devices: Vec::new(),
            gateways: Vec::new(),
            groups: Vec::new(),
            send_empty_lists: false,
        });
    }

    for case in &cases {
        let mut body = json!({ "Name": case.name });
        if let Some(description) = &case.description {
            body["Description"] = json!(description);
        }
        if let Some(trace_content) = &case.trace_content {
            body["TraceContent"] = trace_content.clone();
        }
        if let Some(tags) = &case.tags {
            body["Tags"] = tags.clone();
        }
        if case.send_empty_lists || !case.devices.is_empty() {
            body["WirelessDevices"] = json!(case.devices);
        }
        if case.send_empty_lists || !case.gateways.is_empty() {
            body["WirelessGateways"] = json!(case.gateways);
        }
        if case.send_empty_lists || !case.groups.is_empty() {
            body["MulticastGroups"] = json!(case.groups);
        }

        let (status, created) = create_config(&router, body).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "case {:?}: expected 201, got {status} {created}",
            case.case
        );
        let expected_arn = format!(
            "arn:aws:iotwireless:{REGION}:{ACCOUNT}:NetworkAnalyzerConfiguration/{}",
            case.name
        );
        assert_eq!(
            created["Arn"],
            json!(expected_arn),
            "case {:?}: expected Arn {expected_arn}, got {:?}",
            case.case,
            created["Arn"]
        );
        assert_eq!(
            created["Name"],
            json!(case.name),
            "case {:?}: expected Name {:?}, got {:?}",
            case.case,
            case.name,
            created["Name"]
        );

        let (status, fetched) = get_config(&router, &case.name).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "case {:?}: expected 200 from GetNetworkAnalyzerConfiguration, got {status} \
             {fetched}",
            case.case
        );
        assert_eq!(
            fetched["Arn"],
            json!(expected_arn),
            "case {:?}: the reader's Arn must match the created ARN, got {:?}",
            case.case,
            fetched["Arn"]
        );
        assert_eq!(
            fetched["Name"],
            json!(case.name),
            "case {:?}: expected Name {:?}, got {:?}",
            case.case,
            case.name,
            fetched["Name"]
        );
        // A stored configuration is fully specified, so the reader reports a description
        // for one the create never set (the empty string) and all three trace settings,
        // the ones this create left unset at their default.
        let expected_description = case.description.clone().unwrap_or_default();
        assert_eq!(
            fetched.get("Description").cloned(),
            Some(json!(expected_description)),
            "case {:?}: expected Description {expected_description:?}, got {:?}",
            case.case,
            fetched.get("Description")
        );
        let expected_trace = settled_trace_content(case.trace_content.as_ref());
        assert_eq!(
            fetched.get("TraceContent").cloned(),
            Some(expected_trace.clone()),
            "case {:?}: expected TraceContent {expected_trace}, got {:?}",
            case.case,
            fetched.get("TraceContent")
        );
        assert_tracked(
            case.case,
            "WirelessDevices",
            &tracked_set(&case.devices),
            &fetched,
        );
        assert_tracked(
            case.case,
            "WirelessGateways",
            &tracked_set(&case.gateways),
            &fetched,
        );
        assert_tracked(
            case.case,
            "MulticastGroups",
            &tracked_set(&case.groups),
            &fetched,
        );
    }
}

/// One `UpdateNetworkAnalyzerConfiguration` call and the tracked sets it must leave
/// behind.
struct Step {
    case: &'static str,
    /// The add/remove members of the PATCH, as `(member, ids)`.
    patch: Vec<(&'static str, Vec<String>)>,
    expected_devices: Vec<String>,
    expected_gateways: Vec<String>,
    expected_groups: Vec<String>,
}

/// An update adds and removes the resources a configuration tracks, and every later read
/// answers from the stored set: adding an id already tracked changes nothing, removing an
/// id the configuration does not track changes nothing, an id named in both halves of one
/// call ends up untracked, and a list emptied by removals reads back as empty.
#[tokio::test]
async fn an_update_adds_and_removes_the_resources_a_configuration_tracks() {
    let router = router();
    seed_destination(&router, "NacUpdateDest").await;
    let devices = seed_devices(&router, "NacUpdateDest", 4).await;
    let gateways = seed_gateways(&router, 2).await;
    let groups = seed_groups(&router, 2).await;

    let name = "NacTracking";
    let (status, created) = create_config(&router, json!({ "Name": name })).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "seeding the configuration: expected 201, got {status} {created}"
    );

    let steps = vec![
        Step {
            case: "add two devices",
            patch: vec![(
                "WirelessDevicesToAdd",
                vec![devices[0].clone(), devices[1].clone()],
            )],
            expected_devices: vec![devices[0].clone(), devices[1].clone()],
            expected_gateways: Vec::new(),
            expected_groups: Vec::new(),
        },
        Step {
            case: "add a third device and a gateway in one call",
            patch: vec![
                ("WirelessDevicesToAdd", vec![devices[2].clone()]),
                ("WirelessGatewaysToAdd", vec![gateways[0].clone()]),
            ],
            expected_devices: vec![
                devices[0].clone(),
                devices[1].clone(),
                devices[2].clone(),
            ],
            expected_gateways: vec![gateways[0].clone()],
            expected_groups: Vec::new(),
        },
        Step {
            case: "re-add a device already tracked",
            patch: vec![(
                "WirelessDevicesToAdd",
                vec![devices[1].clone(), devices[1].clone()],
            )],
            expected_devices: vec![
                devices[0].clone(),
                devices[1].clone(),
                devices[2].clone(),
            ],
            expected_gateways: vec![gateways[0].clone()],
            expected_groups: Vec::new(),
        },
        Step {
            case: "remove one tracked device",
            patch: vec![("WirelessDevicesToRemove", vec![devices[1].clone()])],
            expected_devices: vec![devices[0].clone(), devices[2].clone()],
            expected_gateways: vec![gateways[0].clone()],
            expected_groups: Vec::new(),
        },
        Step {
            case: "remove a device the configuration does not track",
            patch: vec![("WirelessDevicesToRemove", vec![devices[3].clone()])],
            expected_devices: vec![devices[0].clone(), devices[2].clone()],
            expected_gateways: vec![gateways[0].clone()],
            expected_groups: Vec::new(),
        },
        Step {
            case: "add and remove the same device in one call",
            patch: vec![
                ("WirelessDevicesToAdd", vec![devices[3].clone()]),
                ("WirelessDevicesToRemove", vec![devices[3].clone()]),
            ],
            expected_devices: vec![devices[0].clone(), devices[2].clone()],
            expected_gateways: vec![gateways[0].clone()],
            expected_groups: Vec::new(),
        },
        Step {
            case: "add both multicast groups and a second gateway",
            patch: vec![
                ("MulticastGroupsToAdd", groups.clone()),
                ("WirelessGatewaysToAdd", vec![gateways[1].clone()]),
            ],
            expected_devices: vec![devices[0].clone(), devices[2].clone()],
            expected_gateways: vec![gateways[0].clone(), gateways[1].clone()],
            expected_groups: groups.clone(),
        },
        Step {
            case: "remove one multicast group and one gateway",
            patch: vec![
                ("MulticastGroupsToRemove", vec![groups[0].clone()]),
                ("WirelessGatewaysToRemove", vec![gateways[0].clone()]),
            ],
            expected_devices: vec![devices[0].clone(), devices[2].clone()],
            expected_gateways: vec![gateways[1].clone()],
            expected_groups: vec![groups[1].clone()],
        },
        Step {
            case: "remove everything still tracked",
            patch: vec![
                (
                    "WirelessDevicesToRemove",
                    vec![devices[0].clone(), devices[2].clone()],
                ),
                ("WirelessGatewaysToRemove", vec![gateways[1].clone()]),
                ("MulticastGroupsToRemove", vec![groups[1].clone()]),
            ],
            expected_devices: Vec::new(),
            expected_gateways: Vec::new(),
            expected_groups: Vec::new(),
        },
    ];

    for step in &steps {
        let mut body = json!({});
        for (member, ids) in &step.patch {
            body[*member] = json!(ids);
        }
        let (status, answered) = update_config(&router, name, body).await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "case {:?}: expected 204 from UpdateNetworkAnalyzerConfiguration, got {status} \
             {answered}",
            step.case
        );

        let (status, fetched) = get_config(&router, name).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "case {:?}: expected 200 from the reader, got {status} {fetched}",
            step.case
        );
        assert_tracked(step.case, "WirelessDevices", &step.expected_devices, &fetched);
        assert_tracked(
            step.case,
            "WirelessGateways",
            &step.expected_gateways,
            &fetched,
        );
        assert_tracked(step.case, "MulticastGroups", &step.expected_groups, &fetched);
    }
}

/// An update replaces only the trace-content members the request carries, and leaves the
/// ones it omits as they were stored.
#[tokio::test]
async fn an_update_replaces_only_the_members_the_request_carries() {
    let router = router();
    let name = "NacTraceMerge";
    let (status, created) = create_config(
        &router,
        json!({
            "Name": name,
            "Description": "before",
            "TraceContent": {
                "LogLevel": LOG_LEVELS[0],
                "WirelessDeviceFrameInfo": WIRELESS_DEVICE_FRAME_INFOS[0],
                "MulticastFrameInfo": MULTICAST_FRAME_INFOS[0],
            },
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "seeding the configuration: expected 201, got {status} {created}"
    );

    // (case, the PATCH body, the TraceContent and Description the reader must report)
    let cases = vec![
        (
            "one trace member",
            json!({ "TraceContent": { "LogLevel": LOG_LEVELS[2] } }),
            json!({
                "LogLevel": LOG_LEVELS[2],
                "WirelessDeviceFrameInfo": WIRELESS_DEVICE_FRAME_INFOS[0],
                "MulticastFrameInfo": MULTICAST_FRAME_INFOS[0],
            }),
            json!("before"),
        ),
        (
            "the description alone",
            json!({ "Description": "after" }),
            json!({
                "LogLevel": LOG_LEVELS[2],
                "WirelessDeviceFrameInfo": WIRELESS_DEVICE_FRAME_INFOS[0],
                "MulticastFrameInfo": MULTICAST_FRAME_INFOS[0],
            }),
            json!("after"),
        ),
        (
            "two trace members and the description",
            json!({
                "Description": "",
                "TraceContent": {
                    "WirelessDeviceFrameInfo": WIRELESS_DEVICE_FRAME_INFOS[1],
                    "MulticastFrameInfo": MULTICAST_FRAME_INFOS[1],
                },
            }),
            json!({
                "LogLevel": LOG_LEVELS[2],
                "WirelessDeviceFrameInfo": WIRELESS_DEVICE_FRAME_INFOS[1],
                "MulticastFrameInfo": MULTICAST_FRAME_INFOS[1],
            }),
            json!(""),
        ),
        (
            "nothing at all",
            json!({}),
            json!({
                "LogLevel": LOG_LEVELS[2],
                "WirelessDeviceFrameInfo": WIRELESS_DEVICE_FRAME_INFOS[1],
                "MulticastFrameInfo": MULTICAST_FRAME_INFOS[1],
            }),
            json!(""),
        ),
    ];

    for (case, patch, expected_trace, expected_description) in cases {
        let (status, answered) = update_config(&router, name, patch).await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "case {case:?}: expected 204, got {status} {answered}"
        );
        let (_, fetched) = get_config(&router, name).await;
        assert_eq!(
            fetched["TraceContent"], expected_trace,
            "case {case:?}: expected TraceContent {expected_trace}, got {:?}",
            fetched["TraceContent"]
        );
        assert_eq!(
            fetched["Description"], expected_description,
            "case {case:?}: expected Description {expected_description}, got {:?}",
            fetched["Description"]
        );
    }
}

/// A trace setting the service settled at create survives an update that does not mention
/// it, and the reader keeps reporting all three settings — so a caller can confirm a
/// setting it applied, and an update meant to preserve one is not indistinguishable from
/// one that cleared it.
#[tokio::test]
async fn a_settled_trace_setting_survives_an_update_that_does_not_mention_it() {
    let router = router();

    // (case, the TraceContent the create carries, the PATCH body that follows it)
    let cases: Vec<(&str, Option<Value>, Value)> = vec![
        (
            "no trace content at all, the description patched",
            None,
            json!({ "Description": "patched" }),
        ),
        (
            "the log level alone, the description patched",
            Some(json!({ "LogLevel": LOG_LEVELS[1] })),
            json!({ "Description": "patched" }),
        ),
        (
            "the device frame info alone, the log level patched",
            Some(json!({ "WirelessDeviceFrameInfo": WIRELESS_DEVICE_FRAME_INFOS[1] })),
            json!({ "TraceContent": { "LogLevel": LOG_LEVELS[2] } }),
        ),
        (
            "the multicast frame info alone, nothing patched",
            Some(json!({ "MulticastFrameInfo": MULTICAST_FRAME_INFOS[1] })),
            json!({}),
        ),
        (
            "two settings, the third patched",
            Some(json!({
                "LogLevel": LOG_LEVELS[0],
                "WirelessDeviceFrameInfo": WIRELESS_DEVICE_FRAME_INFOS[1],
            })),
            json!({ "TraceContent": { "MulticastFrameInfo": MULTICAST_FRAME_INFOS[1] } }),
        ),
    ];

    for (index, (case, trace_content, patch)) in cases.iter().enumerate() {
        let name = format!("NacSettled{index}");
        let mut body = json!({ "Name": name });
        if let Some(trace_content) = trace_content {
            body["TraceContent"] = trace_content.clone();
        }
        let (status, created) = create_config(&router, body).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "case {case:?}: expected 201, got {status} {created}"
        );

        let settled = settled_trace_content(trace_content.as_ref());
        let (_, fetched) = get_config(&router, &name).await;
        assert_eq!(
            fetched["TraceContent"], settled,
            "case {case:?}: expected the settled TraceContent {settled} right after the \
             create, got {:?}",
            fetched["TraceContent"]
        );

        let (status, answered) = update_config(&router, &name, patch.clone()).await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "case {case:?}: expected 204 from the patch, got {status} {answered}"
        );

        // Only the settings the patch mentioned move; the rest keep the value the create
        // settled them at.
        let mut expected = settled.clone();
        if let Some(patched) = patch.get("TraceContent").and_then(Value::as_object) {
            for (member, value) in patched {
                expected[member] = value.clone();
            }
        }
        let expected_description = patch
            .get("Description")
            .cloned()
            .unwrap_or_else(|| json!(""));
        let (_, fetched) = get_config(&router, &name).await;
        assert_eq!(
            fetched["TraceContent"], expected,
            "case {case:?}: expected TraceContent {expected} after the patch, got {:?}",
            fetched["TraceContent"]
        );
        assert_eq!(
            fetched["Description"], expected_description,
            "case {case:?}: expected Description {expected_description} after the patch, \
             got {:?}",
            fetched["Description"]
        );
    }
}

/// Every tracked id is a reference resolved against real state: an id no resource holds
/// is the modelled 404, whether the create or the update names it, and the refused call
/// persists nothing.
#[tokio::test]
async fn a_tracked_id_is_resolved_against_real_state() {
    let router = router();
    seed_destination(&router, "NacRefusalDest").await;
    let devices = seed_devices(&router, "NacRefusalDest", 1).await;
    let gateways = seed_gateways(&router, 1).await;
    let groups = seed_groups(&router, 1).await;
    let absent = "00000000-0000-4000-8000-000000000000";

    // (case, the member that names an id nothing holds, the create body)
    let creates = vec![
        (
            "an unknown wireless device",
            json!({
                "Name": "NacUnknownDevice",
                "WirelessDevices": [devices[0].clone(), absent],
            }),
        ),
        (
            "an unknown wireless gateway",
            json!({
                "Name": "NacUnknownGateway",
                "WirelessGateways": [gateways[0].clone(), absent],
            }),
        ),
        (
            "an unknown multicast group",
            json!({
                "Name": "NacUnknownGroup",
                "MulticastGroups": [groups[0].clone(), absent],
            }),
        ),
    ];

    for (case, body) in creates {
        let name = body["Name"].as_str().expect("a Name").to_string();
        let (status, refused) = create_config(&router, body).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "case {case:?}: expected 404, got {status} {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {refused}"
        );
        let (status, fetched) = get_config(&router, &name).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "case {case:?}: the refused create must persist nothing, got {status} {fetched}"
        );
    }

    let name = "NacResolvedTracking";
    let (status, created) = create_config(
        &router,
        json!({ "Name": name, "WirelessDevices": [devices[0].clone()] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "seeding the configuration: expected 201, got {status} {created}"
    );

    // (case, the PATCH body naming an id nothing holds)
    let updates = vec![
        (
            "adding an unknown wireless device",
            json!({ "WirelessDevicesToAdd": [absent] }),
        ),
        (
            "adding an unknown wireless gateway",
            json!({ "WirelessGatewaysToAdd": [absent] }),
        ),
        (
            "adding an unknown multicast group",
            json!({ "MulticastGroupsToAdd": [absent] }),
        ),
    ];

    for (case, patch) in updates {
        let (status, refused) = update_config(&router, name, patch).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "case {case:?}: expected 404, got {status} {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {refused}"
        );
        let (_, fetched) = get_config(&router, name).await;
        assert_tracked(case, "WirelessDevices", &devices, &fetched);
        assert_tracked(case, "WirelessGateways", &[], &fetched);
        assert_tracked(case, "MulticastGroups", &[], &fetched);
    }

    // Removing an id nothing holds is NOT a refusal: a caller may stop tracking a
    // resource that has since been deleted.
    let (status, answered) = update_config(
        &router,
        name,
        json!({ "WirelessDevicesToRemove": [absent, devices[0].clone()] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "case \"removing an unknown id\": expected 204, got {status} {answered}"
    );
    let (_, fetched) = get_config(&router, name).await;
    assert_tracked("removing an unknown id", "WirelessDevices", &[], &fetched);
}

/// A name the store does not hold is the modelled 404 from every operation that addresses
/// a configuration by it, worded with the service's sentence for this error class around
/// the requested name.
#[tokio::test]
async fn a_name_the_store_does_not_hold_is_a_404() {
    let router = router();

    for case in ["NacMissing", "x", &"n".repeat(NAME_MAX_LEN)] {
        let answered = call_with_headers(
            &router,
            Method::GET,
            &format!("/network-analyzer-configurations/{case}"),
            None,
        )
        .await;
        assert_not_found(case, "GetNetworkAnalyzerConfiguration", case, &answered);

        let answered = call_with_headers(
            &router,
            Method::PATCH,
            &format!("/network-analyzer-configurations/{case}"),
            Some(json!({ "Description": "whatever" })),
        )
        .await;
        assert_not_found(case, "UpdateNetworkAnalyzerConfiguration", case, &answered);

        let answered = delete_config(&router, case).await;
        assert_not_found(case, "DeleteNetworkAnalyzerConfiguration", case, &answered);
    }
}

/// A delete is observable: the configuration is gone from every operation that addresses
/// it, the configurations it was stored beside are untouched, and the name is free for a
/// fresh create.
#[tokio::test]
async fn deleting_a_configuration_is_observable_by_the_next_read() {
    let router = router();
    seed_destination(&router, "NacDeleteDest").await;
    let devices = seed_devices(&router, "NacDeleteDest", 1).await;

    // (case, the create body, whether the configuration is patched before being deleted)
    let cases: Vec<(&str, Value, bool)> = vec![
        (
            "every member set",
            json!({
                "Name": "NacDeleteFull",
                "Description": "delete me",
                "TraceContent": {
                    "LogLevel": LOG_LEVELS[0],
                    "WirelessDeviceFrameInfo": WIRELESS_DEVICE_FRAME_INFOS[0],
                    "MulticastFrameInfo": MULTICAST_FRAME_INFOS[0],
                },
                "WirelessDevices": [devices[0].clone()],
            }),
            false,
        ),
        (
            "the required member alone",
            json!({ "Name": "NacDeleteNameOnly" }),
            false,
        ),
        (
            "patched before the delete",
            json!({ "Name": "Nac_Delete_Patched", "Description": "before" }),
            true,
        ),
        (
            "name at the ceiling",
            json!({ "Name": "d".repeat(NAME_MAX_LEN) }),
            false,
        ),
    ];

    // One configuration nothing in this test deletes: a delete removes the name it
    // addresses and nothing else.
    let (status, kept) = create_config(&router, json!({ "Name": "NacDeleteKept" })).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "seeding the kept configuration: expected 201, got {status} {kept}"
    );

    for (case, body, patch_first) in &cases {
        let name = body["Name"].as_str().expect("a Name").to_string();
        let (status, created) = create_config(&router, body.clone()).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "case {case:?}: expected 201, got {status} {created}"
        );
        if *patch_first {
            let (status, patched) =
                update_config(&router, &name, json!({ "Description": "after" })).await;
            assert_eq!(
                status,
                StatusCode::NO_CONTENT,
                "case {case:?}: expected 204 from the patch, got {status} {patched}"
            );
        }

        let answered = delete_config(&router, &name).await;
        assert_eq!(
            answered.status,
            StatusCode::NO_CONTENT,
            "case {case:?}: expected 204 from the delete, got {} {}",
            answered.status,
            answered.body
        );

        // Every operation that addresses the deleted name now refuses it, and a second
        // delete is refused too: the record is gone, not tombstoned into a live answer.
        let answered = call_with_headers(
            &router,
            Method::GET,
            &format!("/network-analyzer-configurations/{name}"),
            None,
        )
        .await;
        assert_not_found(case, "GetNetworkAnalyzerConfiguration", &name, &answered);
        let answered = call_with_headers(
            &router,
            Method::PATCH,
            &format!("/network-analyzer-configurations/{name}"),
            Some(json!({ "Description": "ghost" })),
        )
        .await;
        assert_not_found(case, "UpdateNetworkAnalyzerConfiguration", &name, &answered);
        let answered = delete_config(&router, &name).await;
        assert_not_found(case, "DeleteNetworkAnalyzerConfiguration", &name, &answered);

        // The name is free again, and what the fresh create stores is all the reader
        // reports: nothing of the deleted configuration survives under the same name.
        let (status, recreated) = create_config(&router, json!({ "Name": name })).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "case {case:?}: the name must be free after the delete, got {status} {recreated}"
        );
        let (status, fetched) = get_config(&router, &name).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "case {case:?}: expected 200 from the re-created configuration, got {status} \
             {fetched}"
        );
        assert_eq!(
            fetched["Description"],
            json!(""),
            "case {case:?}: the re-created configuration must carry its own description, \
             got {:?}",
            fetched["Description"]
        );
        assert_tracked(case, "WirelessDevices", &[], &fetched);
    }

    let (status, fetched) = get_config(&router, "NacDeleteKept").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the configuration no call deleted must still resolve, got {status} {fetched}"
    );
}

/// A deleted configuration releases the `ClientRequestToken` that minted it: the token
/// names no live resource any more, so replaying it creates a configuration again rather
/// than replaying an answer about one that is gone.
#[tokio::test]
async fn a_deleted_configuration_releases_its_idempotency_token() {
    let router = router();

    for (case, token) in [
        ("hyphenated token", "nac-delete-token-1"),
        ("token with underscores", "nac_delete_token_2"),
    ] {
        let name = format!("NacTokenDelete{}", token.replace(['-', '_'], ""));
        let body = json!({
            "Name": name,
            "Description": "tokened",
            "ClientRequestToken": token,
        });
        let (status, first) = create_config(&router, body.clone()).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "case {case:?}: expected 201, got {status} {first}"
        );

        let answered = delete_config(&router, &name).await;
        assert_eq!(
            answered.status,
            StatusCode::NO_CONTENT,
            "case {case:?}: expected 204 from the delete, got {} {}",
            answered.status,
            answered.body
        );

        let (status, replayed) = create_config(&router, body.clone()).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "case {case:?}: replaying the token of a deleted configuration must create it \
             again, got {status} {replayed}"
        );
        let (status, fetched) = get_config(&router, &name).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "case {case:?}: expected 200 after the replay, got {status} {fetched}"
        );
        assert_eq!(
            fetched["Description"],
            json!("tokened"),
            "case {case:?}: expected the replayed create's description, got {:?}",
            fetched["Description"]
        );
    }
}

/// `Name` identifies at most one configuration, so a second claimant is refused rather
/// than overwriting the first — and an `@idempotencyToken` replay with the same
/// parameters is the first call's answer again, while one with different parameters is a
/// conflict.
#[tokio::test]
async fn a_name_identifies_at_most_one_configuration() {
    let router = router();

    for (case, name) in [
        ("plain name", "NacTaken".to_string()),
        ("underscored name", "Nac_Taken_2".to_string()),
        ("name at the ceiling", "z".repeat(NAME_MAX_LEN)),
    ] {
        let body = json!({ "Name": name, "Description": "first" });
        let (status, first) = create_config(&router, body.clone()).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "case {case:?}: expected 201, got {status} {first}"
        );

        let (status, conflict) =
            create_config(&router, json!({ "Name": name, "Description": "second" })).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "case {case:?}: a second claimant must be 409, got {status} {conflict}"
        );
        assert_eq!(
            error_code(&conflict),
            "ConflictException",
            "case {case:?}: expected ConflictException, got {conflict}"
        );
        let (_, fetched) = get_config(&router, &name).await;
        assert_eq!(
            fetched["Description"],
            json!("first"),
            "case {case:?}: the refused create must not have overwritten the stored \
             configuration, got {:?}",
            fetched["Description"]
        );
    }

    // The same token with the same parameters replays the first call's answer; with
    // different parameters it is a 409 and the stored configuration is untouched.
    for (case, token) in [
        ("token with digits", "token-1"),
        ("token with underscores", "token_two"),
    ] {
        let body = json!({
            "Name": format!("NacToken{token}").replace('-', "_"),
            "Description": "tokened",
            "ClientRequestToken": token,
        });
        let (status, first) = create_config(&router, body.clone()).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "case {case:?}: expected 201, got {status} {first}"
        );
        let (status, replay) = create_config(&router, body.clone()).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "case {case:?}: a replay with the same parameters must succeed, got {status} \
             {replay}"
        );
        assert_eq!(
            replay, first,
            "case {case:?}: a replay must return the first answer {first}, got {replay}"
        );

        let mut changed = body.clone();
        changed["Description"] = json!("changed");
        let (status, conflict) = create_config(&router, changed).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "case {case:?}: a replay with different parameters must be 409, got {status} \
             {conflict}"
        );
        assert_eq!(
            error_code(&conflict),
            "ConflictException",
            "case {case:?}: expected ConflictException, got {conflict}"
        );
        let name = body["Name"].as_str().expect("a Name");
        let (_, fetched) = get_config(&router, name).await;
        assert_eq!(
            fetched["Description"],
            json!("tokened"),
            "case {case:?}: the refused replay must not have changed the stored \
             configuration, got {:?}",
            fetched["Description"]
        );
    }
}

/// A minted ARN carries the region the request's SigV4 credential scope names, and
/// `DEFAULT_REGION` when the request carries no parseable scope.
#[tokio::test]
async fn a_minted_configuration_arn_carries_the_region_the_request_names() {
    let router = router();

    for (index, scope_region) in [Some("us-west-2"), Some("eu-central-1"), Some("ap-south-1"), None]
        .into_iter()
        .enumerate()
    {
        let name = format!("NacRegion{index}");
        let (status, created) = call_signed_for(
            &router,
            scope_region,
            Method::POST,
            "/network-analyzer-configurations",
            Some(json!({ "Name": name })),
        )
        .await;
        let case = scope_region.unwrap_or("<unsigned>");
        assert_eq!(
            status,
            StatusCode::CREATED,
            "case {case:?}: expected 201, got {status} {created}"
        );
        let expected_region = scope_region.unwrap_or(DEFAULT_REGION);
        let expected_arn = format!(
            "arn:aws:iotwireless:{expected_region}:{ACCOUNT}:NetworkAnalyzerConfiguration/{name}"
        );
        assert_eq!(
            created["Arn"],
            json!(expected_arn),
            "case {case:?}: expected Arn {expected_arn}, got {:?}",
            created["Arn"]
        );
        let (_, fetched) = call_signed_for(
            &router,
            scope_region,
            Method::GET,
            &format!("/network-analyzer-configurations/{name}"),
            None,
        )
        .await;
        assert_eq!(
            fetched["Arn"],
            json!(expected_arn),
            "case {case:?}: the reader must report the stored ARN, got {:?}",
            fetched["Arn"]
        );
    }
}

/// A tracked-resource list is bounded: a request at the published ceiling of the member
/// that carries it is accepted as far as resolving its ids, one past that ceiling is
/// refused, and a stored set cannot be grown past it by repeated updates either.
#[tokio::test]
async fn a_tracked_resource_list_cannot_pass_its_published_ceiling() {
    let router = router();
    let absent: Vec<String> = (0..=MAX_TRACKED_WIRELESS_DEVICES)
        .map(|index| format!("00000000-0000-4000-8000-{index:012}"))
        .collect();

    // (case, member, the published ceiling for that member)
    let bounds = [
        ("WirelessDevices", MAX_TRACKED_WIRELESS_DEVICES),
        ("MulticastGroups", MAX_TRACKED_MULTICAST_GROUPS),
    ];

    for (member, max) in bounds {
        // At the ceiling the request's shape is accepted, so the refusal is about the
        // ids it names, not about how many there are.
        let (status, refused) = create_config(
            &router,
            json!({ "Name": format!("NacAtBound{member}"), member: absent[..max] }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "case {member:?} at its ceiling of {max}: expected 404 about the ids, got \
             {status} {refused}"
        );

        // One past the ceiling the request itself is refused.
        let (status, refused) = create_config(
            &router,
            json!({ "Name": format!("NacPastBound{member}"), member: absent[..=max] }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "case {member:?} one past its ceiling of {max}: expected 400, got {status} \
             {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ValidationException",
            "case {member:?} one past its ceiling of {max}: expected ValidationException, \
             got {refused}"
        );
    }

    // The ceiling holds across calls too: a stored set at it cannot be grown by an
    // update that adds one more.
    let groups = seed_groups(&router, MAX_TRACKED_MULTICAST_GROUPS + 1).await;
    let name = "NacGroupCeiling";
    let (status, created) = create_config(
        &router,
        json!({ "Name": name, "MulticastGroups": groups[..MAX_TRACKED_MULTICAST_GROUPS] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "a configuration tracking {MAX_TRACKED_MULTICAST_GROUPS} multicast groups must be \
         accepted, got {status} {created}"
    );
    let (status, refused) = update_config(
        &router,
        name,
        json!({ "MulticastGroupsToAdd": [groups[MAX_TRACKED_MULTICAST_GROUPS].clone()] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "case \"one multicast group past the ceiling\": expected 400, got {status} {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ValidationException",
        "case \"one multicast group past the ceiling\": expected ValidationException, got \
         {refused}"
    );
    let (_, fetched) = get_config(&router, name).await;
    assert_tracked(
        "one multicast group past the ceiling",
        "MulticastGroups",
        &groups[..MAX_TRACKED_MULTICAST_GROUPS],
        &fetched,
    );
}

/// The configuration store is bounded: it accepts configurations up to the ceiling the
/// service declares and refuses the next one, without disturbing what it already holds.
#[tokio::test]
async fn the_configuration_store_is_bounded() {
    let router = router();

    for index in 0..MAX_NETWORK_ANALYZER_CONFIGURATIONS {
        let (status, created) =
            create_config(&router, json!({ "Name": format!("NacBound_{index}") })).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "case {index}: expected 201 below the ceiling of \
             {MAX_NETWORK_ANALYZER_CONFIGURATIONS}, got {status} {created}"
        );
    }

    let (status, refused) = create_config(&router, json!({ "Name": "NacOverTheCeiling" })).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "case \"one past the ceiling\": expected 400, got {status} {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ValidationException",
        "case \"one past the ceiling\": expected ValidationException, got {refused}"
    );
    let (status, fetched) = get_config(&router, "NacBound_0").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "case \"one past the ceiling\": the refused create must leave the store intact, got \
         {status} {fetched}"
    );
    let (status, fetched) = get_config(&router, "NacOverTheCeiling").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "case \"one past the ceiling\": the refused create must persist nothing, got {status} \
         {fetched}"
    );
}
