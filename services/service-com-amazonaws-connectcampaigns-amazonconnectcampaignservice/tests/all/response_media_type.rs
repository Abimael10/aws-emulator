// SPDX-License-Identifier: CC0-1.0

//! The media type every reply of this service ANNOUNCES, end to end over HTTP.
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
//! Asserted over a TABLE whose rows differ along what the rule generalises over: the STATUS
//! class of the reply (200 with a body, 200 with none, 400, 404, 409), which LAYER produced
//! it (a handler's success, a handler's modelled error, the boundary validator, the router's
//! own routing failure, the tag tier's own refusals) and the operation it came from, across
//! every one of this service's planes. The status and the body each row answers with are
//! asserted too, so a row cannot pass by failing in some other way.
//!
//! One reply no author seam of this service reaches keeps the protocol-implied value: the
//! authentication refusal, which the router renders before this service is asked anything.
//! Nothing here drives one — these suites run with request authentication off.

use serde_json::{json, Value};
use service_com_amazonaws_connectcampaigns_amazonconnectcampaignservice::service::RESPONSE_MEDIA_TYPE;

use super::common::{call, router, Harness};

/// The region every case here signs for; the announced media type is a property of the
/// service and not of a region, so one is enough.
const REGION: &str = "us-east-1";

/// The Connect instance the rows that need a served call work against.
const INSTANCE: &str = "media-type-instance";

/// The media type this service's own front door announces on what it answers.
const FRONT_DOOR_MEDIA_TYPE: &str = "application/x-amz-json-1.1";

/// The media type the TAG tier's own refusals are announced under — the protocol's own,
/// which is the value its producer stamped.
const TAG_TIER_MEDIA_TYPE: &str = "application/json";

/// The `{arn}` path label of `/tags/{arn}`, percent-encoded as a single segment.
fn encoded(arn: &str) -> String {
    arn.replace(':', "%3A").replace('/', "%2F")
}

/// A minimal `CreateCampaign` body for `instance`: every member the model marks required
/// (`name`, `connectInstanceId`, `dialerConfig`, `outboundCallConfig`), and nothing else.
fn create_body(name: &str, instance: &str) -> Value {
    json!({
        "name": name,
        "connectInstanceId": instance,
        "dialerConfig": {"progressiveDialerConfig": {"bandwidthAllocation": 0.5}},
        "outboundCallConfig": {"connectContactFlowId": "flow-1"},
    })
}

/// One row: what to send, the status it must answer, what the body must carry — a member
/// name that has to be present (`Some`) or an empty body (`None`) — and the media type the
/// layer that answers it announces.
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
/// `stored` is a campaign created for `INSTANCE`, which the account holds, so the served
/// rows are served and the refusals are refusals of the request rather than of the fixture;
/// `fresh` is a second campaign no row has moved out of the state a create leaves it in, so
/// the state-conflict row is a conflict and nothing else.
fn rows(stored: &str, stored_arn: &str, fresh: &str) -> Vec<Row> {
    // An ARN whose resource path is not a campaign of this service, and a well-formed
    // campaign ARN this store holds nothing under: the two refusals the TAG tier answers,
    // each under its own wire code.
    let not_a_resource = stored_arn.replace("campaign/", "queue/");
    let absent_campaign = stored_arn.replace(stored, "2f9c7b416d054e389a210b7c5d4e3f20");
    vec![
        // A handler's success, with a body: the campaign reads back.
        Row {
            case: "DescribeCampaign on a stored campaign (200, a body)",
            method: "GET",
            path: format!("/campaigns/{stored}"),
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
            path: format!("/campaigns/{stored}/start"),
            body: Value::Null,
            status: 200,
            member: None,
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // A listing: the other success shape, with a list member.
        Row {
            case: "ListCampaigns (200, a list)",
            method: "POST",
            path: "/campaigns-summary".to_string(),
            body: json!({"maxResults": 50}),
            status: 200,
            member: Some("campaignSummaryList"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // A handler's modelled 404 on the campaign plane.
        Row {
            case: "DescribeCampaign on an unknown id (404)",
            method: "GET",
            path: "/campaigns/no-such-campaign".to_string(),
            body: Value::Null,
            status: 404,
            member: Some("message"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // A handler's modelled 404 on the instance plane.
        Row {
            case: "GetConnectInstanceConfig on an unknown instance (404)",
            method: "GET",
            path: "/connect-instance/no-such-instance/config".to_string(),
            body: Value::Null,
            status: 404,
            member: Some("message"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // A handler's modelled 400: the create is gated on an Amazon Connect instance the
        // account holds, and this id names none.
        Row {
            case: "CreateCampaign for an instance the account does not hold (400, a handler)",
            method: "PUT",
            path: "/campaigns".to_string(),
            body: create_body("refused", "an-instance-the-account-does-not-hold"),
            status: 400,
            member: Some("message"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // The boundary validator's own refusal: `name` is @required, so the request is
        // refused BEFORE any handler runs — the layer a codec swap cannot reach.
        Row {
            case: "CreateCampaign with a required member absent (400, the validator)",
            method: "PUT",
            path: "/campaigns".to_string(),
            body: json!({
                "connectInstanceId": INSTANCE,
                "dialerConfig": {"progressiveDialerConfig": {"bandwidthAllocation": 0.5}},
                "outboundCallConfig": {"connectContactFlowId": "flow-1"},
            }),
            status: 400,
            member: Some("message"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // A value outside a modelled bound (`maxResults` is @range(min 1, max 50)),
        // refused by the same layer.
        Row {
            case: "ListCampaigns with maxResults below the modelled minimum (400, the validator)",
            method: "POST",
            path: "/campaigns-summary".to_string(),
            body: json!({"maxResults": 0}),
            status: 400,
            member: Some("message"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // A handler's modelled 409: a campaign that has never run cannot be RESUMED, so
        // this is the state-conflict envelope.
        Row {
            case: "ResumeCampaign on a campaign that has never run (409)",
            method: "POST",
            path: format!("/campaigns/{fresh}/resume"),
            body: Value::Null,
            status: 409,
            member: Some("message"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // The TAG plane's success is the front door's answer, like every other success.
        Row {
            case: "ListTagsForResource on a stored campaign (200)",
            method: "GET",
            path: format!("/tags/{}", encoded(stored_arn)),
            body: Value::Null,
            status: 200,
            member: Some("tags"),
            announced: FRONT_DOOR_MEDIA_TYPE,
        },
        // The TAG tier's own refusals: an ARN whose resource path is not one of this
        // service's resources, and a campaign ARN nothing is stored under. Each carries a
        // code this model declares no shape for, and each is announced under the protocol's
        // own media type rather than the front door's.
        Row {
            case: "ListTagsForResource on an ARN of another resource (400, the tag tier)",
            method: "GET",
            path: format!("/tags/{}", encoded(&not_a_resource)),
            body: Value::Null,
            status: 400,
            member: Some("message"),
            announced: TAG_TIER_MEDIA_TYPE,
        },
        Row {
            case: "ListTagsForResource on an absent campaign (404, the tag tier)",
            method: "GET",
            path: format!("/tags/{}", encoded(&absent_campaign)),
            body: Value::Null,
            status: 404,
            member: Some("message"),
            announced: TAG_TIER_MEDIA_TYPE,
        },
        Row {
            case: "TagResource on an absent campaign (404, the tag tier)",
            method: "POST",
            path: format!("/tags/{}", encoded(&absent_campaign)),
            body: json!({"tags": {"owner": "media-type"}}),
            status: 404,
            member: Some("message"),
            announced: TAG_TIER_MEDIA_TYPE,
        },
    ]
}

/// Give the account `INSTANCE` and create one campaign named `name` for it, handing back
/// the minted id and the ARN it was minted with — the ARN the tag rows address it by.
async fn a_stored_campaign(router: &Harness, name: &str) -> (String, String) {
    router.register_connect_instance(REGION, INSTANCE);
    let created = call(
        router,
        REGION,
        "PUT",
        "/campaigns",
        &create_body(name, INSTANCE),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case \"create the {name} campaign\": expected HTTP 200, got {} {}",
        created.status, created.body
    );
    let id = created.body["id"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case \"create the {name} campaign\": no id in {}",
                created.body
            )
        })
        .to_string();
    let arn = created.body["arn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case \"create the {name} campaign\": no arn in {}",
                created.body
            )
        })
        .to_string();
    (id, arn)
}

#[tokio::test]
async fn every_reply_announces_the_media_type_of_the_layer_that_answered_it() {
    let router = router();
    let (stored, stored_arn) = a_stored_campaign(&router, "media-type-campaign").await;
    let (fresh, _) = a_stored_campaign(&router, "media-type-never-run").await;

    for row in rows(&stored, &stored_arn, &fresh) {
        let answer = call(&router, REGION, row.method, &row.path, &row.body).await;
        assert_eq!(
            answer.status, row.status,
            "case {:?}: expected HTTP {}, got {} {}",
            row.case, row.status, answer.status, answer.body
        );
        match row.member {
            Some(member) => assert!(
                answer.body.get(member).is_some(),
                "case {:?}: expected the body to carry {member:?}, got {}",
                row.case,
                answer.body
            ),
            None => assert_eq!(
                answer.body,
                Value::Null,
                "case {:?}: expected no body, got {}",
                row.case,
                answer.body
            ),
        }
        assert_eq!(
            answer.media_type, row.announced,
            "case {:?}: expected the media type {:?} of the layer that answered it, got {:?}",
            row.case, row.announced, answer.media_type
        );
    }
}

#[tokio::test]
async fn a_request_naming_no_modelled_operation_is_announced_by_the_front_door_too() {
    let router = router();

    // A path this service claims (its first segment is one of its own route segments) with
    // a method no operation of the model binds to it: the router's own routing failure,
    // rendered OUTSIDE the request walk, which the front door announces like every other
    // answer of its own.
    let answer = call(&router, REGION, "PATCH", "/campaigns", &Value::Null).await;
    assert_eq!(
        answer.media_type, FRONT_DOOR_MEDIA_TYPE,
        "case \"a route this service claims but no modelled operation serves\": expected the \
         media type {FRONT_DOOR_MEDIA_TYPE:?} its front door announces, got {:?} on HTTP {} {}",
        answer.media_type, answer.status, answer.body
    );
    assert_eq!(
        answer.status, 501,
        "case \"a route this service claims but no modelled operation serves\": expected the \
         routing failure this protocol's envelope renders, got HTTP {} {}",
        answer.status, answer.body
    );
    assert!(
        answer.body.get("message").is_some(),
        "case \"a route this service claims but no modelled operation serves\": expected the \
         routing failure to carry a message, got {}",
        answer.body
    );
}

#[tokio::test]
async fn the_announced_media_type_is_not_the_one_the_protocol_trait_implies() {
    // The pair itself: the value this service announces is the awsJson 1.1 media type,
    // which is NOT the `application/json` its restJson1 protocol trait implies — so a reply
    // announcing the implied value would be announcing a different media type from the one
    // this service's wire carries.
    assert_eq!(
        RESPONSE_MEDIA_TYPE.announced(),
        FRONT_DOOR_MEDIA_TYPE,
        "the announced media type drifted from what the wire carries"
    );
    assert_eq!(
        RESPONSE_MEDIA_TYPE.implied(),
        TAG_TIER_MEDIA_TYPE,
        "the implied media type drifted from the protocol trait's own"
    );
    assert_ne!(
        RESPONSE_MEDIA_TYPE.announced(),
        RESPONSE_MEDIA_TYPE.implied(),
        "the pair must replace one value with another"
    );
}
