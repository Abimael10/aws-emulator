// SPDX-License-Identifier: CC0-1.0

//! The not-found answer every campaign-id lookup gives, end to end over the wire.
//!
//! The rule under test: when a well-formed `CampaignId` names no stored campaign, this
//! service answers HTTP 404 `ResourceNotFoundException` with the sentence its own wire
//! states for THAT operation, composed from the request and never from the store. Three
//! sentences, one per plane, and each is the one every operation of that plane shares
//! (`crate::state` is their one home):
//!
//! * the plane that ACTS on a campaign — its lifecycle transitions, the state it is at
//!   and the outbound requests submitted against it — reports the campaign ABSENT, naming
//!   it (`campaign_absent_message`);
//! * the plane that changes a campaign's CONFIGURATION reports the id unresolved, naming
//!   it (`campaign_id_not_found_message`), as do the rename and the deletion of the
//!   campaign itself;
//! * the reading projection and the configuration blocks whose wire states no id report
//!   the id unresolved without it (`CAMPAIGN_NOT_FOUND_MESSAGE`).
//!
//! Asserted over a TABLE spanning every operation that resolves a campaign by `{id}`,
//! crossed with ids spanning `CampaignId` (len 1..256, pattern `^[-:/a-zA-Z0-9]+$`,
//! `smithy_query(query="shape", shape="CampaignId")`). A positive control creates a
//! campaign first, so the 404s below are the lookup failing and not the operations being
//! broken.

use serde_json::{json, Value};
use service_com_amazonaws_connectcampaignsv2_amazonconnectcampaignservicev2::state::{
    campaign_absent_message, campaign_id_not_found_message, CAMPAIGN_NOT_FOUND_MESSAGE,
};

use super::common::{call, error_code, onboard_instance, router, Harness};

/// The region every case here signs for.
const REGION: &str = "us-east-1";

/// The Connect instance the control campaign belongs to.
const INSTANCE: &str = "not-found-control-instance";

/// Which of the three sentences an operation's own wire states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sentence {
    /// The campaign reported absent, naming it.
    Absent,
    /// The id reported unresolved, naming it.
    IdNotFound,
    /// The id reported unresolved without it.
    NotFound,
}

impl Sentence {
    /// The text this sentence states about `id`.
    fn about(self, id: &str) -> String {
        match self {
            Self::Absent => campaign_absent_message(id),
            Self::IdNotFound => campaign_id_not_found_message(id),
            Self::NotFound => CAMPAIGN_NOT_FOUND_MESSAGE.to_string(),
        }
    }
}

/// One row of the table: the operation, the request that reaches its campaign-id lookup
/// (method, path suffix, body) and the sentence its wire states.
struct Row {
    operation: &'static str,
    method: &'static str,
    suffix: &'static str,
    body: Value,
    sentence: Sentence,
}

/// A flow ARN inside `Arn`'s `@length` 20..500 and its pattern, for the rows that carry
/// one.
const FLOW_ARN: &str = "arn:aws:connect:us-east-1:000000000000:instance/i/flow/f";

/// Every operation that resolves a campaign by its `{id}` label, with the sentence its
/// recorded wire states. The routes and required members are the model's
/// (`OPERATIONS.md`, generated from the Smithy model).
fn rows() -> Vec<Row> {
    vec![
        Row {
            operation: "DescribeCampaign",
            method: "GET",
            suffix: "",
            body: Value::Null,
            sentence: Sentence::NotFound,
        },
        Row {
            operation: "GetCampaignState",
            method: "GET",
            suffix: "/state",
            body: Value::Null,
            sentence: Sentence::Absent,
        },
        Row {
            operation: "StartCampaign",
            method: "POST",
            suffix: "/start",
            body: Value::Null,
            sentence: Sentence::Absent,
        },
        Row {
            operation: "PauseCampaign",
            method: "POST",
            suffix: "/pause",
            body: Value::Null,
            sentence: Sentence::Absent,
        },
        Row {
            operation: "ResumeCampaign",
            method: "POST",
            suffix: "/resume",
            body: Value::Null,
            sentence: Sentence::Absent,
        },
        Row {
            operation: "StopCampaign",
            method: "POST",
            suffix: "/stop",
            body: Value::Null,
            sentence: Sentence::Absent,
        },
        Row {
            operation: "DeleteCampaign",
            method: "DELETE",
            suffix: "",
            body: Value::Null,
            sentence: Sentence::IdNotFound,
        },
        Row {
            operation: "PutOutboundRequestBatch",
            method: "PUT",
            suffix: "/outbound-requests",
            body: json!({
                "outboundRequests": [{
                    "clientToken": "token-0001",
                    "expirationTime": "2030-01-01T00:00:00Z",
                    "channelSubtypeParameters": {
                        "telephony": {
                            "destinationPhoneNumber": "+15550100",
                            "attributes": {}
                        }
                    }
                }]
            }),
            sentence: Sentence::Absent,
        },
        Row {
            operation: "UpdateCampaignName",
            method: "POST",
            suffix: "/name",
            body: json!({"name": "renamed-campaign"}),
            sentence: Sentence::IdNotFound,
        },
        Row {
            operation: "UpdateCampaignSource",
            method: "POST",
            suffix: "/source",
            body: json!({"source": {"customerProfilesSegmentArn":
                "arn:aws:profile:us-east-1:000000000000:domains/d/segments/s"}}),
            sentence: Sentence::IdNotFound,
        },
        Row {
            operation: "UpdateCampaignEntryLimits",
            method: "POST",
            suffix: "/entry-limits",
            body: json!({"entryLimitsConfig": {"maxEntryCount": 4, "minEntryInterval": "PT45M"}}),
            sentence: Sentence::IdNotFound,
        },
        Row {
            operation: "UpdateCampaignCommunicationLimits",
            method: "POST",
            suffix: "/communication-limits",
            body: json!({"communicationLimitsOverride": {"allChannelSubtypes": {
                "communicationLimitsList": [
                    {"maxCountPerRecipient": 3, "frequency": 1, "unit": "DAY"}
                ]
            }}}),
            sentence: Sentence::IdNotFound,
        },
        Row {
            operation: "UpdateCampaignFlowAssociation",
            method: "POST",
            suffix: "/flow",
            body: json!({"connectCampaignFlowArn": FLOW_ARN}),
            sentence: Sentence::IdNotFound,
        },
        Row {
            operation: "DeleteCampaignCommunicationLimits",
            method: "DELETE",
            suffix: "/communication-limits?config=ALL_CHANNEL_SUBTYPES",
            body: Value::Null,
            sentence: Sentence::IdNotFound,
        },
        Row {
            operation: "DeleteCampaignEntryLimits",
            method: "DELETE",
            suffix: "/entry-limits",
            body: Value::Null,
            sentence: Sentence::IdNotFound,
        },
        Row {
            operation: "UpdateCampaignSchedule",
            method: "POST",
            suffix: "/schedule",
            body: json!({"schedule": {
                "startTime": "2030-01-01T00:00:00Z",
                "endTime": "2030-01-02T00:00:00Z",
            }}),
            sentence: Sentence::NotFound,
        },
        Row {
            operation: "UpdateCampaignCommunicationTime",
            method: "POST",
            suffix: "/communication-time",
            body: json!({"communicationTimeConfig": {"localTimeZoneConfig": {
                "defaultTimeZone": "America/New_York",
            }}}),
            sentence: Sentence::NotFound,
        },
        Row {
            operation: "UpdateCampaignChannelSubtypeConfig",
            method: "POST",
            suffix: "/channel-subtype-config",
            body: json!({"channelSubtypeConfig": {"telephony": {
                "outboundMode": {"agentless": {}},
                "defaultOutboundConfig": {"connectContactFlowId": "flow-0001"},
            }}}),
            sentence: Sentence::NotFound,
        },
        Row {
            operation: "DeleteCampaignChannelSubtypeConfig",
            method: "DELETE",
            suffix: "/channel-subtype-config?channelSubtype=TELEPHONY",
            body: Value::Null,
            sentence: Sentence::NotFound,
        },
        Row {
            operation: "DeleteCampaignCommunicationTime",
            method: "DELETE",
            suffix: "/communication-time?config=TELEPHONY",
            body: Value::Null,
            sentence: Sentence::NotFound,
        },
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

#[tokio::test]
async fn every_campaign_id_lookup_answers_the_sentence_its_own_wire_states() {
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

    for row in rows() {
        for (id_case, id) in unknown_ids() {
            let case = format!("{} / {id_case}", row.operation);
            let (status, body) = call(
                &router,
                REGION,
                row.method,
                &format!("/v2/campaigns/{id}{}", row.suffix),
                &row.body,
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
                Some(row.sentence.about(&id).as_str()),
                "case {case:?}: expected the {:?} sentence about the id asked for, got {body}",
                row.sentence
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
async fn the_campaign_plane_sentences_are_worded_as_aws_words_them() {
    // The suites above assert the operation -> sentence mapping through the crate's own
    // helpers, which is what the handlers call; this case pins the TEXT of each sentence,
    // so a typo in a helper cannot keep them green. The id is filled from the request —
    // these rows show the template, not one run's value.
    let id = "a-campaign-id";
    assert_eq!(
        campaign_absent_message(id),
        "Campaign: a-campaign-id doesn't exist",
        "the absent-campaign sentence drifted from what the wire carries"
    );
    assert_eq!(
        campaign_id_not_found_message(id),
        "CampaignId not found a-campaign-id.",
        "the id-naming lookup sentence drifted from what the wire carries"
    );
    assert_eq!(
        CAMPAIGN_NOT_FOUND_MESSAGE, "CampaignId not found.",
        "the id-less lookup sentence drifted from what the wire carries"
    );
}

#[tokio::test]
async fn a_deleted_campaign_answers_the_same_sentences() {
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
    // sentence is the operation's, not the request's history.
    for row in rows() {
        let case = format!("{} after the delete", row.operation);
        let (status, body) = call(
            &router,
            REGION,
            row.method,
            &format!("/v2/campaigns/{stored}{}", row.suffix),
            &row.body,
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} {body}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(row.sentence.about(&stored).as_str()),
            "case {case:?}: expected the {:?} sentence about the id asked for, got {body}",
            row.sentence
        );
    }
}
