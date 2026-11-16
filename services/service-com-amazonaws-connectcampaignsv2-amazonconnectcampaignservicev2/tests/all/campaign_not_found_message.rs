// SPDX-License-Identifier: CC0-1.0

//! The not-found answer every campaign-id lookup gives, end to end over the wire.
//!
//! The rule under test: when a well-formed `CampaignId` names no stored campaign,
//! this service answers HTTP 404 `ResourceNotFoundException` with its FIXED
//! not-found text — a constant that carries nothing from the request, so the answer
//! is the same whatever id was asked for, and a client that matches or logs on that
//! text matches it. `DeleteCampaign` is the one operation that NAMES the id it was
//! asked to delete, so a caller deleting many campaigns learns which one was missing;
//! its sentence is the same text with the id and a period after it.
//!
//! Asserted over a table: every operation that resolves a campaign by `{id}`
//! (`DescribeCampaign`, `DeleteCampaign`, `GetCampaignState`, `StartCampaign`,
//! `PauseCampaign`, `StopCampaign`) crossed with ids spanning `CampaignId` (len
//! 1..256, pattern `^[-:/a-zA-Z0-9]+$`). A positive control creates a campaign first,
//! so the 404s below are the lookup failing and not the operations being broken.

use serde_json::{json, Value};
use service_com_amazonaws_connectcampaignsv2_amazonconnectcampaignservicev2::state::{
    delete_campaign_not_found_message, CAMPAIGN_NOT_FOUND_MESSAGE,
};

use super::common::{call, error_code, onboard_instance, router, Harness};

/// The region every case here signs for.
const REGION: &str = "us-east-1";

/// The Connect instance the control campaign belongs to.
const INSTANCE: &str = "not-found-control-instance";

/// Onboard `INSTANCE` so `CreateCampaign` has a campaign target, then create one
/// campaign and hand back its id.
async fn a_stored_campaign(router: &Harness) -> String {
    onboard_instance(router, REGION, INSTANCE).await;

    let (status, body) = call(
        router,
        REGION,
        "PUT",
        "/v2/campaigns",
        &json!({
            "name": "control-campaign",
            "connectInstanceId": INSTANCE,
            "channelSubtypeConfig": {
                "telephony": {
                    "outboundMode": {"agentless": {}},
                    "defaultOutboundConfig": {"connectContactFlowId": "flow-0001"}
                }
            },
            "source": {"customerProfilesSegmentArn":
                "arn:aws:profile:us-east-1:000000000000:domains/d/segments/s"},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"create the control campaign\": expected HTTP 200, got {status} {body}"
    );
    body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"create the control campaign\": no id in {body}"))
        .to_string()
}

/// Every operation that resolves a campaign by its `{id}` label, as
/// `(name, method, path suffix)` — the bindings
/// `smithy_query(query="operation", operation="DescribeCampaign,...")` reports.
fn campaign_id_operations() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("DescribeCampaign", "GET", ""),
        ("GetCampaignState", "GET", "/state"),
        ("StartCampaign", "POST", "/start"),
        ("PauseCampaign", "POST", "/pause"),
        ("StopCampaign", "POST", "/stop"),
        ("DeleteCampaign", "DELETE", ""),
    ]
}

/// Ids spanning `CampaignId` (len 1..256, pattern `^[-:/a-zA-Z0-9]+$`): the uuid form
/// a campaign id takes, the shortest id, every punctuation character the pattern
/// admits, the longest id the bound allows, and the zero-padded form this emulator
/// mints (with a digit changed, so it names no campaign).
fn unknown_ids() -> Vec<(&'static str, String)> {
    vec![
        (
            "uuid form",
            "dd584dec-fcfd-444b-93bc-1af01b2b3b1b".to_string(),
        ),
        ("one character (length minimum)", "z".to_string()),
        (
            "every admitted punctuation a path label can carry",
            "a-Z:09".to_string(),
        ),
        ("256 characters (length maximum)", "c".repeat(256)),
        ("the minted id form", "000000009999".to_string()),
    ]
}

/// The not-found sentence `operation` answers for the id `id`: the service's fixed
/// text, which `DeleteCampaign` alone states with the id it was asked to delete.
fn expected_not_found(operation: &str, id: &str) -> String {
    if operation == "DeleteCampaign" {
        delete_campaign_not_found_message(id)
    } else {
        CAMPAIGN_NOT_FOUND_MESSAGE.to_string()
    }
}

#[tokio::test]
async fn every_campaign_id_lookup_answers_the_services_fixed_not_found_text() {
    let router = router();
    let stored = a_stored_campaign(&router).await;

    // Positive control: the id that IS stored reads back, so the operations below
    // are refusing the lookup rather than failing outright.
    let (status, body) = call(
        &router,
        REGION,
        "GET",
        &format!("/v2/campaigns/{stored}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the stored campaign\": expected HTTP 200, got {status} {body}"
    );

    for (operation, method, suffix) in campaign_id_operations() {
        for (id_case, id) in unknown_ids() {
            let case = format!("{operation} / {id_case}");
            let (status, body) = call(
                &router,
                REGION,
                method,
                &format!("/v2/campaigns/{id}{suffix}"),
                &Value::Null,
            )
            .await;
            assert_eq!(
                status, 404,
                "case {case:?}: expected HTTP 404 for an id no campaign carries, got \
                 {status} {body}"
            );
            assert!(
                error_code(&body).ends_with("ResourceNotFoundException"),
                "case {case:?}: expected ResourceNotFoundException, got {}",
                error_code(&body)
            );
            assert_eq!(
                body.get("message").and_then(Value::as_str),
                Some(expected_not_found(operation, &id).as_str()),
                "case {case:?}: expected the service's not-found text for this operation — \
                 the same one whatever the id asked for — got {body}"
            );
        }
    }

    // None of those lookups touched the stored campaign.
    let (status, body) = call(
        &router,
        REGION,
        "GET",
        &format!("/v2/campaigns/{stored}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the stored campaign afterwards\": expected HTTP 200, got {status} {body}"
    );
}

#[tokio::test]
async fn a_deleted_campaign_answers_the_same_fixed_not_found_text() {
    let router = router();
    let stored = a_stored_campaign(&router).await;

    let (status, body) = call(
        &router,
        REGION,
        "DELETE",
        &format!("/v2/campaigns/{stored}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"delete the stored campaign\": expected HTTP 200, got {status} {body}"
    );

    // An id that WAS a campaign is not found the same way one that never was is: the
    // text is the resource's, not the request's.
    for (operation, method, suffix) in campaign_id_operations() {
        let case = format!("{operation} after the delete");
        let (status, body) = call(
            &router,
            REGION,
            method,
            &format!("/v2/campaigns/{stored}{suffix}"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} {body}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(expected_not_found(operation, &stored).as_str()),
            "case {case:?}: expected the service's not-found text for this operation, got {body}"
        );
    }
}
