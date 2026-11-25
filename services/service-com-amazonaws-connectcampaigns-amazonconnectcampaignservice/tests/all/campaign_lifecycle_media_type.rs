// SPDX-License-Identifier: CC0-1.0

//! The media type the CAMPAIGN LIFECYCLE plane and the campaign LISTING announce, end to
//! end over HTTP.
//!
//! The rule under test: every reply this service's own modelled front door produces
//! announces the media type that front door labels its JSON bodies with
//! ([`FRONT_DOOR_MEDIA_TYPE`]), whatever operation answered it, whatever status class it
//! carries and whichever layer inside the request walk produced it — a handler's success,
//! a handler's modelled refusal, or the boundary validator's refusal of a value outside a
//! modelled bound. It is NOT the `application/json` this model's `restJson1` protocol
//! trait implies, and a client keys its parsing and content negotiation off what it reads,
//! so an otherwise byte-correct reply announced under the implied value is misclassified.
//!
//! `response_media_type.rs` beside this file asserts that rule across the LAYERS that
//! answer for this service (including the tag tier, whose own refusals keep the media type
//! their producer stamped), and this suite takes the value, the fixture and the create body
//! from it rather than restating them. What it adds is the same rule across the whole
//! campaign lifecycle plane: every state-moving and every amending operation of a campaign,
//! each one both SERVED on a stored campaign and REFUSED for an id the store holds nothing
//! under, plus the listing across the page shapes it answers and both sides of the one
//! bound its input declares. Every row asserts the status, the wire error code of a refusal
//! and the body member a success carries too, so a row cannot pass by failing in some other
//! way.

use serde_json::{json, Value};

use super::common::{call, router, Answer, Harness};
use super::response_media_type::{a_stored_campaign, FRONT_DOOR_MEDIA_TYPE, INSTANCE, REGION};

/// The published bounds of `ListCampaigns$maxResults` — `@range(min 1, max 50)`, from
/// `smithy_query(query="operation", operation="ListCampaigns")`.
const MAX_RESULTS_MIN: i32 = 1;
const MAX_RESULTS_MAX: i32 = 50;

/// A `dialerConfig` of the same dialer KIND the campaigns of this suite are created with,
/// which is what an amendment of a stored campaign's dialer may carry.
fn dialer_config() -> Value {
    json!({"progressiveDialerConfig": {"bandwidthAllocation": 0.75}})
}

/// Assert that `answer` announces the front door's media type, naming the case.
fn announces_the_front_door(case: &str, answer: &Answer) {
    assert_eq!(
        answer.media_type, FRONT_DOOR_MEDIA_TYPE,
        "case {case:?}: expected the media type {FRONT_DOOR_MEDIA_TYPE:?} this service's \
         front door announces, got {:?} on HTTP {} {}",
        answer.media_type, answer.status, answer.body
    );
}

/// One request of the campaign lifecycle plane: the HTTP method, the path template the
/// model binds it to (with `{id}` still in it) and the body it carries.
struct PlaneCall {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    body: Value,
    /// The body member a SERVED answer carries, or `None` for an operation whose output
    /// the model declares no members for.
    member: Option<&'static str>,
}

/// Every operation of the campaign lifecycle plane, as one request each, in an order each
/// one's own transition rule accepts when they are served in sequence against a campaign
/// a create has just minted: the amendments and the reads, then the four state movers, and
/// the delete last. Across all three HTTP methods the model binds this plane to, with and
/// without a request body.
fn plane_calls() -> Vec<PlaneCall> {
    vec![
        PlaneCall {
            case: "UpdateCampaignName",
            method: "POST",
            path: "/campaigns/{id}/name",
            body: json!({"name": "renamed-by-the-media-type-suite"}),
            member: None,
        },
        PlaneCall {
            case: "UpdateCampaignOutboundCallConfig",
            method: "POST",
            path: "/campaigns/{id}/outbound-call-config",
            body: json!({"connectContactFlowId": "flow-2"}),
            member: None,
        },
        PlaneCall {
            case: "UpdateCampaignDialerConfig",
            method: "POST",
            path: "/campaigns/{id}/dialer-config",
            body: json!({"dialerConfig": dialer_config()}),
            member: None,
        },
        PlaneCall {
            case: "DescribeCampaign",
            method: "GET",
            path: "/campaigns/{id}",
            body: Value::Null,
            member: Some("campaign"),
        },
        PlaneCall {
            case: "GetCampaignState",
            method: "GET",
            path: "/campaigns/{id}/state",
            body: Value::Null,
            member: Some("state"),
        },
        PlaneCall {
            case: "StartCampaign",
            method: "POST",
            path: "/campaigns/{id}/start",
            body: Value::Null,
            member: None,
        },
        PlaneCall {
            case: "PauseCampaign",
            method: "POST",
            path: "/campaigns/{id}/pause",
            body: Value::Null,
            member: None,
        },
        PlaneCall {
            case: "ResumeCampaign",
            method: "POST",
            path: "/campaigns/{id}/resume",
            body: Value::Null,
            member: None,
        },
        PlaneCall {
            case: "StopCampaign",
            method: "POST",
            path: "/campaigns/{id}/stop",
            body: Value::Null,
            member: None,
        },
        PlaneCall {
            case: "DeleteCampaign",
            method: "DELETE",
            path: "/campaigns/{id}",
            body: Value::Null,
            member: None,
        },
    ]
}

/// The campaign ids the store holds nothing under, differing in form — the shape of the id
/// is not what the rule turns on, so the refusal of each is announced the same way. Every
/// one is within `CampaignId` (len 0..256, pattern `^[\S]*$`).
const ABSENT_IDS: [&str; 3] = [
    "2f9c7b416d054e389a210b7c5d4e3f20",
    "no-such-campaign",
    "A.mixed_Case-id.with~other-characters",
];

#[tokio::test]
async fn every_lifecycle_refusal_of_an_absent_campaign_announces_the_front_door_media_type() {
    let harness = router();
    // A stored campaign of the same instance, so the store this plane reads is NOT empty
    // and each refusal is a refusal of the id the request named.
    let _neighbour = a_stored_campaign(&harness, "lifecycle-media-type-neighbour").await;

    let requests = plane_calls();
    for absent in ABSENT_IDS {
        for request in &requests {
            let case = format!("{} on the absent campaign {absent:?}", request.case);
            let path = request.path.replace("{id}", absent);
            let answer = call(&harness, REGION, request.method, &path, &request.body).await;
            assert_eq!(
                answer.status, 404,
                "case {case:?}: expected HTTP 404 for an id the store holds nothing under, \
                 got {} {}",
                answer.status, answer.body
            );
            assert!(
                answer.error_code.ends_with("ResourceNotFoundException"),
                "case {case:?}: expected the modelled ResourceNotFoundException, got {:?}",
                answer.error_code
            );
            assert!(
                answer.body.get("message").is_some(),
                "case {case:?}: expected the refusal to carry a message, got {}",
                answer.body
            );
            announces_the_front_door(&case, &answer);
        }
    }
}

#[tokio::test]
async fn every_served_lifecycle_call_announces_the_front_door_media_type() {
    let harness = router();
    let (id, _arn) = a_stored_campaign(&harness, "lifecycle-media-type-campaign").await;

    for request in plane_calls() {
        let case = format!("{} on the stored campaign", request.case);
        let path = request.path.replace("{id}", &id);
        let answer = call(&harness, REGION, request.method, &path, &request.body).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 on a stored campaign, got {} {}",
            answer.status, answer.body
        );
        match request.member {
            Some(member) => assert!(
                answer.body.get(member).is_some(),
                "case {case:?}: expected the body to carry {member:?}, got {}",
                answer.body
            ),
            None => assert_eq!(
                answer.body,
                Value::Null,
                "case {case:?}: expected no body, got {}",
                answer.body
            ),
        }
        announces_the_front_door(&case, &answer);
    }

    // The delete the sequence above ended with really removed the campaign, and the
    // refusal that proves it is announced by the same front door as the success before it.
    let gone = call(
        &harness,
        REGION,
        "GET",
        &format!("/campaigns/{id}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        gone.status, 404,
        "case \"DescribeCampaign after the delete\": expected HTTP 404, got {} {}",
        gone.status, gone.body
    );
    announces_the_front_door("DescribeCampaign after the delete", &gone);
}

/// `ListCampaigns` with `body`, through `harness`.
async fn list_campaigns(harness: &Harness, body: &Value) -> Answer {
    call(harness, REGION, "POST", "/campaigns-summary", body).await
}

#[tokio::test]
async fn every_listing_page_and_bound_refusal_announces_the_front_door_media_type() {
    let harness = router();
    // Two campaigns, so a page of one leaves a page to follow and the token that names it.
    let _first = a_stored_campaign(&harness, "listing-media-type-first").await;
    let _second = a_stored_campaign(&harness, "listing-media-type-second").await;

    // The page shapes this listing answers: no body at all, an empty body, each published
    // bound of `maxResults`, and a page filtered to one instance.
    let served = [
        ("ListCampaigns with no request body", Value::Null),
        ("ListCampaigns with an empty request body", json!({})),
        (
            "ListCampaigns at the published minimum page size",
            json!({"maxResults": MAX_RESULTS_MIN}),
        ),
        (
            "ListCampaigns at the published maximum page size",
            json!({"maxResults": MAX_RESULTS_MAX}),
        ),
        (
            "ListCampaigns filtered to one instance",
            json!({"filters": {"instanceIdFilter": {"value": INSTANCE, "operator": "Eq"}}}),
        ),
    ];

    for (case, body) in served {
        let answer = list_campaigns(&harness, &body).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} {}",
            answer.status, answer.body
        );
        assert!(
            answer.body.get("campaignSummaryList").is_some(),
            "case {case:?}: expected the page to carry campaignSummaryList, got {}",
            answer.body
        );
        announces_the_front_door(case, &answer);
    }

    // The next page, reached through the token the one-campaign page handed back: a
    // success of the same listing that is not its first page.
    let first_page = list_campaigns(&harness, &json!({"maxResults": MAX_RESULTS_MIN})).await;
    let token = first_page.body["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case \"ListCampaigns at the published minimum page size\": expected a token \
                 for the page that follows, got {}",
                first_page.body
            )
        })
        .to_string();
    let next_page = list_campaigns(&harness, &json!({"nextToken": token})).await;
    assert_eq!(
        next_page.status, 200,
        "case \"ListCampaigns resumed from a token\": expected HTTP 200, got {} {}",
        next_page.status, next_page.body
    );
    announces_the_front_door("ListCampaigns resumed from a token", &next_page);

    // One step outside each published bound: refused by the boundary validator, before any
    // handler of this service runs, and announced by the same front door.
    for maxresults in [MAX_RESULTS_MIN - 1, MAX_RESULTS_MAX + 1] {
        let case = format!("ListCampaigns with maxResults {maxresults}, outside the bound");
        let answer = list_campaigns(&harness, &json!({"maxResults": maxresults})).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400 for a value outside the modelled bound, got {} {}",
            answer.status, answer.body
        );
        assert!(
            answer.error_code.ends_with("ValidationException"),
            "case {case:?}: expected the modelled ValidationException, got {:?}",
            answer.error_code
        );
        assert!(
            answer.body.get("message").is_some(),
            "case {case:?}: expected the refusal to carry a message, got {}",
            answer.body
        );
        announces_the_front_door(&case, &answer);
    }
}
