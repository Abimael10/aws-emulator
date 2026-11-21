// SPDX-License-Identifier: CC0-1.0

//! The Direct Connect gateway lifecycle, end to end over the wire.
//!
//! Every assertion goes through `POST /` with `X-Amz-Target: OvertureService.<Op>`
//! (awsJson1_1), the same path a client takes, and every expectation is about what
//! the SERVICE STORED: a `CreateDirectConnectGateway` is proved by reading the
//! gateway back through `DescribeDirectConnectGateways`.
//!
//! The tables span the class each rule generalises over (ASN range and bound, tag
//! count, name shape, page size, filter), and the ASN tables are BUILT from the
//! service's own `PRIVATE_AMAZON_SIDE_ASN_RANGES` — the ranges
//! `CreateDirectConnectGatewayRequest$amazonSideAsn` documents — so every published
//! bound and one value past it is covered rather than a chosen few.
//!
//! Two of the rules pinned here are about what a REFUSAL is: a `directConnectGatewayId`
//! that selects no gateway is an empty page and not a client fault, and an
//! `amazonSideAsn` outside the documented private ranges is refused with the one
//! sentence this service states for it, the rejected value and nothing else of the
//! request in it.

use std::time::Duration;

use serde_json::{json, Map, Value};
use service_com_amazonaws_directconnect_overtureservice::state::gateway_associations::MAX_ALLOWED_PREFIXES_PER_ASSOCIATION;
use service_com_amazonaws_directconnect_overtureservice::state::{
    ComAmazonawsDirectconnectOvertureserviceState, DEFAULT_AMAZON_SIDE_ASN, MAX_RESULT_SET_SIZE,
    PRIVATE_AMAZON_SIDE_ASN_RANGES,
};

use crate::connection_and_virtual_interface_lifecycle::{error_code, Emulator};

/// Long enough for the `pending` span the service owes the clock to fall due —
/// its own `TRANSITION_DELAY` plus slack.
///
/// Awaited with `std::thread::sleep` rather than `tokio::time::sleep`: the
/// workspace enables tokio's `macros`/`rt` features only, so `tokio::time` is not
/// available here. Nothing else is pending on the test's runtime.
const SETTLE: Duration = Duration::from_millis(900);

/// The group lengths of a UUID (`8-4-4-4-12`), the form AWS reports a
/// `directConnectGatewayId` in.
const UUID_GROUP_LENGTHS: [usize; 5] = [8, 4, 4, 4, 12];

/// One row of the create table: case, name, `amazonSideAsn`, tags.
type GatewayRow<'a> = (&'a str, &'a str, Option<i64>, &'a [(&'a str, &'a str)]);

/// Whether `id` is a server-minted gateway id: lowercase hex in UUID groups.
fn is_minted_gateway_id(id: &Value) -> bool {
    let Some(id) = id.as_str() else { return false };
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == UUID_GROUP_LENGTHS.len()
        && groups
            .iter()
            .zip(UUID_GROUP_LENGTHS)
            .all(|(group, length)| {
                group.len() == length
                    && group
                        .chars()
                        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
            })
}

/// The message an awsJson1_1 error body carries in `message`.
fn error_message(body: &Value) -> String {
    body["message"].as_str().unwrap_or_default().to_string()
}

/// A `CreateDirectConnectGateway` body.
fn create_gateway_body(
    name: &str,
    amazon_side_asn: Option<i64>,
    tags: &[(&str, &str)],
) -> Value {
    let mut body = Map::new();
    body.insert("directConnectGatewayName".to_string(), json!(name));
    if let Some(asn) = amazon_side_asn {
        body.insert("amazonSideAsn".to_string(), json!(asn));
    }
    if !tags.is_empty() {
        body.insert(
            "tags".to_string(),
            Value::Array(
                tags.iter()
                    .map(|(key, value)| json!({"key": key, "value": value}))
                    .collect(),
            ),
        );
    }
    Value::Object(body)
}

/// Create one gateway and return its id, failing the test if it was refused.
async fn gateway(dx: &Emulator, name: &str) -> String {
    let (status, body) = dx
        .call(
            "CreateDirectConnectGateway",
            create_gateway_body(name, None, &[]),
        )
        .await;
    assert_eq!(
        status, 200,
        "setup {name:?}: expected CreateDirectConnectGateway to succeed, got {status} with {body}"
    );
    body["directConnectGateway"]["directConnectGatewayId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The gateways `DescribeDirectConnectGateways` answers for `body`, with the
/// response's `nextToken`.
async fn describe(dx: &Emulator, body: Value) -> (Vec<Value>, Value) {
    let (status, response) = dx.call("DescribeDirectConnectGateways", body).await;
    assert_eq!(
        status, 200,
        "expected DescribeDirectConnectGateways 200, got {status} with {response}"
    );
    (
        response["directConnectGateways"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
        response["nextToken"].clone(),
    )
}

/// `gateway` without its state, so a round-trip comparison does not race the
/// `pending` -> `available` transition the clock owes.
fn without_state(gateway: &Value) -> Value {
    let mut copy = gateway.clone();
    if let Some(object) = copy.as_object_mut() {
        object.remove("directConnectGatewayState");
    }
    copy
}

#[tokio::test]
async fn a_created_gateway_is_minted_stored_and_read_back_field_for_field() {
    let dx = Emulator::new();

    // (case, name, amazonSideAsn, tags). The ASNs span both private ranges the
    // service's PRIVATE_AMAZON_SIDE_ASN_RANGES names, including each bound.
    let rows: &[GatewayRow<'_>] = &[
        ("no amazonSideAsn, so the documented default", "dxgw-default", None, &[]),
        (
            "the low bound of the two-byte private range",
            "Gateway With Spaces",
            Some(PRIVATE_AMAZON_SIDE_ASN_RANGES[0].0),
            &[("env", "prod")],
        ),
        (
            "the high bound of the two-byte private range",
            "gw.with.dots",
            Some(PRIVATE_AMAZON_SIDE_ASN_RANGES[0].1),
            &[("env", "test"), ("team", "network")],
        ),
        (
            "the low bound of the four-byte private range",
            "x",
            Some(PRIVATE_AMAZON_SIDE_ASN_RANGES[1].0),
            &[],
        ),
        (
            "the high bound of the four-byte private range",
            "UPPER-and-lower_99",
            Some(PRIVATE_AMAZON_SIDE_ASN_RANGES[1].1),
            &[("only", "one-tag")],
        ),
        (
            "a value inside the four-byte private range",
            "mid-range-asn",
            Some(4_250_000_000),
            &[],
        ),
    ];

    let mut minted: Vec<String> = Vec::new();
    for (case, name, amazon_side_asn, tags) in rows {
        let (status, response) = dx
            .call(
                "CreateDirectConnectGateway",
                create_gateway_body(name, *amazon_side_asn, tags),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {response}"
        );
        let created = response["directConnectGateway"].clone();
        assert!(
            is_minted_gateway_id(&created["directConnectGatewayId"]),
            "case {case:?}: expected a server-minted UUID id, got {:?}",
            created["directConnectGatewayId"]
        );
        assert_eq!(
            created["directConnectGatewayName"],
            json!(name),
            "case {case:?}: expected the requested name, got {:?}",
            created["directConnectGatewayName"]
        );
        assert_eq!(
            created["directConnectGatewayState"],
            json!("pending"),
            "case {case:?}: pending is the initial state after the create, got {:?}",
            created["directConnectGatewayState"]
        );
        assert_eq!(
            created["amazonSideAsn"],
            json!(amazon_side_asn.unwrap_or(DEFAULT_AMAZON_SIDE_ASN)),
            "case {case:?}: expected the requested ASN or the documented default, got {:?}",
            created["amazonSideAsn"]
        );
        assert_eq!(
            created["ownerAccount"],
            json!(engine::EMULATOR_ACCOUNT_ID),
            "case {case:?}: expected the emulator account, got {:?}",
            created["ownerAccount"]
        );
        // A brand new gateway has no attachments, so no route prefixes are
        // allocated to it.
        assert_eq!(
            created["totalPrefixPoolAllocations"],
            json!(0),
            "case {case:?}: a gateway with no attachments allocates no prefixes, got {:?}",
            created["totalPrefixPoolAllocations"]
        );
        assert!(
            created["stateChangeError"].is_null(),
            "case {case:?}: no state advance has failed, got {:?}",
            created["stateChangeError"]
        );
        assert_eq!(
            created["tags"].as_array().map_or(0, Vec::len),
            tags.len(),
            "case {case:?}: expected {} tags, got {:?}",
            tags.len(),
            created["tags"]
        );

        // The stored record, read back: field for field what the create returned.
        let id = created["directConnectGatewayId"].as_str().unwrap_or_default();
        let (described, next_token) = describe(&dx, json!({"directConnectGatewayId": id})).await;
        assert_eq!(
            described.len(),
            1,
            "case {case:?}: expected exactly the named gateway, got {described:?}"
        );
        assert!(
            next_token.is_null(),
            "case {case:?}: a single-item page is the last one, got {next_token:?}"
        );
        assert_eq!(
            without_state(&described[0]),
            without_state(&created),
            "case {case:?}: the describe must render the stored record, got {:?}",
            described[0]
        );

        minted.push(id.to_string());
    }

    // Ids are unique, and a describe with no filter answers every stored one in
    // creation order.
    let (all, _) = describe(&dx, json!({})).await;
    let listed: Vec<String> = all
        .iter()
        .map(|gateway| {
            gateway["directConnectGatewayId"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        listed, minted,
        "expected every stored gateway in creation order, got {listed:?}"
    );
}

#[tokio::test]
async fn a_pending_gateway_becomes_available_when_the_clock_makes_it_due() {
    let dx = Emulator::new();
    let id = gateway(&dx, "clock-owed").await;

    // The span is never zero, so a caller reading straight back still observes
    // the transient state the model documents.
    let (described, _) = describe(&dx, json!({"directConnectGatewayId": &id})).await;
    assert_eq!(
        described[0]["directConnectGatewayState"],
        json!("pending"),
        "case {id:?}: a gateway read straight back is still pending, got {:?}",
        described[0]["directConnectGatewayState"]
    );

    std::thread::sleep(SETTLE);

    let (described, _) = describe(&dx, json!({"directConnectGatewayId": &id})).await;
    assert_eq!(
        described[0]["directConnectGatewayState"],
        json!("available"),
        "case {id:?}: the pending span has elapsed, so the gateway is available, got {:?}",
        described[0]["directConnectGatewayState"]
    );
    assert_eq!(
        described[0]["directConnectGatewayId"],
        json!(&id),
        "case {id:?}: the id must not change when the state advances, got {:?}",
        described[0]["directConnectGatewayId"]
    );
    // Reading it again answers the same state: the read materialised only what
    // the clock already owed.
    let (again, _) = describe(&dx, json!({"directConnectGatewayId": &id})).await;
    assert_eq!(
        again[0], described[0],
        "case {id:?}: a second read must answer the same record, got {:?}",
        again[0]
    );
}

#[tokio::test]
async fn an_amazon_side_asn_outside_the_documented_private_ranges_is_refused() {
    let dx = Emulator::new();

    // One value past each published bound of each documented private range, plus
    // values far outside them.
    let mut rows: Vec<(String, Value)> = Vec::new();
    for (low, high) in PRIVATE_AMAZON_SIDE_ASN_RANGES {
        rows.push((format!("one below the low bound {low}"), json!(low - 1)));
        rows.push((format!("one above the high bound {high}"), json!(high + 1)));
    }
    rows.push(("a public two-byte ASN".to_string(), json!(7_224)));
    rows.push(("zero".to_string(), json!(0)));
    rows.push(("negative".to_string(), json!(-1)));

    for (case, asn) in &rows {
        let mut body = Map::new();
        body.insert("directConnectGatewayName".to_string(), json!("asn-check"));
        body.insert("amazonSideAsn".to_string(), asn.clone());
        let (status, response) = dx
            .call("CreateDirectConnectGateway", Value::Object(body))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a refusal, got {status} with {response}"
        );
        assert!(
            error_code(&response).contains("DirectConnectClientException"),
            "case {case:?}: expected DirectConnectClientException, got {:?}",
            error_code(&response)
        );
        // Every client fault of this service shares one error shape and one 400
        // status, so the SENTENCE is the whole discriminator: fixed text, with the
        // rejected value — and nothing else of the request — in front of it. The
        // member is not named and the ranges are not restated.
        let expected = format!("{asn} is not a valid value for Amazon Side ASN");
        assert_eq!(
            error_message(&response),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            error_message(&response)
        );
    }

    // A refused create stores nothing.
    let (stored, _) = describe(&dx, json!({})).await;
    assert!(
        stored.is_empty(),
        "a refused create must store no gateway, got {stored:?}"
    );
}

#[tokio::test]
async fn a_request_that_names_nothing_is_refused() {
    let dx = Emulator::new();

    // (case, body, the error code expected)
    let rows: &[(&str, Value, &str)] = &[
        (
            "an empty gateway name",
            json!({"directConnectGatewayName": ""}),
            "DirectConnectClientException",
        ),
        (
            "a gateway name of only whitespace",
            json!({"directConnectGatewayName": "   "}),
            "DirectConnectClientException",
        ),
        (
            "a pagination token naming nothing",
            json!({"nextToken": "00000000-0000-4000-8000-000000000000"}),
            "DirectConnectClientException",
        ),
    ];

    for (case, body, expected) in rows {
        let target = if body.get("directConnectGatewayName").is_some() {
            "CreateDirectConnectGateway"
        } else {
            "DescribeDirectConnectGateways"
        };
        let (status, response) = dx.call(target, body.clone()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a refusal, got {status} with {response}"
        );
        assert!(
            error_code(&response).contains(expected),
            "case {case:?}: expected {expected}, got {:?}",
            error_code(&response)
        );
    }
}

#[tokio::test]
async fn a_gateway_id_filter_that_selects_nothing_answers_an_empty_page() {
    let dx = Emulator::new();

    // Two gateways stored, so a filter that selects nothing is answering about a
    // store that HOLDS something.
    let stored = gateway(&dx, "selection-one").await;
    let other = gateway(&dx, "selection-two").await;

    // And one the store holds in the `deleted` state: "Deleted Direct Connect
    // gateways are not returned", which is that same empty selection rather than a
    // bad parameter. No operation deletes a gateway yet, so the record is put in
    // that state directly through the shared engine — the store the handler reads.
    let deleted = gateway(&dx, "selection-deleted").await;
    dx.engine()
        .state::<ComAmazonawsDirectconnectOvertureserviceState>()
        .lock()
        .gateways
        .get_mut(&deleted)
        .expect("the gateway just created is in the store")
        .direct_connect_gateway_state = "deleted".to_string();

    // (case, the directConnectGatewayId filtered by, the ids the page must hold).
    // `DirectConnectGatewayId` declares no constraint
    // (`smithy_query(query="shape",shape="DirectConnectGatewayId")`), so every row
    // below is a well-formed request; they differ along the FORM of the value — a
    // UUID naming no gateway, a value in another resource's id form, a value
    // carrying a space, the empty string — and along what the store holds for it: a
    // gateway that is deleted, and a gateway that is there.
    let rows: Vec<(&str, String, Vec<String>)> = vec![
        (
            "a UUID naming no gateway",
            "00000000-0000-4000-8000-000000000000".to_string(),
            vec![],
        ),
        (
            "a value in no gateway-id form at all",
            "dxgw-nothing-here".to_string(),
            vec![],
        ),
        ("a value carrying a space", "not a gateway".to_string(), vec![]),
        ("the empty string", String::new(), vec![]),
        ("the id of a deleted gateway", deleted.clone(), vec![]),
        (
            "the id of a stored gateway",
            stored.clone(),
            vec![stored.clone()],
        ),
    ];

    for (case, filter, expected) in &rows {
        for body in [
            json!({"directConnectGatewayId": filter}),
            // The same selection asked for one page at a time: a window over
            // nothing is still nothing, and never a refusal.
            json!({"directConnectGatewayId": filter, "maxResults": 1}),
        ] {
            let (status, response) = dx.call("DescribeDirectConnectGateways", body.clone()).await;
            assert_eq!(
                status, 200,
                "case {case:?} as {body}: expected 200, got {status} with {response}"
            );
            // The member is SERIALIZED even when it holds nothing: a client reads
            // an empty list as "absent", and an omitted member as "no answer".
            let Some(listed) = response["directConnectGateways"].as_array() else {
                panic!(
                    "case {case:?} as {body}: directConnectGateways must be present, got {response}"
                );
            };
            let ids: Vec<String> = listed
                .iter()
                .map(|gateway| {
                    gateway["directConnectGatewayId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect();
            assert_eq!(
                &ids, expected,
                "case {case:?} as {body}: expected {expected:?}, got {ids:?}"
            );
            assert!(
                response["nextToken"].is_null(),
                "case {case:?} as {body}: one page holds the whole selection, so no token, got {:?}",
                response["nextToken"]
            );
        }
    }

    // The filter NARROWS the listing rather than replacing it: unfiltered, every
    // gateway that is not deleted is still answered.
    let (all, _) = describe(&dx, json!({})).await;
    let listed: Vec<String> = all
        .iter()
        .map(|gateway| {
            gateway["directConnectGatewayId"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        listed,
        vec![stored, other],
        "expected every gateway that is not deleted, got {listed:?}"
    );
}

#[tokio::test]
async fn gateways_page_with_max_results_and_next_token() {
    let dx = Emulator::new();
    let mut ids = Vec::new();
    for n in 0..5 {
        ids.push(gateway(&dx, &format!("page-{n}")).await);
    }

    // (case, page size, expected page lengths)
    let rows: &[(&str, i64, &[usize])] = &[
        ("one at a time", 1, &[1, 1, 1, 1, 1]),
        ("two at a time", 2, &[2, 2, 1]),
        ("exactly the whole set", 5, &[5]),
        ("more than the whole set", 50, &[5]),
    ];

    for (case, page_size, expected_lengths) in rows {
        let mut token: Option<String> = None;
        let mut seen: Vec<String> = Vec::new();
        for (index, expected_length) in expected_lengths.iter().enumerate() {
            let mut body = Map::new();
            body.insert("maxResults".to_string(), json!(page_size));
            if let Some(token) = &token {
                body.insert("nextToken".to_string(), json!(token));
            }
            let (page, next_token) = describe(&dx, Value::Object(body)).await;
            assert_eq!(
                page.len(),
                *expected_length,
                "case {case:?} page {index}: expected {expected_length} items, got {}",
                page.len()
            );
            seen.extend(page.iter().map(|gateway| {
                gateway["directConnectGatewayId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            }));
            let is_last = index + 1 == expected_lengths.len();
            assert_eq!(
                next_token.is_null(),
                is_last,
                "case {case:?} page {index}: nextToken must be null only on the last page, got {next_token:?}"
            );
            token = next_token.as_str().map(ToString::to_string);
        }
        assert_eq!(
            seen, ids,
            "case {case:?}: paging must visit every stored gateway exactly once, got {seen:?}"
        );
    }
}

#[tokio::test]
async fn a_max_results_above_the_documented_ceiling_is_capped() {
    let dx = Emulator::new();
    // One more than the documented ceiling, so a capped page is observable.
    let total = usize::try_from(MAX_RESULT_SET_SIZE).expect("the ceiling is positive") + 1;
    for n in 0..total {
        gateway(&dx, &format!("capped-{n}")).await;
    }

    // "If MaxResults is given a value larger than 100, only 100 results are
    // returned."
    for asked in [MAX_RESULT_SET_SIZE + 1, 250, 100_000] {
        let (page, next_token) = describe(&dx, json!({"maxResults": asked})).await;
        assert_eq!(
            page.len(),
            usize::try_from(MAX_RESULT_SET_SIZE).expect("the ceiling is positive"),
            "case {asked:?}: a page is capped at {MAX_RESULT_SET_SIZE}, got {}",
            page.len()
        );
        assert!(
            next_token.is_string(),
            "case {asked:?}: a capped page is not the last one, got {next_token:?}"
        );

        let (rest, rest_token) =
            describe(&dx, json!({"maxResults": asked, "nextToken": next_token})).await;
        assert_eq!(
            rest.len(),
            total - usize::try_from(MAX_RESULT_SET_SIZE).expect("the ceiling is positive"),
            "case {asked:?}: the next page holds what the cap left, got {}",
            rest.len()
        );
        assert!(
            rest_token.is_null(),
            "case {asked:?}: the last page carries no token, got {rest_token:?}"
        );
    }
}

#[tokio::test]
async fn a_private_virtual_interface_terminates_on_a_stored_gateway() {
    let dx = Emulator::new();
    let (status, connection) = dx
        .call(
            "CreateConnection",
            json!({"connectionName": "gw-host", "location": "EqDC2", "bandwidth": "1Gbps"}),
        )
        .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateConnection to succeed, got {status} with {connection}"
    );
    let connection_id = connection["connectionId"].as_str().unwrap_or_default();

    let asn_rows = [
        PRIVATE_AMAZON_SIDE_ASN_RANGES[0].0,
        PRIVATE_AMAZON_SIDE_ASN_RANGES[1].1,
    ];
    for (index, amazon_side_asn) in asn_rows.into_iter().enumerate() {
        let (status, created) = dx
            .call(
                "CreateDirectConnectGateway",
                create_gateway_body(&format!("vif-gateway-{index}"), Some(amazon_side_asn), &[]),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {amazon_side_asn:?}: expected the gateway create to succeed, got {status} with {created}"
        );
        let gateway_id = created["directConnectGateway"]["directConnectGatewayId"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let (status, vif) = dx
            .call(
                "CreatePrivateVirtualInterface",
                json!({
                    "connectionId": connection_id,
                    "newPrivateVirtualInterface": {
                        "virtualInterfaceName": format!("vif-{index}"),
                        "vlan": 200 + i64::try_from(index).unwrap_or(0),
                        "asn": 65_000,
                        "directConnectGatewayId": &gateway_id,
                    },
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {amazon_side_asn:?}: expected the virtual interface create to succeed, got {status} with {vif}"
        );
        assert_eq!(
            vif["directConnectGatewayId"],
            json!(&gateway_id),
            "case {amazon_side_asn:?}: expected the gateway the caller named, got {:?}",
            vif["directConnectGatewayId"]
        );
        // The Amazon side of a peering that terminates on a Direct Connect
        // gateway is that gateway's ASN.
        assert_eq!(
            vif["amazonSideAsn"],
            json!(amazon_side_asn),
            "case {amazon_side_asn:?}: expected the gateway's ASN, got {:?}",
            vif["amazonSideAsn"]
        );
        assert!(
            vif["virtualGatewayId"].is_null(),
            "case {amazon_side_asn:?}: a gateway-terminated interface names no virtual private gateway, got {:?}",
            vif["virtualGatewayId"]
        );
    }

    // (case, the newPrivateVirtualInterface members under test)
    let refusals: &[(&str, Value)] = &[
        (
            "a Direct Connect gateway id naming nothing",
            json!({
                "virtualInterfaceName": "unknown-gw",
                "vlan": 301,
                "asn": 65_000,
                "directConnectGatewayId": "00000000-0000-4000-8000-000000000000",
            }),
        ),
        (
            "both gateway kinds at once",
            json!({
                "virtualInterfaceName": "both-gw",
                "vlan": 302,
                "asn": 65_000,
                "directConnectGatewayId": "00000000-0000-4000-8000-000000000000",
                "virtualGatewayId": "vgw-0123456789abcdef0",
            }),
        ),
        (
            "neither gateway kind",
            json!({
                "virtualInterfaceName": "no-gw",
                "vlan": 303,
                "asn": 65_000,
            }),
        ),
    ];

    for (case, requested) in refusals {
        let (status, response) = dx
            .call(
                "CreatePrivateVirtualInterface",
                json!({
                    "connectionId": connection_id,
                    "newPrivateVirtualInterface": requested.clone(),
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a refusal, got {status} with {response}"
        );
        assert!(
            error_code(&response).contains("DirectConnectClientException"),
            "case {case:?}: expected DirectConnectClientException, got {:?}",
            error_code(&response)
        );
    }
}

/// The associations `DescribeDirectConnectGatewayAssociations` answers for `body`.
async fn associations(dx: &Emulator, case: &str, body: Value) -> Vec<Value> {
    let (status, response) = dx
        .call("DescribeDirectConnectGatewayAssociations", body)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the describe to be served, got {status} with {response}"
    );
    response["directConnectGatewayAssociations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Rule: the gateway a Direct Connect gateway association is made on is a resource of
// the service that OWNS virtual private gateways and transit gateways, resolved there
// before anything is stored. So Direct Connect alone refuses a request that names no
// Direct Connect gateway, names no gateway to associate, names an identifier that is
// no gateway identifier at all, or names more prefixes than this emulator advertises
// per association — and with no owner of gateways on this engine to ask, every
// gateway identifier resolves to nothing, which is fail-closed: nothing is stored and
// no association is reported.
//
// The ACCEPTED side needs that owner beside this service on one engine, so it is
// driven end to end in
// `core/router/tests/all/direct_connect_gateway_association_network.rs`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_association_naming_no_gateway_the_account_holds_is_refused_and_stores_nothing() {
    let dx = Emulator::new();
    let gateway_id = gateway(&dx, "association-refusals").await;
    // One prefix past the ceiling this emulator advertises per association.
    let too_many: Vec<Value> = (0..=MAX_ALLOWED_PREFIXES_PER_ASSOCIATION)
        .map(|n| json!({"cidr": format!("10.{}.{}.0/24", n / 256, n % 256)}))
        .collect();

    // (case, the request body, the text the refusal must name)
    let rows: Vec<(String, Value, String)> = vec![
        (
            "a Direct Connect gateway id naming nothing, beside a gateway no owner \
             answers for: the gateway to associate is resolved first"
                .to_string(),
            json!({
                "directConnectGatewayId": "00000000-0000-4000-8000-000000000000",
                "gatewayId": "vgw-0123456789abcdef0",
            }),
            "vgw-0123456789abcdef0".to_string(),
        ),
        (
            "no gateway to associate at all".to_string(),
            json!({"directConnectGatewayId": &gateway_id}),
            "virtualGatewayId".to_string(),
        ),
        (
            "gatewayId and virtualGatewayId naming different gateways".to_string(),
            json!({
                "directConnectGatewayId": &gateway_id,
                "gatewayId": "vgw-0123456789abcdef0",
                "virtualGatewayId": "vgw-0f1e2d3c4b5a69780",
            }),
            "virtualGatewayId".to_string(),
        ),
        (
            "an identifier that is no gateway identifier".to_string(),
            json!({"directConnectGatewayId": &gateway_id, "gatewayId": "potato"}),
            "potato".to_string(),
        ),
        (
            "a virtual private gateway no owner answers for".to_string(),
            json!({"directConnectGatewayId": &gateway_id, "gatewayId": "vgw-0123456789abcdef0"}),
            "vgw-0123456789abcdef0".to_string(),
        ),
        (
            "a transit gateway no owner answers for".to_string(),
            json!({
                "directConnectGatewayId": &gateway_id,
                "gatewayId": "tgw-0f1e2d3c4b5a69780",
                "addAllowedPrefixesToDirectConnectGateway": [{"cidr": "10.0.0.0/16"}],
            }),
            "tgw-0f1e2d3c4b5a69780".to_string(),
        ),
        (
            "one prefix past the ceiling".to_string(),
            json!({
                "directConnectGatewayId": &gateway_id,
                "gatewayId": "tgw-0f1e2d3c4b5a69780",
                "addAllowedPrefixesToDirectConnectGateway": too_many,
            }),
            MAX_ALLOWED_PREFIXES_PER_ASSOCIATION.to_string(),
        ),
        (
            "prefixes named for a virtual private gateway, whose prefixes are its \
             attached VPCs' blocks"
                .to_string(),
            json!({
                "directConnectGatewayId": &gateway_id,
                "gatewayId": "vgw-0123456789abcdef0",
                "addAllowedPrefixesToDirectConnectGateway": [{"cidr": "10.0.0.0/16"}],
            }),
            "addAllowedPrefixesToDirectConnectGateway".to_string(),
        ),
    ];

    for (case, body, named) in &rows {
        let (status, response) = dx
            .call("CreateDirectConnectGatewayAssociation", body.clone())
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a refusal, got {status} with {response}"
        );
        assert!(
            error_code(&response).contains("DirectConnectClientException"),
            "case {case:?}: expected DirectConnectClientException, got {:?}",
            error_code(&response)
        );
        let message = response["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(named.as_str()),
            "case {case:?}: expected the refusal to name {named:?}, got {response}"
        );
        // A refused create stores nothing, so no association is reported for either
        // gateway the rows name.
        for selector in [
            json!({"directConnectGatewayId": &gateway_id}),
            json!({"directConnectGatewayId": "00000000-0000-4000-8000-000000000000"}),
        ] {
            let stored = associations(&dx, case, selector).await;
            assert!(
                stored.is_empty(),
                "case {case:?}: a refused create must store no association, got {stored:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_delete_that_names_no_stored_association_is_refused() {
    let dx = Emulator::new();
    let gateway_id = gateway(&dx, "delete-refusals").await;

    // (case, the request body)
    let rows: Vec<(&str, Value)> = vec![
        (
            "an associationId naming nothing",
            json!({"associationId": "00000000-0000-4000-8000-000000000000"}),
            ),
        (
            "a Direct Connect gateway and a gateway with no association between them",
            json!({
                "directConnectGatewayId": &gateway_id,
                "virtualGatewayId": "vgw-0123456789abcdef0",
            }),
        ),
        (
            "neither an associationId nor both gateway ids",
            json!({"directConnectGatewayId": &gateway_id}),
        ),
        ("nothing at all", json!({})),
    ];

    for (case, body) in &rows {
        let (status, response) = dx
            .call("DeleteDirectConnectGatewayAssociation", body.clone())
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a refusal, got {status} with {response}"
        );
        assert!(
            error_code(&response).contains("DirectConnectClientException"),
            "case {case:?}: expected DirectConnectClientException, got {:?}",
            error_code(&response)
        );
    }

    // What this listing does with a selector, and with none, is the next test's
    // subject: `a_listing_of_associations_that_selects_by_nothing_is_refused_whatever_else_it_carries`.
}

// ---------------------------------------------------------------------------
// Rule: an operation published only in forms that each name a selector refuses a
// request carrying none of them, with the sentence it states for that refusal — a
// windowing member is not a selector, so it never makes such a request answerable,
// and a selector that IS carried only filters, whatever its value names.
// ---------------------------------------------------------------------------

/// The sentence `DescribeDirectConnectGatewayAssociations` states for a request
/// that selects by nothing. FIXED text: no member, value or identifier of the
/// request appears in it.
const SELECTOR_REQUIRED: &str =
    "Either Direct Connect Gateway ID or Associated Gateway ID or Association ID must be set";

#[tokio::test]
async fn a_listing_of_associations_that_selects_by_nothing_is_refused_whatever_else_it_carries() {
    let dx = Emulator::new();
    let gateway_id = gateway(&dx, "selector-required").await;

    // (case, the request body). Every row carries NO selector and differs in which
    // windowing members it carries and what they hold — including each bound of
    // `maxResults` ("If MaxResults is given a value larger than 100, only 100
    // results are returned") and one value past it.
    let refused: Vec<(&str, Value)> = vec![
        ("no member at all", json!({})),
        ("the smallest page", json!({"maxResults": 1})),
        (
            "the largest page the member documents",
            json!({"maxResults": MAX_RESULT_SET_SIZE}),
        ),
        (
            "one past the largest page the member documents",
            json!({"maxResults": MAX_RESULT_SET_SIZE + 1}),
        ),
        ("a page size of zero", json!({"maxResults": 0})),
        (
            "a pagination token",
            json!({"nextToken": "00000000-0000-4000-8000-000000000000"}),
        ),
        (
            "a page size and a pagination token",
            json!({"maxResults": 7, "nextToken": "not-a-token"}),
        ),
    ];

    for (case, body) in &refused {
        let (status, response) = dx
            .call("DescribeDirectConnectGatewayAssociations", body.clone())
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a refusal, got {status} with {response}"
        );
        assert!(
            error_code(&response).contains("DirectConnectClientException"),
            "case {case:?}: expected DirectConnectClientException, got {:?}",
            error_code(&response)
        );
        assert_eq!(
            error_message(&response),
            SELECTOR_REQUIRED,
            "case {case:?}: expected the sentence this listing states, got {response}"
        );
    }

    // And a request that DOES carry one selector is served, under every window the
    // refused rows carried: a selector is a filter, so a value naming no association
    // is an empty page and not a client fault.
    let selected: Vec<(&str, Value)> = vec![
        (
            "the stored Direct Connect gateway",
            json!({"directConnectGatewayId": &gateway_id}),
        ),
        (
            "a Direct Connect gateway id naming nothing",
            json!({"directConnectGatewayId": "00000000-0000-4000-8000-000000000000"}),
        ),
        (
            "an associated virtual private gateway",
            json!({"associatedGatewayId": "vgw-0123456789abcdef0"}),
        ),
        (
            "an associated transit gateway",
            json!({"associatedGatewayId": "tgw-0f1e2d3c4b5a69780"}),
        ),
        (
            "the older spelling of the associated gateway",
            json!({"virtualGatewayId": "vgw-0123456789abcdef0"}),
        ),
        (
            "an association id",
            json!({"associationId": "00000000-0000-4000-8000-000000000000"}),
        ),
        (
            "a selector holding nothing at all",
            json!({"directConnectGatewayId": ""}),
        ),
        (
            "a selector holding only space",
            json!({"associationId": "   "}),
        ),
    ];

    for (selector_case, selector) in &selected {
        for window in [
            json!({}),
            json!({"maxResults": 1}),
            json!({"maxResults": MAX_RESULT_SET_SIZE + 1}),
        ] {
            let mut body = selector.as_object().cloned().unwrap_or_default();
            for (member, value) in window.as_object().into_iter().flatten() {
                body.insert(member.clone(), value.clone());
            }
            let case = format!("{selector_case} windowed by {window}");
            let page = associations(&dx, &case, Value::Object(body)).await;
            assert!(
                page.is_empty(),
                "case {case:?}: expected an empty page over a store holding no association, got \
                 {page:?}"
            );
        }
    }

    // A `nextToken` is not a selector either way: beside one it names a page of the
    // selected set, so a token naming nothing in that set is the caller's error.
    let (status, response) = dx
        .call(
            "DescribeDirectConnectGatewayAssociations",
            json!({
                "directConnectGatewayId": &gateway_id,
                "nextToken": "00000000-0000-4000-8000-000000000000",
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case \"a pagination token naming nothing in the selected set\": expected a refusal, got \
         {status} with {response}"
    );
    assert!(
        error_code(&response).contains("DirectConnectClientException"),
        "case \"a pagination token naming nothing in the selected set\": expected \
         DirectConnectClientException, got {:?}",
        error_code(&response)
    );
}

// ---------------------------------------------------------------------------
// Rule: the resource a create is refused ABOUT is the first identifier it resolves,
// and this create resolves the gateway to associate — the account's own resource,
// held by the service that owns gateways — before the Direct Connect gateway the
// association would be made on. So a gateway the account does not hold is reported
// as the missing one, named by the kind its identifier says it is and by the account
// that would own it, whether or not the Direct Connect gateway exists either and
// whichever member of the pair named it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_association_is_refused_about_the_gateway_to_associate_before_the_direct_connect_gateway()
{
    let dx = Emulator::new();
    let stored_gateway = gateway(&dx, "resolution-order").await;
    // A Direct Connect gateway id the store holds no gateway under, so a row naming
    // it would be refused about the Direct Connect gateway if the order were the
    // other way round.
    let no_such_direct_connect_gateway = "00000000-0000-4000-8000-000000000000";
    // The account the gateway would belong to comes from the emulator, never from a
    // recorded response.
    let owner = engine::emulator_account_id();

    // (case, the request body, the gateway the refusal is about, the noun that
    // gateway's kind is named by)
    let rows: Vec<(String, Value, &str, &str)> = vec![
        (
            "a virtual private gateway as gatewayId, beside a Direct Connect gateway that \
             exists"
                .to_string(),
            json!({
                "directConnectGatewayId": &stored_gateway,
                "gatewayId": "vgw-0123456789abcdef0",
            }),
            "vgw-0123456789abcdef0",
            "VirtualGateway",
        ),
        (
            "a virtual private gateway as virtualGatewayId, beside a Direct Connect gateway \
             that does not exist"
                .to_string(),
            json!({
                "directConnectGatewayId": no_such_direct_connect_gateway,
                "virtualGatewayId": "vgw-0f1e2d3c4b5a69780",
            }),
            "vgw-0f1e2d3c4b5a69780",
            "VirtualGateway",
        ),
        (
            "both spellings naming the same virtual private gateway".to_string(),
            json!({
                "directConnectGatewayId": &stored_gateway,
                "gatewayId": "vgw-00112233445566778",
                "virtualGatewayId": "vgw-00112233445566778",
            }),
            "vgw-00112233445566778",
            "VirtualGateway",
        ),
        (
            "a virtual private gateway named with surrounding space".to_string(),
            json!({
                "directConnectGatewayId": &stored_gateway,
                "gatewayId": "  vgw-0aabbccddeeff0011  ",
            }),
            "vgw-0aabbccddeeff0011",
            "VirtualGateway",
        ),
        (
            "a transit gateway with the prefixes such an association advertises".to_string(),
            json!({
                "directConnectGatewayId": &stored_gateway,
                "gatewayId": "tgw-0f1e2d3c4b5a69780",
                "addAllowedPrefixesToDirectConnectGateway": [{"cidr": "10.0.0.0/16"}],
            }),
            "tgw-0f1e2d3c4b5a69780",
            "TransitGateway",
        ),
        (
            "a transit gateway with no prefixes and a Direct Connect gateway that does not \
             exist: the gateway to associate is still what is reported"
                .to_string(),
            json!({
                "directConnectGatewayId": no_such_direct_connect_gateway,
                "virtualGatewayId": "tgw-00112233445566778",
            }),
            "tgw-00112233445566778",
            "TransitGateway",
        ),
    ];

    for (case, body, named, noun) in &rows {
        let (status, response) = dx
            .call("CreateDirectConnectGatewayAssociation", body.clone())
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a refusal, got {status} with {response}"
        );
        assert!(
            error_code(&response).contains("DirectConnectClientException"),
            "case {case:?}: expected DirectConnectClientException, got {:?}",
            error_code(&response)
        );
        assert_eq!(
            error_message(&response),
            format!("The {noun} {named} for owner {owner} does not exist"),
            "case {case:?}: expected the refusal to be about the gateway to associate, got \
             {response}"
        );
        // A refused create stores nothing, for either Direct Connect gateway the rows
        // name.
        for selector in [
            json!({"directConnectGatewayId": &stored_gateway}),
            json!({"directConnectGatewayId": no_such_direct_connect_gateway}),
        ] {
            let page = associations(&dx, case, selector).await;
            assert!(
                page.is_empty(),
                "case {case:?}: a refused create must store no association, got {page:?}"
            );
        }
    }
}
