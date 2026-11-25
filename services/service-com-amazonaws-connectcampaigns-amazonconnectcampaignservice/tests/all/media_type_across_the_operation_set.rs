// SPDX-License-Identifier: CC0-1.0

//! The media type this service announces, across its WHOLE modelled operation set, end to
//! end over HTTP.
//!
//! The rule under test is the one `response_media_type.rs` states per status class and per
//! answering layer: the `content-type` of a response is the media type of the layer that
//! ANSWERED it, not the one the protocol trait implies. This service's own front door
//! announces `application/x-amz-json-1.1` on every reply it produces — a success, a modelled
//! refusal its handler raises and a constraint refusal the framework renders before any
//! handler runs alike — while the refusals of its TAG tier keep the protocol's own
//! `application/json` and are recognisable on the wire by the error CODE they carry. The
//! answering layer is a property of the SERVICE, so the value cannot depend on which
//! operation was called.
//!
//! This suite asserts that rule along the dimension the other one generalises over but only
//! samples: the OPERATION. The refusal table spans EVERY operation the service model binds —
//! `smithy_query(query="digest",
//! model_path="services/service-com-amazonaws-connectcampaigns-amazonconnectcampaignservice/smithy-model/com-amazonaws-connectcampaigns-amazonconnectcampaignservice.json")`
//! — each addressed at a resource this store holds nothing under, and the table is checked
//! against [`MODELLED_OPERATIONS`], so an operation added to the model later cannot stay
//! uncovered. The served walk then drives the success path of the instance-onboarding and
//! tag planes, which the other suite's table reaches on neither.
//!
//! The expected value of every case is DERIVED from the error code on the wire rather than
//! written out per case, because that derivation IS the rule: a client error carrying one of
//! the tag tier's own codes is announced under the protocol's media type and every other
//! reply under the front door's. Each case's status and body are asserted too, so a case
//! cannot pass by failing in some other way.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::common::{
    call, hold_the_job_unfinished, reach_the_due_instant, router, Answer, Harness,
};

/// The region every case here signs for; the announced media type is a property of the
/// service and not of a region, so one is enough.
const REGION: &str = "us-east-1";

/// The media type this service's own front door announces on what it answers.
const FRONT_DOOR_MEDIA_TYPE: &str = "application/x-amz-json-1.1";

/// The media type the `restJson1` protocol trait implies, which the tag tier's own refusals
/// keep because that is the value their producer stamped.
const PROTOCOL_MEDIA_TYPE: &str = "application/json";

/// The wire error codes the TAG tier answers under. Neither names a shape this service's
/// model declares, which is what tells that tier's refusals apart from every answer of the
/// modelled front door; the crate states them once (`state::TAG_BAD_REQUEST_CODE` and
/// `state::TAG_NOT_FOUND_CODE`) and they are written out here so this suite pins the codes a
/// client branches on rather than reading them back out of the implementation.
const TAG_TIER_ERROR_CODES: [&str; 2] = ["BadRequestException", "NotFoundException"];

/// Every operation `AmazonConnectCampaignService` binds, from the model's own digest
/// (`smithy_query(query="digest", model_path=…)`, 22 operations). The refusal table has to
/// name all of them and nothing else.
const MODELLED_OPERATIONS: [&str; 22] = [
    "CreateCampaign",
    "DeleteCampaign",
    "DeleteConnectInstanceConfig",
    "DeleteInstanceOnboardingJob",
    "DescribeCampaign",
    "GetCampaignState",
    "GetCampaignStateBatch",
    "GetConnectInstanceConfig",
    "GetInstanceOnboardingJobStatus",
    "ListCampaigns",
    "ListTagsForResource",
    "PauseCampaign",
    "PutDialRequestBatch",
    "ResumeCampaign",
    "StartCampaign",
    "StartInstanceOnboardingJob",
    "StopCampaign",
    "TagResource",
    "UntagResource",
    "UpdateCampaignDialerConfig",
    "UpdateCampaignName",
    "UpdateCampaignOutboundCallConfig",
];

/// The Amazon Connect instance the cases that need a served call work against: one the
/// account holds, which is what makes it onboardable.
const INSTANCE: &str = "media-type-operation-set";

/// An instance id this service holds no onboarding record for, inside `InstanceId`'s
/// `^[a-zA-Z0-9_\-.]*$` pattern.
const ABSENT_INSTANCE: &str = "media-type-absent-instance";

/// A campaign id nothing is stored under, inside `CampaignId`'s `^[\S]*$` pattern.
const ABSENT_CAMPAIGN: &str = "6d1f0b7c48e24a1d93f5c2e70b8a4d16";

/// The tag key the served walk writes and then removes.
const TAG_KEY: &str = "owner";

/// The `{arn}` path label of `/tags/{arn}`, percent-encoded as a single segment.
fn encoded(arn: &str) -> String {
    arn.replace(':', "%3A").replace('/', "%2F")
}

/// The wire error code of `answer` — the last segment of the code a client reads, since both
/// JSON protocols may qualify it with a namespace, and with any trailing detail dropped — or
/// `""` for a reply that carries none.
fn wire_code(answer: &Answer) -> &str {
    answer
        .error_code
        .split('#')
        .next_back()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
}

/// The media type the layer that answered `answer` announces: the protocol's own for a
/// client error carrying one of the TAG tier's codes, and this service's front door's for
/// every other reply. The rule itself, read off the response a client receives.
fn announced_by_the_answering_layer(answer: &Answer) -> &'static str {
    let from_the_tag_tier =
        (400..500).contains(&answer.status) && TAG_TIER_ERROR_CODES.contains(&wire_code(answer));
    if from_the_tag_tier {
        PROTOCOL_MEDIA_TYPE
    } else {
        FRONT_DOOR_MEDIA_TYPE
    }
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

/// One `DialRequest`, carrying every member that shape marks required (`clientToken`,
/// `phoneNumber`, `expirationTime`, `attributes`), so a batch gets past the boundary
/// validator and reaches the campaign lookup.
fn dial_request() -> Value {
    json!({
        "clientToken": "media-type-token",
        "phoneNumber": "+15550100",
        "expirationTime": "2030-01-01T00:00:00Z",
        "attributes": {"purpose": "media-type"},
    })
}

/// An `EncryptionConfig` with the service's own key: `enabled` is the one member the shape
/// marks required, and a disabled config names no KMS key.
fn service_key_config() -> Value {
    json!({"encryptionConfig": {"enabled": false}})
}

/// One case: the operation it calls, what to send, the status it must answer and what its
/// body must carry — a member name that has to be present (`Some`) or no body at all
/// (`None`).
struct Case {
    operation: &'static str,
    method: &'static str,
    path: String,
    body: Value,
    status: u16,
    member: Option<&'static str>,
}

/// Every modelled operation addressed at a resource this store holds nothing under, so each
/// answers the outcome its OWN model declares for that: the not-found error on both resource
/// planes, the create's bad request for an instance the account does not hold, the tag tier's
/// own refusal on an ARN nothing is stored under, and a served answer for the two operations
/// that report per-id or list results rather than refusing.
///
/// `absent_arn` is a campaign ARN of the form this service MINTS, with an id nothing is
/// stored under, so the tag cases are refused by the tier that owns the ARN rather than by
/// its syntax.
fn refusals(absent_arn: &str) -> Vec<Case> {
    vec![
        Case {
            operation: "CreateCampaign",
            method: "PUT",
            path: "/campaigns".to_string(),
            body: create_body("media-type-refused", ABSENT_INSTANCE),
            status: 400,
            member: Some("message"),
        },
        Case {
            operation: "DeleteCampaign",
            method: "DELETE",
            path: format!("/campaigns/{ABSENT_CAMPAIGN}"),
            body: Value::Null,
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "DeleteConnectInstanceConfig",
            method: "DELETE",
            path: format!("/connect-instance/{ABSENT_INSTANCE}/config"),
            body: Value::Null,
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "DeleteInstanceOnboardingJob",
            method: "DELETE",
            path: format!("/connect-instance/{ABSENT_INSTANCE}/onboarding"),
            body: Value::Null,
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "DescribeCampaign",
            method: "GET",
            path: format!("/campaigns/{ABSENT_CAMPAIGN}"),
            body: Value::Null,
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "GetCampaignState",
            method: "GET",
            path: format!("/campaigns/{ABSENT_CAMPAIGN}/state"),
            body: Value::Null,
            status: 404,
            member: Some("message"),
        },
        // The batch answers every id it was asked about, so an id nothing is stored under is
        // a FAILED entry of a served answer rather than a refusal of the call.
        Case {
            operation: "GetCampaignStateBatch",
            method: "POST",
            path: "/campaigns-state".to_string(),
            body: json!({"campaignIds": [ABSENT_CAMPAIGN]}),
            status: 200,
            member: Some("failedRequests"),
        },
        Case {
            operation: "GetConnectInstanceConfig",
            method: "GET",
            path: format!("/connect-instance/{ABSENT_INSTANCE}/config"),
            body: Value::Null,
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "GetInstanceOnboardingJobStatus",
            method: "GET",
            path: format!("/connect-instance/{ABSENT_INSTANCE}/onboarding"),
            body: Value::Null,
            status: 404,
            member: Some("message"),
        },
        // A listing has no not-found outcome: it answers the campaigns the account holds,
        // which is this operation's only reply here.
        Case {
            operation: "ListCampaigns",
            method: "POST",
            path: "/campaigns-summary".to_string(),
            body: json!({"maxResults": 50}),
            status: 200,
            member: Some("campaignSummaryList"),
        },
        Case {
            operation: "ListTagsForResource",
            method: "GET",
            path: format!("/tags/{}", encoded(absent_arn)),
            body: Value::Null,
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "PauseCampaign",
            method: "POST",
            path: format!("/campaigns/{ABSENT_CAMPAIGN}/pause"),
            body: Value::Null,
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "PutDialRequestBatch",
            method: "PUT",
            path: format!("/campaigns/{ABSENT_CAMPAIGN}/dial-requests"),
            body: json!({"dialRequests": [dial_request()]}),
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "ResumeCampaign",
            method: "POST",
            path: format!("/campaigns/{ABSENT_CAMPAIGN}/resume"),
            body: Value::Null,
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "StartCampaign",
            method: "POST",
            path: format!("/campaigns/{ABSENT_CAMPAIGN}/start"),
            body: Value::Null,
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "StartInstanceOnboardingJob",
            method: "PUT",
            path: format!("/connect-instance/{ABSENT_INSTANCE}/onboarding"),
            body: service_key_config(),
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "StopCampaign",
            method: "POST",
            path: format!("/campaigns/{ABSENT_CAMPAIGN}/stop"),
            body: Value::Null,
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "TagResource",
            method: "POST",
            path: format!("/tags/{}", encoded(absent_arn)),
            body: json!({"tags": {TAG_KEY: "media-type"}}),
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "UntagResource",
            method: "DELETE",
            path: format!("/tags/{}?tagKeys={TAG_KEY}", encoded(absent_arn)),
            body: Value::Null,
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "UpdateCampaignDialerConfig",
            method: "POST",
            path: format!("/campaigns/{ABSENT_CAMPAIGN}/dialer-config"),
            body: json!({
                "dialerConfig": {"progressiveDialerConfig": {"bandwidthAllocation": 0.5}}
            }),
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "UpdateCampaignName",
            method: "POST",
            path: format!("/campaigns/{ABSENT_CAMPAIGN}/name"),
            body: json!({"name": "media-type-refused"}),
            status: 404,
            member: Some("message"),
        },
        Case {
            operation: "UpdateCampaignOutboundCallConfig",
            method: "POST",
            path: format!("/campaigns/{ABSENT_CAMPAIGN}/outbound-call-config"),
            body: json!({"connectContactFlowId": "flow-2"}),
            status: 404,
            member: Some("message"),
        },
    ]
}

/// Drive one request through the router, assert its status, its body and the media type the
/// layer that answered it announces, and hand the answer back so a caller can read an id out
/// of it. `case` names the step, so a failure says which one.
async fn announced(
    harness: &Harness,
    case: &str,
    method: &str,
    path: &str,
    body: &Value,
    status: u16,
    member: Option<&str>,
) -> Answer {
    let answer = call(harness, REGION, method, path, body).await;
    assert_eq!(
        answer.status, status, "case {case:?}: expected HTTP {status}, got {} {}",
        answer.status, answer.body
    );
    match member {
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
    let expected = announced_by_the_answering_layer(&answer);
    assert_eq!(
        answer.media_type, expected,
        "case {case:?}: expected the media type {expected:?} of the layer that answered it \
         (wire code {:?}), got {:?}",
        wire_code(&answer),
        answer.media_type
    );
    answer
}

/// Give the account [`INSTANCE`], onboard it, let the clock reach the instant its job is due
/// to have finished, and create one campaign for it — handing back the ARN the create minted,
/// which is the only form of ARN the tag tier resolves.
async fn an_onboarded_instance_with_a_campaign(harness: &Harness) -> String {
    harness.register_connect_instance(REGION, INSTANCE);
    let started = call(
        harness,
        REGION,
        "PUT",
        &format!("/connect-instance/{INSTANCE}/onboarding"),
        &service_key_config(),
    )
    .await;
    assert_eq!(
        started.status, 200,
        "case \"onboard the fixture instance\": expected HTTP 200, got {} {}",
        started.status, started.body
    );
    reach_the_due_instant(harness, INSTANCE);

    let created = call(
        harness,
        REGION,
        "PUT",
        "/campaigns",
        &create_body("media-type-fixture", INSTANCE),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case \"create the fixture campaign\": expected HTTP 200, got {} {}",
        created.status, created.body
    );
    created.body["arn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case \"create the fixture campaign\": no arn in {}",
                created.body
            )
        })
        .to_string()
}

#[tokio::test]
async fn every_modelled_operation_announces_the_layer_that_refused_it() {
    let harness = router();
    let minted_arn = an_onboarded_instance_with_a_campaign(&harness).await;
    // A campaign ARN of the form this service mints, with an id nothing is stored under: the
    // tag cases are then refused by the tier that owns the ARN rather than by its shape.
    let absent_arn = minted_arn
        .rsplit_once('/')
        .map(|(prefix, _)| format!("{prefix}/{ABSENT_CAMPAIGN}"))
        .unwrap_or_else(|| panic!("the minted ARN {minted_arn:?} names no resource path"));

    for case in refusals(&absent_arn) {
        announced(
            &harness,
            case.operation,
            case.method,
            &case.path,
            &case.body,
            case.status,
            case.member,
        )
        .await;
    }
}

#[tokio::test]
async fn the_served_instance_and_tag_planes_announce_the_front_door_too() {
    let harness = router();
    harness.register_connect_instance(REGION, INSTANCE);

    // The instance plane's successes, in the order its onboarding lifecycle reaches them.
    // The repeat start is the `@idempotent` one: it reports the job already under way, which
    // is why the job is held unfinished first rather than waited on.
    let onboarding = format!("/connect-instance/{INSTANCE}/onboarding");
    let config = format!("/connect-instance/{INSTANCE}/config");
    announced(
        &harness,
        "StartInstanceOnboardingJob",
        "PUT",
        &onboarding,
        &service_key_config(),
        200,
        Some("connectInstanceOnboardingJobStatus"),
    )
    .await;
    hold_the_job_unfinished(&harness, INSTANCE);
    announced(
        &harness,
        "StartInstanceOnboardingJob (a repeat while the job runs)",
        "PUT",
        &onboarding,
        &service_key_config(),
        200,
        Some("connectInstanceOnboardingJobStatus"),
    )
    .await;
    announced(
        &harness,
        "GetInstanceOnboardingJobStatus",
        "GET",
        &onboarding,
        &Value::Null,
        200,
        Some("connectInstanceOnboardingJobStatus"),
    )
    .await;
    reach_the_due_instant(&harness, INSTANCE);
    announced(
        &harness,
        "GetConnectInstanceConfig",
        "GET",
        &config,
        &Value::Null,
        200,
        Some("connectInstanceConfig"),
    )
    .await;

    // The tag plane's successes, against a campaign of that instance: its two writes answer
    // a `Unit` output, so they carry no body at all and still announce the media type.
    let created = announced(
        &harness,
        "CreateCampaign",
        "PUT",
        "/campaigns",
        &create_body("media-type-served", INSTANCE),
        200,
        Some("arn"),
    )
    .await;
    let arn = created.body["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"CreateCampaign\": no arn in {}", created.body))
        .to_string();
    let id = created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"CreateCampaign\": no id in {}", created.body))
        .to_string();
    let tags = format!("/tags/{}", encoded(&arn));
    announced(
        &harness,
        "TagResource",
        "POST",
        &tags,
        &json!({"tags": {TAG_KEY: "media-type"}}),
        200,
        None,
    )
    .await;
    announced(
        &harness,
        "ListTagsForResource",
        "GET",
        &tags,
        &Value::Null,
        200,
        Some("tags"),
    )
    .await;
    announced(
        &harness,
        "UntagResource",
        "DELETE",
        &format!("{tags}?tagKeys={TAG_KEY}"),
        &Value::Null,
        200,
        None,
    )
    .await;

    // The two deletes that end the lifecycle: the campaign first, because the instance keeps
    // its configuration while it still owns one.
    announced(
        &harness,
        "DeleteCampaign",
        "DELETE",
        &format!("/campaigns/{id}"),
        &Value::Null,
        200,
        None,
    )
    .await;
    announced(
        &harness,
        "DeleteConnectInstanceConfig",
        "DELETE",
        &config,
        &Value::Null,
        200,
        None,
    )
    .await;
}

#[test]
fn the_refusal_table_spans_every_operation_the_model_binds() {
    let modelled: BTreeSet<&str> = MODELLED_OPERATIONS.into_iter().collect();
    assert_eq!(
        modelled.len(),
        MODELLED_OPERATIONS.len(),
        "case \"the modelled catalog\": it names an operation twice"
    );

    let tabled: BTreeSet<&str> = refusals("arn:aws:connect-campaigns:us-east-1:0:campaign/x")
        .iter()
        .map(|case| case.operation)
        .collect();
    let uncovered: Vec<&str> = modelled.difference(&tabled).copied().collect();
    assert!(
        uncovered.is_empty(),
        "case \"coverage\": every modelled operation needs a row, missing {uncovered:?}"
    );
    let unmodelled: Vec<&str> = tabled.difference(&modelled).copied().collect();
    assert!(
        unmodelled.is_empty(),
        "case \"coverage\": a row names an operation the model does not bind: {unmodelled:?}"
    );
}
