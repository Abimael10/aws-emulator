// SPDX-License-Identifier: CC0-1.0

//! The media type every reply of this service ANNOUNCES, end to end over the wire.
//!
//! The rule under test: the `content-type` of a response is the media type the layer that
//! ANSWERED it announces, not the one the protocol trait implies. This model declares
//! `restJson1`, whose generic default is `application/json`, while this service's own front
//! door announces `application/x-amz-json-1.1` on every reply it produces — a success, a
//! modelled refusal its handler raises, a constraint refusal the framework renders before
//! any handler runs and a request that names no modelled operation alike — and the media
//! type a client reads is what it keys its parsing and content negotiation off. The refusals
//! of its TAG operations come from another layer, recorded under the protocol's own
//! `application/json`, and carry that layer's own error codes (`BadRequestException`,
//! `NotFoundException`), neither of which names a shape this model declares.
//!
//! Asserted over a TABLE whose rows differ along what the rule generalises over: the
//! STATUS class of the reply (200 with a body, 200 with none, 400, 404, 409, 501), which
//! LAYER produced it (a handler's success, a handler's modelled error, the boundary
//! validator, the router's own routing failure, the tag tier's own refusals) and the
//! operation it came from, across all three of this service's planes. The body each row
//! answers with is asserted too, so a row cannot pass by failing in some other way.
//!
//! One reply no author seam of this service reaches keeps the protocol-implied value: the
//! authentication refusal, which the router renders before this service is asked
//! anything. Nothing here drives one — these suites run with request authentication off.

use serde_json::{json, Value};
use service_com_amazonaws_connectcampaignsv2_amazonconnectcampaignservicev2::service::RESPONSE_MEDIA_TYPE;

use super::common::{
    call_with_headers, create_campaign_with, describe_campaign, onboard_instance, router, Harness,
};

/// The region every case here signs for; the announced media type is a property of the
/// service and not of a region, so one is enough.
const REGION: &str = "us-east-1";

/// The Connect instance the rows that need a served call work against.
const INSTANCE: &str = "media-type-instance";

/// The `{arn}` path label of `/v2/tags/{arn}`, percent-encoded as a single segment.
fn encoded(arn: &str) -> String {
    arn.replace(':', "%3A").replace('/', "%2F")
}

/// The `content-type` `headers` announce, or a marker when the reply carries none.
fn announced(headers: &http::HeaderMap) -> String {
    headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("<no content-type>")
        .to_string()
}

/// The media type this service's own front door announces on what it answers.
const FRONT_DOOR_MEDIA_TYPE: &str = "application/x-amz-json-1.1";

/// The media type the TAG tier's own refusals are announced under — the protocol's own,
/// which is the value its producer stamped.
const TAG_TIER_MEDIA_TYPE: &str = "application/json";

/// One row: what to send, the status it must answer, what the body must carry — a
/// member name that has to be present (`Some`) or an empty body (`None`) — and the media
/// type the layer that answers it announces.
struct Row {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Value,
    status: u16,
    member: Option<&'static str>,
    announced: &'static str,
}

/// The rows: one per status class this service answers and per layer that produces one.
///
/// `stored` is a campaign the fixture created and `INSTANCE` is onboarded, so the served
/// rows are served and the refusals are refusals of the request rather than of the
/// fixture.
fn rows(stored: &str, stored_arn: &str) -> Vec<Row> {
    // An ARN of this service whose resource path is not one of its own resources, and a
    // campaign ARN this region stores nothing under: the two refusals the TAG tier answers,
    // each under its own wire code.
    let not_a_resource = format!(
        "arn:aws:connect-campaigns:{REGION}:{}:queue/media-type",
        emulator_server::emulator_account_id()
    );
    let absent_campaign = format!(
        "arn:aws:connect-campaigns:{REGION}:{}:campaign/00000000-0000-0000-0000-000000000000",
        emulator_server::emulator_account_id()
    );
    vec![
        // A handler's success, with a body: the campaign reads back.
        Row {
            case: "DescribeCampaign on a stored campaign (200, a body)",
            method: "GET",
            path: format!("/v2/campaigns/{stored}"),
            body: Value::Null,
            status: 200,
            member: Some("campaign"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // A handler's success whose output the model declares no members for: the reply
        // carries no body at all, and still announces the media type.
        Row {
            case: "StartCampaign on a stored campaign (200, no body)",
            method: "POST",
            path: format!("/v2/campaigns/{stored}/start"),
            body: Value::Null,
            status: 200,
            member: None,
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // A listing: the other success shape, with a list member.
        Row {
            case: "ListCampaigns (200, a list)",
            method: "POST",
            path: "/v2/campaigns-summary".to_string(),
            body: json!({}),
            status: 200,
            member: Some("campaignSummaryList"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // A handler's modelled 404 on the campaign plane.
        Row {
            case: "DescribeCampaign on an unknown id (404)",
            method: "GET",
            path: "/v2/campaigns/no-such-campaign".to_string(),
            body: Value::Null,
            status: 404,
            member: Some("message"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // A handler's modelled 404 on the instance plane.
        Row {
            case: "GetConnectInstanceConfig on an unknown instance (404)",
            method: "GET",
            path: "/v2/connect-instance/no-such-instance/config".to_string(),
            body: Value::Null,
            status: 404,
            member: Some("message"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // The boundary validator's own refusal: `name` is @required, so the request is
        // refused BEFORE any handler runs — the layer `override_protocol` cannot reach.
        Row {
            case: "CreateCampaign with a required member absent (400, the validator)",
            method: "PUT",
            path: "/v2/campaigns".to_string(),
            body: json!({
                "connectInstanceId": INSTANCE,
                "channelSubtypeConfig": {"telephony": {
                    "outboundMode": {"agentless": {}},
                    "defaultOutboundConfig": {"connectContactFlowId": "flow-0001"},
                }},
            }),
            status: 400,
            member: Some("message"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // A value outside a modelled bound, refused by the same layer.
        Row {
            case: "ListCampaigns with maxResults out of range (400, the validator)",
            method: "POST",
            path: "/v2/campaigns-summary".to_string(),
            body: json!({"maxResults": 9_999}),
            status: 400,
            member: Some("message"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // A handler's modelled 409: the campaign the fixture started cannot be started
        // again, so this is the state-conflict envelope.
        Row {
            case: "StartCampaign on a running campaign (409)",
            method: "POST",
            path: format!("/v2/campaigns/{stored}/start"),
            body: Value::Null,
            status: 409,
            member: Some("message"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // A request that names no modelled operation at all: the router's own routing
        // failure, rendered outside the request walk — for a REST protocol a 501 whose
        // body names the request that resolved to nothing.
        Row {
            case: "a path this model binds no operation to (the routing failure)",
            method: "GET",
            path: "/v2/not-an-operation".to_string(),
            body: Value::Null,
            status: 501,
            member: Some("message"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // The TAG plane's success is the front door's answer, like every other success.
        Row {
            case: "ListTagsForResource on a stored campaign (200)",
            method: "GET",
            path: format!("/v2/tags/{}", encoded(stored_arn)),
            body: Value::Null,
            status: 200,
            member: Some("tags"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // The TAG tier's own refusals: an ARN whose resource path is not one of this
        // service's resources, and a campaign ARN nothing is stored under. Each carries a
        // code this model declares no shape for, and each is announced under the
        // protocol's own media type rather than the front door's.
        Row {
            case: "ListTagsForResource on an ARN of another resource (400, the tag tier)",
            method: "GET",
            path: format!("/v2/tags/{}", encoded(&not_a_resource)),
            body: Value::Null,
            status: 400,
            member: Some("message"),
            announced: TAG_TIER_MEDIA_TYPE,
        },
        Row {
            case: "ListTagsForResource on an absent campaign (404, the tag tier)",
            method: "GET",
            path: format!("/v2/tags/{}", encoded(&absent_campaign)),
            body: Value::Null,
            status: 404,
            member: Some("message"),
            announced: TAG_TIER_MEDIA_TYPE,
        },
        Row {
            case: "TagResource on an absent campaign (404, the tag tier)",
            method: "POST",
            path: format!("/v2/tags/{}", encoded(&absent_campaign)),
            body: json!({"tags": {"owner": "media-type"}}),
            status: 404,
            member: Some("message"),
            announced: TAG_TIER_MEDIA_TYPE,
        },
    ]
}

/// Create the fixture's campaign, with its instance onboarded first, and answer its id and
/// the ARN it was minted with — the ARN the tag rows address it by.
async fn a_stored_campaign(router: &Harness) -> (String, String) {
    onboard_instance(router, REGION, INSTANCE).await;
    // Created WITH a tag, so the tag plane's success row has a tag map to answer with.
    let id = create_campaign_with(
        router,
        REGION,
        INSTANCE,
        "media-type-campaign",
        json!({"tags": {"owner": "media-type"}}),
    )
    .await;
    let campaign = describe_campaign(router, REGION, &id, "the media-type fixture").await;
    let arn = campaign["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("the stored campaign carries its minted arn: {campaign}"))
        .to_string();
    (id, arn)
}

#[tokio::test]
async fn every_reply_announces_the_media_type_of_the_layer_that_answered_it() {
    let router = router();
    let (stored, stored_arn) = a_stored_campaign(&router).await;

    for row in rows(&stored, &stored_arn) {
        let (status, headers, body) =
            call_with_headers(&router, REGION, row.method, &row.path, &row.body).await;
        assert_eq!(
            status, row.status,
            "case {:?}: expected HTTP {}, got {status} {body}",
            row.case, row.status
        );
        match row.member {
            Some(member) => assert!(
                body.get(member).is_some(),
                "case {:?}: expected the body to carry {member:?}, got {body}",
                row.case
            ),
            None => assert_eq!(
                body,
                Value::Null,
                "case {:?}: expected no body, got {body}",
                row.case
            ),
        }
        assert_eq!(
            announced(&headers),
            row.announced,
            "case {:?}: expected the media type {:?} of the layer that answered it, got {:?}",
            row.case,
            row.announced,
            announced(&headers)
        );
    }
}

#[tokio::test]
async fn the_announced_media_type_is_not_the_one_the_protocol_trait_implies() {
    // The pair itself: the value this service announces is the awsJson 1.1 media type,
    // which is NOT the `application/json` its restJson1 protocol trait implies — so a
    // reply announcing the implied value would be announcing a different media type from
    // the one the service is contracted to produce.
    assert_eq!(
        RESPONSE_MEDIA_TYPE.announced(),
        "application/x-amz-json-1.1",
        "the announced media type drifted from what the wire carries"
    );
    assert_eq!(
        RESPONSE_MEDIA_TYPE.implied(),
        "application/json",
        "the implied media type drifted from the protocol trait's own"
    );
    assert_ne!(
        RESPONSE_MEDIA_TYPE.announced(),
        RESPONSE_MEDIA_TYPE.implied(),
        "the pair must replace one value with another"
    );
}
