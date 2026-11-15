// SPDX-License-Identifier: CC0-1.0

//! Patching and deleting a wireless gateway, and the beaconing container its reader
//! reports, end to end over HTTP.
//!
//! Every case drives the real `restJson1` wire path — `MultiServiceRouter::dispatch`
//! with a signed `http::Request` — so what is asserted is what an SDK client sees, not
//! a handler return value.
//!
//! The rules under test, each asserted over a SET of inputs rather than one recorded
//! request:
//!
//! * a LoRaWAN gateway's reported `LoRaWAN` block always carries the `Beaconing`
//!   container — its own members unset while nothing has configured them — so a client
//!   can tell "no beacon parameters" from "this field is not reported";
//! * a gateway's mutable members live on ONE stored record, so `UpdateWirelessGateway`
//!   writes every member the request carried into it — the frame filters and `MaxEirp`
//!   inside its `LoRaWAN` block too — leaves every member it omitted as it was, and the
//!   next `GetWirelessGateway` reports the patched record;
//! * `DeleteWirelessGateway` removes the record, so the delete is observable: the next
//!   read through any modelled identifier kind, patch, delete or task read of that id is
//!   the modelled 404, the `GatewayEui` it held is free for another gateway to claim, and
//!   the gateways that were not deleted keep resolving unchanged;
//! * an id the account does not hold is refused identically by the read, the patch and
//!   the delete, with this service's one fixed sentence for a missing gateway.
//!
//! The wire harness is the one the sibling suites already have: `tests/all/*.rs` are
//! modules of ONE test target, so `call`, `error_code`, `router` and `call_with_headers`
//! are shared from there rather than copied.

use http::{Method, StatusCode};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The name rule the service enforces, so this suite asserts what the implementation
/// declares rather than a list copied by hand.
use service_com_amazonaws_iotwireless_iotwireless::state::FORBIDDEN_NAME_CHARS;

use super::destination_and_wireless_device_lifecycle::{call, error_code, router};
use super::multicast_group_update_and_delete::{call_with_headers, Answer};

/// The identifier kinds `GetWirelessGateway` accepts — the model-generated constant the
/// handler validates against, so the suite and the service cannot drift apart.
const WIRELESS_GATEWAY_ID_TYPES: &[&str] =
    service_com_amazonaws_iotwireless_iotwireless::ops::types::WIRELESS_GATEWAY_ID_TYPE_VALUES;

/// `GatewayMaxEirp`'s published range.
/// Source: `smithy_query(query="shape", shape="GatewayMaxEirp")`.
const MAX_EIRP_MIN: f64 = 0.0;
const MAX_EIRP_MAX: f64 = 30.0;

/// `BeaconingDataRate`'s published range.
/// Source: `smithy_query(query="shape", shape="BeaconingDataRate")`.
const DATA_RATE_MIN: i64 = 0;
const DATA_RATE_MAX: i64 = 15;

/// `BeaconingFrequency`'s published range.
/// Source: `smithy_query(query="shape", shape="BeaconingFrequencies")`.
const FREQUENCY_MIN: i64 = 100_000_000;
const FREQUENCY_MAX: i64 = 1_000_000_000;

/// `WirelessGatewayName`'s published length ceiling.
/// Source: `smithy_query(query="operation", operation="UpdateWirelessGateway")`.
const NAME_MAX_LEN: usize = 256;

/// The gateway members `UpdateWirelessGateway` models a value for, so a patch that
/// carries one must write it and a patch that omits it must leave it alone.
/// Source: `smithy_query(query="operation", operation="UpdateWirelessGateway")`.
const PATCHABLE_MEMBERS: &[&str] = &["Name", "Description"];

/// The same, for the members that live inside the gateway's stored `LoRaWAN` block.
/// Source: the same operation's input, against
/// `smithy_query(query="shape", shape="LoRaWANGateway")`.
const PATCHABLE_LORAWAN_MEMBERS: &[&str] = &["JoinEuiFilters", "NetIdFilters", "MaxEirp"];

/// The `LoRaWANGateway` members no patch models, so every one of them must read back
/// exactly as the create stored it.
/// Source: `smithy_query(query="shape", shape="LoRaWANGateway")` minus the operation's
/// input members above.
const UNPATCHABLE_LORAWAN_MEMBERS: &[&str] = &["GatewayEui", "RfRegion", "SubBands", "Beaconing"];

/// A 204 reports nothing about the resource: whatever the wire protocol writes for an
/// output shape with no members, the answer carries no member.
fn assert_empty_body(case: &str, operation: &str, body: &Value) {
    assert!(
        body.as_object().is_none_or(|members| members.is_empty()),
        "case {case:?}: expected no members in {operation}'s answer, got {body}"
    );
}

async fn create_gateway(router: &MultiServiceRouter, body: Value) -> (StatusCode, Value) {
    call(router, Method::POST, "/wireless-gateways", Some(body)).await
}

/// Read a gateway back by one of the modelled identifier kinds.
async fn get_gateway(
    router: &MultiServiceRouter,
    identifier_type: &str,
    identifier: &str,
) -> (StatusCode, Value) {
    call(
        router,
        Method::GET,
        &format!("/wireless-gateways/{identifier}?identifierType={identifier_type}"),
        None,
    )
    .await
}

/// Read a gateway back by the `Id` the create minted.
async fn get_gateway_by_id(router: &MultiServiceRouter, id: &str) -> (StatusCode, Value) {
    get_gateway(router, "WirelessGatewayId", id).await
}

async fn patch_gateway(
    router: &MultiServiceRouter,
    id: &str,
    body: Value,
) -> (StatusCode, Value) {
    call(
        router,
        Method::PATCH,
        &format!("/wireless-gateways/{id}"),
        Some(body),
    )
    .await
}

async fn delete_gateway(router: &MultiServiceRouter, id: &str) -> (StatusCode, Value) {
    call(
        router,
        Method::DELETE,
        &format!("/wireless-gateways/{id}"),
        None,
    )
    .await
}

/// Create a gateway from `body` and return the `Id` the service minted.
async fn seed_gateway(router: &MultiServiceRouter, body: Value) -> String {
    let (status, created) = create_gateway(router, body).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "seeding a gateway must succeed, got {status} {created}"
    );
    created["Id"]
        .as_str()
        .expect("a created gateway carries an Id")
        .to_string()
}

/// A full `LoRaWAN` member set, so a patch is always exercised against a record that has
/// something to lose.
fn seed_lorawan(gateway_eui: &str) -> Value {
    json!({
        "GatewayEui": gateway_eui,
        "RfRegion": "US915",
        "JoinEuiFilters": [["0000000000000001", "00000000000000ff"]],
        "NetIdFilters": ["000001"],
        "SubBands": [1, 2],
        "Beaconing": { "DataRate": 7, "Frequencies": [500_000_000] },
        "MaxEirp": 12.5,
    })
}

/// A `GatewayEui` in the unseparated form the model's pattern admits, unique per case.
fn gateway_eui(index: usize) -> String {
    format!("{:016x}", 0x0a00_0000_0000_0000_u64 + index as u64)
}

/// The refusal every wireless-gateway-addressing operation gives for an identifier the
/// account does not hold: 404 `ResourceNotFoundException`, the shape named in
/// `x-amzn-errortype`, and this service's one fixed sentence for a missing gateway, which
/// interpolates nothing and carries neither optional member.
fn assert_gateway_not_found(case: &str, operation: &str, answered: &Answer) {
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
    assert_eq!(
        answered.body["Message"].as_str(),
        Some("Wireless Gateway doesn't exist"),
        "case {case:?}: expected the missing-gateway sentence from {operation}, got {}",
        answered.body
    );
    for member in ["ResourceId", "ResourceType"] {
        assert_eq!(
            answered.body.get(member),
            None,
            "case {case:?}: {operation} must leave {member} unset, got {:?}",
            answered.body.get(member)
        );
    }
}

/// A LoRaWAN gateway's reported `LoRaWAN` block always carries the `Beaconing` container,
/// whatever the create supplied for it — nothing at all, an empty container, one member,
/// or both at either end of their published ranges — and the container keeps reporting
/// exactly what was stored across later reads and a patch that names none of it.
#[tokio::test]
async fn a_lorawan_gateway_always_reports_its_beaconing_container() {
    let router = router();

    // (case, the Beaconing value the create carries, the container the reader must
    // report). The first two rows differ only in how "nothing configured" was expressed,
    // which is the distinction the container exists to preserve.
    let cases: Vec<(&str, Option<Value>, Value)> = vec![
        ("no Beaconing member at all", None, json!({})),
        ("an empty Beaconing container", Some(json!({})), json!({})),
        (
            "DataRate at the bottom of its range",
            Some(json!({ "DataRate": DATA_RATE_MIN })),
            json!({ "DataRate": DATA_RATE_MIN }),
        ),
        (
            "DataRate at the top of its range",
            Some(json!({ "DataRate": DATA_RATE_MAX })),
            json!({ "DataRate": DATA_RATE_MAX }),
        ),
        (
            "an empty frequency list",
            Some(json!({ "Frequencies": [] })),
            json!({ "Frequencies": [] }),
        ),
        (
            "frequencies at both ends of the range",
            Some(json!({ "Frequencies": [FREQUENCY_MIN, FREQUENCY_MAX] })),
            json!({ "Frequencies": [FREQUENCY_MIN, FREQUENCY_MAX] }),
        ),
        (
            "both members supplied",
            Some(json!({ "DataRate": 7, "Frequencies": [500_000_000] })),
            json!({ "DataRate": 7, "Frequencies": [500_000_000] }),
        ),
    ];

    for (index, (case, beaconing, expected)) in cases.into_iter().enumerate() {
        let eui = gateway_eui(index);
        let mut lorawan = json!({ "GatewayEui": eui, "RfRegion": "US915" });
        if let Some(beaconing) = beaconing {
            lorawan["Beaconing"] = beaconing;
        }
        let id = seed_gateway(
            &router,
            json!({ "Name": format!("beaconing-{index}"), "LoRaWAN": lorawan }),
        )
        .await;

        let (status, fetched) = get_gateway_by_id(&router, &id).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "case {case:?}: expected 200 from the read, got {status} {fetched}"
        );
        assert_eq!(
            fetched["LoRaWAN"].get("Beaconing"),
            Some(&expected),
            "case {case:?}: expected the Beaconing container {expected}, got {:?}",
            fetched["LoRaWAN"].get("Beaconing")
        );

        // A read reports the container, and reporting it changes nothing: the next read,
        // and a read taken after a patch that names no beaconing member, answer the same.
        let (_, again) = get_gateway_by_id(&router, &id).await;
        assert_eq!(
            again["LoRaWAN"].get("Beaconing"),
            Some(&expected),
            "case {case:?}: a second read must report the same container, got {:?}",
            again["LoRaWAN"].get("Beaconing")
        );
        let (status, answered) =
            patch_gateway(&router, &id, json!({ "Name": format!("renamed-{index}") })).await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "case {case:?}: expected 204 from the patch, got {status} {answered}"
        );
        let (_, after) = get_gateway_by_id(&router, &id).await;
        assert_eq!(
            after["LoRaWAN"].get("Beaconing"),
            Some(&expected),
            "case {case:?}: a patch naming no beaconing member must not change the \
             container, got {:?}",
            after["LoRaWAN"].get("Beaconing")
        );
    }
}

/// Every member `UpdateWirelessGateway` carries is written to the stored record and read
/// back by the next `GetWirelessGateway`; every member it omits is left exactly as it was,
/// inside the `LoRaWAN` block too; and the gateway's identity never moves.
#[tokio::test]
async fn a_patch_writes_the_members_it_carries_and_leaves_the_rest() {
    let router = router();

    // The patch bodies spanning the class: each modelled member alone, several together,
    // every member at once, both ends of the published `MaxEirp` range, a list emptied, a
    // name at its length ceiling, an empty string, and a patch that carries nothing.
    let long_name = "n".repeat(NAME_MAX_LEN);
    let cases: Vec<(&str, Value)> = vec![
        ("nothing at all", json!({})),
        ("Name only", json!({ "Name": "renamed" })),
        ("Name at its length ceiling", json!({ "Name": long_name })),
        ("Name emptied", json!({ "Name": "" })),
        ("Description only", json!({ "Description": "patched" })),
        ("Description emptied", json!({ "Description": "" })),
        (
            "Name and Description",
            json!({ "Name": "renamed-both", "Description": "patched-both" }),
        ),
        (
            "JoinEuiFilters replaced",
            json!({ "JoinEuiFilters": [
                ["0000000000000010", "0000000000000020"],
                ["0000000000000030", "0000000000000040"],
            ] }),
        ),
        ("JoinEuiFilters emptied", json!({ "JoinEuiFilters": [] })),
        (
            "NetIdFilters replaced",
            json!({ "NetIdFilters": ["000002", "ffffff"] }),
        ),
        ("NetIdFilters emptied", json!({ "NetIdFilters": [] })),
        (
            "MaxEirp at the bottom of its range",
            json!({ "MaxEirp": MAX_EIRP_MIN }),
        ),
        (
            "MaxEirp at the top of its range",
            json!({ "MaxEirp": MAX_EIRP_MAX }),
        ),
        (
            "every modelled member at once",
            json!({
                "Name": "renamed-all",
                "Description": "patched-all",
                "JoinEuiFilters": [["00000000000000aa", "00000000000000bb"]],
                "NetIdFilters": ["0000aa"],
                "MaxEirp": 7.5,
            }),
        ),
    ];

    for (index, (case, patch)) in cases.into_iter().enumerate() {
        let id = seed_gateway(
            &router,
            json!({
                "Name": format!("patched-gateway-{index}"),
                "Description": "the seeded description",
                "LoRaWAN": seed_lorawan(&gateway_eui(index)),
            }),
        )
        .await;
        let (_, before) = get_gateway_by_id(&router, &id).await;

        let (status, answered) = patch_gateway(&router, &id, patch.clone()).await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "case {case:?}: expected 204 from the patch, got {status} {answered}"
        );
        assert_empty_body(case, "UpdateWirelessGateway", &answered);

        let (status, after) = get_gateway_by_id(&router, &id).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "case {case:?}: expected 200 from the read, got {status} {after}"
        );
        for member in PATCHABLE_MEMBERS {
            let expected = patch.get(member).or_else(|| before.get(member));
            assert_eq!(
                after.get(member),
                expected,
                "case {case:?}: expected {member} = {expected:?}, got {:?}",
                after.get(member)
            );
        }
        for member in PATCHABLE_LORAWAN_MEMBERS {
            let expected = patch
                .get(member)
                .or_else(|| before["LoRaWAN"].get(member));
            assert_eq!(
                after["LoRaWAN"].get(member),
                expected,
                "case {case:?}: expected LoRaWAN.{member} = {expected:?}, got {:?}",
                after["LoRaWAN"].get(member)
            );
        }
        for member in UNPATCHABLE_LORAWAN_MEMBERS {
            assert_eq!(
                after["LoRaWAN"].get(member),
                before["LoRaWAN"].get(member),
                "case {case:?}: no patch models LoRaWAN.{member}, so it must not change, \
                 got {:?}",
                after["LoRaWAN"].get(member)
            );
        }
        for member in ["Id", "Arn"] {
            assert_eq!(
                after.get(member),
                before.get(member),
                "case {case:?}: a patch must not move the gateway's {member}, got {:?}",
                after.get(member)
            );
        }
    }
}

/// A patch is a write, so the LAST one wins: a sequence of patches leaves the record at
/// the value the final one supplied, and every read in between reports the value then
/// current.
#[tokio::test]
async fn the_last_patch_of_a_member_is_what_the_reader_reports() {
    let router = router();
    let id = seed_gateway(
        &router,
        json!({
            "Name": "sequenced-gateway",
            "LoRaWAN": seed_lorawan(&gateway_eui(0)),
        }),
    )
    .await;

    for (case, name) in [
        ("first rename", "sequenced-gateway-renamed"),
        ("second rename", "sequenced-gateway-renamed-again"),
        ("back to the original", "sequenced-gateway"),
        ("emptied", ""),
    ] {
        let (status, answered) = patch_gateway(&router, &id, json!({ "Name": name })).await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "case {case:?}: expected 204 from the patch, got {status} {answered}"
        );
        let (_, fetched) = get_gateway_by_id(&router, &id).await;
        assert_eq!(
            fetched["Name"].as_str(),
            Some(name),
            "case {case:?}: expected Name {name:?} from the read, got {:?}",
            fetched["Name"]
        );
    }
}

/// `Name`'s documented exclusion list is enforced by the patch as well as the create: a
/// name carrying one of the excluded characters is refused and writes nothing.
#[tokio::test]
async fn a_patched_name_carrying_an_excluded_character_is_refused() {
    let router = router();
    let id = seed_gateway(
        &router,
        json!({ "Name": "excluded-chars", "LoRaWAN": seed_lorawan(&gateway_eui(0)) }),
    )
    .await;

    for excluded in FORBIDDEN_NAME_CHARS {
        let case = format!("name carrying {excluded:?}");
        let name = format!("Gateway{excluded}One");
        let (status, answered) = patch_gateway(&router, &id, json!({ "Name": name })).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "case {case:?}: expected 400 from the patch, got {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {answered}"
        );
        let (_, fetched) = get_gateway_by_id(&router, &id).await;
        assert_eq!(
            fetched["Name"].as_str(),
            Some("excluded-chars"),
            "case {case:?}: a refused patch must write nothing, got {:?}",
            fetched["Name"]
        );
    }
}

/// Deleting a gateway is observable through every operation that addresses it: the read
/// through each modelled identifier kind, the patch, a second delete and the gateway's
/// task reader are all the modelled 404, the `GatewayEui` it held is free again, and the
/// gateway that was not deleted keeps resolving unchanged.
#[tokio::test]
async fn deleting_a_gateway_is_observable_by_the_next_caller() {
    let router = router();

    // (case, the body the gateway is created from, whether to patch it before deleting
    // it) — so the delete is exercised on a gateway with every member set, one with only
    // the required member, and one whose stored record a patch has already rewritten.
    let cases: Vec<(&str, Value, bool)> = vec![
        (
            "every member set",
            json!({
                "Name": "deleted-full",
                "Description": "the whole member set",
                "LoRaWAN": seed_lorawan(&gateway_eui(1)),
            }),
            false,
        ),
        (
            "only the required member",
            json!({ "LoRaWAN": { "GatewayEui": gateway_eui(2) } }),
            false,
        ),
        (
            "patched before the delete",
            json!({
                "Name": "deleted-patched",
                "LoRaWAN": seed_lorawan(&gateway_eui(3)),
            }),
            true,
        ),
    ];

    for (index, (case, body, patch_first)) in cases.into_iter().enumerate() {
        // A second gateway nothing in this case touches, so the delete is shown to remove
        // ONE record rather than emptying the store.
        let keeper_eui = gateway_eui(100 + index);
        let keeper = seed_gateway(
            &router,
            json!({ "Name": "kept", "LoRaWAN": { "GatewayEui": keeper_eui } }),
        )
        .await;

        let eui = body["LoRaWAN"]["GatewayEui"]
            .as_str()
            .expect("the case supplies a GatewayEui")
            .to_string();
        let id = seed_gateway(&router, body.clone()).await;
        if patch_first {
            let (status, answered) =
                patch_gateway(&router, &id, json!({ "Name": "deleted-renamed" })).await;
            assert_eq!(
                status,
                StatusCode::NO_CONTENT,
                "case {case:?}: expected 204 from the patch, got {status} {answered}"
            );
        }
        let (_, keeper_before) = get_gateway_by_id(&router, &keeper).await;

        let (status, answered) = delete_gateway(&router, &id).await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "case {case:?}: expected 204 from the delete, got {status} {answered}"
        );
        assert_empty_body(case, "DeleteWirelessGateway", &answered);

        // Every modelled identifier kind refuses the gateway, whatever it was addressed
        // by: the id and the EUI resolved it a moment ago, and a thing name never did.
        for identifier_type in WIRELESS_GATEWAY_ID_TYPES {
            let identifier = match *identifier_type {
                "GatewayEui" => eui.clone(),
                _ => id.clone(),
            };
            let answered = call_with_headers(
                &router,
                Method::GET,
                &format!("/wireless-gateways/{identifier}?identifierType={identifier_type}"),
                None,
            )
            .await;
            assert_gateway_not_found(
                case,
                &format!("GetWirelessGateway by {identifier_type}"),
                &answered,
            );
        }

        for (operation, method) in [
            ("UpdateWirelessGateway", Method::PATCH),
            ("DeleteWirelessGateway", Method::DELETE),
        ] {
            let payload = (method == Method::PATCH).then(|| json!({ "Name": "after-delete" }));
            let answered = call_with_headers(
                &router,
                method,
                &format!("/wireless-gateways/{id}"),
                payload,
            )
            .await;
            assert_gateway_not_found(case, operation, &answered);
        }

        // The gateway's task reader addresses the gateway too, so it stops resolving with
        // the record.
        let answered = call_with_headers(
            &router,
            Method::GET,
            &format!("/wireless-gateways/{id}/tasks"),
            None,
        )
        .await;
        assert_gateway_not_found(case, "GetWirelessGatewayTask", &answered);

        // The deleted gateway's `GatewayEui` identified it alone, so it is free for
        // another gateway to claim — which is a DIFFERENT gateway, under a fresh id.
        let reclaimed = seed_gateway(
            &router,
            json!({ "Name": "reclaimed", "LoRaWAN": { "GatewayEui": eui } }),
        )
        .await;
        assert_ne!(
            reclaimed, id,
            "case {case:?}: re-claiming the EUI must mint a new gateway, got {reclaimed}"
        );

        // Nothing else moved.
        let (status, keeper_after) = get_gateway_by_id(&router, &keeper).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "case {case:?}: the gateway that was not deleted must still read, got \
             {status} {keeper_after}"
        );
        assert_eq!(
            keeper_after, keeper_before,
            "case {case:?}: the gateway that was not deleted must be unchanged, got \
             {keeper_after}"
        );
    }
}

/// An identifier the account does not hold is refused identically by the read, the patch
/// and the delete — whatever the identifier looks like — and the gateway the store does
/// hold keeps resolving.
#[tokio::test]
async fn an_identifier_nothing_holds_is_refused_by_every_operation() {
    let router = router();
    let id = seed_gateway(
        &router,
        json!({ "Name": "survivor", "LoRaWAN": seed_lorawan(&gateway_eui(0)) }),
    )
    .await;

    // Identifier shapes spanning what a client can send: a uuid nothing minted, a value
    // that is not a uuid at all, and one at the published length ceiling.
    for unknown in [
        "11111111-2222-3333-4444-555555555555".to_string(),
        "not-a-uuid".to_string(),
        "z".repeat(NAME_MAX_LEN),
    ] {
        let case = format!("identifier {unknown:?}");
        let answered = call_with_headers(
            &router,
            Method::GET,
            &format!("/wireless-gateways/{unknown}?identifierType=WirelessGatewayId"),
            None,
        )
        .await;
        assert_gateway_not_found(&case, "GetWirelessGateway", &answered);

        let answered = call_with_headers(
            &router,
            Method::PATCH,
            &format!("/wireless-gateways/{unknown}"),
            Some(json!({ "Name": "nothing-to-patch" })),
        )
        .await;
        assert_gateway_not_found(&case, "UpdateWirelessGateway", &answered);

        let answered = call_with_headers(
            &router,
            Method::DELETE,
            &format!("/wireless-gateways/{unknown}"),
            None,
        )
        .await;
        assert_gateway_not_found(&case, "DeleteWirelessGateway", &answered);

        let (status, fetched) = get_gateway_by_id(&router, &id).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "case {case:?}: the stored gateway must keep resolving, got {status} {fetched}"
        );
        assert_eq!(
            fetched["Name"].as_str(),
            Some("survivor"),
            "case {case:?}: the stored gateway must be unchanged, got {fetched}"
        );
    }
}
