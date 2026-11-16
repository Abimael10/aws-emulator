// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS a request it refused for failing the rules its model
//! declares, end to end over the wire.
//!
//! Every case drives real HTTP through the router the production entrypoint serves, so
//! each assertion exercises the restJson1 wire protocol, the modelled constraints and
//! the handlers together. Three rules, each held over a table of members and
//! operations rather than over the one request that showed it:
//!
//! * a `@pattern` is stated as the expression BODY — the `^`/`$` the model anchors it
//!   with are not part of what the refusal reports;
//! * the Amazon Connect instance id is never echoed: the member that carries one is
//!   named, its value is not;
//! * a violation of a constraint on an ELEMENT of a list of scalars is reported
//!   against the LIST member, with the list rendered in brackets and the element
//!   shape's whole constraint set stated as one composite requirement.
//!
//! and one operation's refusal body is double-encoded: `PutOutboundRequestBatch`
//! carries the clause nested in a `{ "message": … }` JSON document, which its sibling
//! `PutProfileOutboundRequestBatch` does not.
//!
//! Every row is HTTP 400 `ValidationException`, and every expected message is spelled
//! out here rather than taken from the crate, so the assertions pin the wording and
//! not merely the fact that every refusal goes through one renderer.

use serde_json::{json, Value};
use service_com_amazonaws_connectcampaignsv2_amazonconnectcampaignservicev2::state::{
    CAMPAIGN_ID_MAX_LENGTH, CAMPAIGN_ID_MIN_LENGTH,
};

use super::common::{call, create_campaign, error_code, error_message, onboard_instance, router};

/// The region every case here signs for: nothing in these rules is region-dependent.
const REGION: &str = "us-east-1";

/// `InstanceId`'s modelled `@pattern` body, without the anchors the model writes
/// around it — `^[-_.a-zA-Z0-9]+$` (source: `smithy_query(query="shape",
/// shape="InstanceId")`).
const INSTANCE_ID_PATTERN_BODY: &str = "[-_.a-zA-Z0-9]+";

/// `CampaignId`'s modelled `@pattern` body, from `^[-:/a-zA-Z0-9]+$`.
const CAMPAIGN_ID_PATTERN_BODY: &str = "[-:/a-zA-Z0-9]+";

/// `ClientToken`'s modelled `@pattern` body, from `^[a-zA-Z0-9_\-.]*$`.
const CLIENT_TOKEN_PATTERN_BODY: &str = r"[a-zA-Z0-9_\-.]*";

/// `Iso8601Duration`'s modelled `@pattern` body — the shape
/// `Schedule$refreshFrequency` targets.
const DURATION_PATTERN_BODY: &str =
    r"P(?:([-+]?[0-9]+)D)?(T(?:([-+]?[0-9]+)H)?(?:([-+]?[0-9]+)M)?(?:([-+]?[0-9]+)(?:[.,]([0-9]{0,9}))?S)?)?";

/// The instant an outbound request in these cases expires at — far enough ahead that
/// no row is about expiry.
const FAR_FUTURE: &str = "2099-01-01T00:00:00Z";

/// A telephony channel-subtype config, whose `outboundMode` and
/// `defaultOutboundConfig` the model both require.
fn telephony_config() -> Value {
    json!({"telephony": {
        "outboundMode": {"agentless": {}},
        "defaultOutboundConfig": {"connectContactFlowId": "flow-0001"},
    }})
}

/// The whole envelope one clause arrives in.
fn one_error(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// The clause a member at `path` holding `value` is refused with for failing
/// `requirement`.
fn clause(value: &str, path: &str, requirement: &str) -> String {
    format!("Value '{value}' at '{path}' failed to satisfy constraint: {requirement}")
}

/// The requirement a `@pattern` violation states: the expression body.
fn pattern_requirement(body: &str) -> String {
    format!("Member must satisfy regular expression pattern: {body}")
}

/// A pattern refusal states the expression BODY, over a TABLE spanning the members
/// whose `@pattern` a request can violate: a body member, a member inside a list
/// element, a member behind an optional structure and a `@httpLabel`.
///
/// The row's own pattern is asserted to be reported unanchored, and the anchored
/// spelling the model writes is asserted ABSENT — so a refusal cannot pass by
/// carrying both.
#[tokio::test]
async fn a_pattern_refusal_states_the_expression_body() {
    let router = router();
    let instance = "pattern-refusal-instance";
    let campaign = create_campaign(&router, REGION, instance, "pattern-refusal", None).await;

    // (case, method, path, body, the pattern body the refusal must state)
    let rows: Vec<(&str, &str, String, Value, &str)> = vec![
        (
            "CreateCampaign's connectInstanceId",
            "PUT",
            "/v2/campaigns".to_string(),
            json!({
                "name": "pattern-row",
                "connectInstanceId": "not a valid instance id!",
                "channelSubtypeConfig": telephony_config(),
            }),
            INSTANCE_ID_PATTERN_BODY,
        ),
        (
            "ListCampaigns' instance filter value",
            "POST",
            "/v2/campaigns-summary".to_string(),
            json!({"filters": {"instanceIdFilter": {
                "value": "not a valid instance id!",
                "operator": "Eq",
            }}}),
            INSTANCE_ID_PATTERN_BODY,
        ),
        (
            "GetCampaignStateBatch's campaign ids",
            "POST",
            "/v2/campaigns-state".to_string(),
            json!({"campaignIds": ["not a valid id!"]}),
            CAMPAIGN_ID_PATTERN_BODY,
        ),
        (
            "PutOutboundRequestBatch's client token",
            "PUT",
            format!("/v2/campaigns/{campaign}/outbound-requests"),
            json!({"outboundRequests": [{
                "clientToken": "bad token! not allowed",
                "expirationTime": FAR_FUTURE,
                "channelSubtypeParameters": {"telephony": {
                    "destinationPhoneNumber": "+15555550100",
                    "attributes": {},
                }},
            }]}),
            CLIENT_TOKEN_PATTERN_BODY,
        ),
        (
            "UpdateCampaignSchedule's refresh frequency",
            "POST",
            format!("/v2/campaigns/{campaign}/schedule"),
            json!({"schedule": {
                "startTime": "2099-01-01T00:00:00Z",
                "endTime": "2099-01-02T00:00:00Z",
                "refreshFrequency": "every day",
            }}),
            DURATION_PATTERN_BODY,
        ),
        (
            "DescribeCampaign's id path label",
            "GET",
            "/v2/campaigns/not!an!id".to_string(),
            Value::Null,
            CAMPAIGN_ID_PATTERN_BODY,
        ),
    ];

    for (case, method, path, body, pattern_body) in rows {
        let (status, response) = call(&router, REGION, method, &path, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {response}"
        );
        assert!(
            error_code(&response).ends_with("ValidationException"),
            "case {case:?}: expected ValidationException, got {}",
            error_code(&response)
        );
        let message = error_message(&response);
        assert!(
            message.contains(&pattern_requirement(pattern_body)),
            "case {case:?}: expected the unanchored pattern {pattern_body:?}, got {message:?}"
        );
        assert!(
            !message.contains(&format!("^{pattern_body}$")),
            "case {case:?}: expected no anchored pattern in the refusal, got {message:?}"
        );
    }
}

/// The Connect instance id is never echoed: the member is named, the value the caller
/// sent is not — over every member of this service's inputs that carries one.
#[tokio::test]
async fn a_connect_instance_id_is_named_but_not_echoed() {
    let router = router();

    // A value outside `InstanceId`'s pattern that no URI path can carry a space in, so
    // the same value serves a body member and a path label alike.
    let bad = "not!valid";

    // (case, method, path, body, the member path the refusal names)
    let rows: Vec<(&str, &str, String, Value, &str)> = vec![
        (
            "CreateCampaign's connectInstanceId",
            "PUT",
            "/v2/campaigns".to_string(),
            json!({
                "name": "elided-row",
                "connectInstanceId": bad,
                "channelSubtypeConfig": telephony_config(),
            }),
            "connectInstanceId",
        ),
        (
            "ListCampaigns' instance filter value",
            "POST",
            "/v2/campaigns-summary".to_string(),
            json!({"filters": {"instanceIdFilter": {"value": bad, "operator": "Eq"}}}),
            "filters.instanceIdFilter.value",
        ),
        (
            "StartInstanceOnboardingJob's path label",
            "PUT",
            format!("/v2/connect-instance/{bad}/onboarding"),
            json!({"encryptionConfig": {"enabled": false}}),
            "connectInstanceId",
        ),
        (
            "GetConnectInstanceConfig's path label",
            "GET",
            format!("/v2/connect-instance/{bad}/config"),
            Value::Null,
            "connectInstanceId",
        ),
    ];

    for (case, method, path, body, member) in rows {
        let (status, response) = call(&router, REGION, method, &path, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {response}"
        );
        let message = error_message(&response);
        let expected = one_error(&format!(
            "Value at '{member}' failed to satisfy constraint: {}",
            pattern_requirement(INSTANCE_ID_PATTERN_BODY)
        ));
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains(bad),
            "case {case:?}: the instance id must not be echoed, got {message:?}"
        );
    }
}

/// A constraint on the ELEMENTS of a list of scalars is reported against the LIST
/// member: the whole list in brackets, and the element shape's constraints as one
/// composite set — whichever of them the value failed.
///
/// The rows differ along what the rule generalises over: which constraint the element
/// failed (the pattern, each published length bound), how many elements failed, and
/// where in the list they sit.
#[tokio::test]
async fn a_scalar_lists_elements_are_reported_against_the_list() {
    let router = router();

    // The element shape's whole constraint set, as the composite requirement states
    // it: `CampaignId` is @length 1..256 with the pattern above.
    let composite = format!(
        "Member must satisfy constraint: [Member must have length less than or equal to \
         {CAMPAIGN_ID_MAX_LENGTH}, Member must have length greater than or equal to \
         {CAMPAIGN_ID_MIN_LENGTH}, {}]",
        pattern_requirement(CAMPAIGN_ID_PATTERN_BODY)
    );

    let too_long = "c".repeat((CAMPAIGN_ID_MAX_LENGTH + 1) as usize);
    // A value past the ceiling a refusal echoes arrives as a prefix of that many bytes
    // followed by its own length, which is how every refusal in this emulator shows an
    // over-long value (`MAX_ECHOED_VALUE` in `aws_smithy_schema_server::constraints`).
    const MAX_ECHOED_VALUE: usize = 256;
    let too_long_echoed = format!(
        "{}... [{} bytes]",
        &too_long[..MAX_ECHOED_VALUE],
        too_long.len()
    );
    // (case, the ids the request sends, the list as the refusal renders it)
    let rows: Vec<(&str, Vec<String>, String)> = vec![
        (
            "one id outside the pattern",
            vec!["not a valid id!".to_string()],
            "not a valid id!".to_string(),
        ),
        (
            "two ids outside the pattern",
            vec!["bad one!".to_string(), "bad two!".to_string()],
            "bad one!, bad two!".to_string(),
        ),
        (
            "an id past the length maximum",
            vec![too_long.clone()],
            too_long_echoed,
        ),
        (
            "an id below the length minimum",
            vec![String::new()],
            String::new(),
        ),
        (
            "a bad id after a good one",
            vec!["000000000000".to_string(), "still bad!".to_string()],
            "still bad!".to_string(),
        ),
    ];

    for (case, ids, rendered) in rows {
        let (status, response) = call(
            &router,
            REGION,
            "POST",
            "/v2/campaigns-state",
            &json!({"campaignIds": ids}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {response}"
        );
        let message = error_message(&response);
        let expected = one_error(&clause(&format!("[{rendered}]"), "campaignIds", &composite));
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains("campaignIds.1.member"),
            "case {case:?}: the refusal names the list member, not the element, got {message:?}"
        );
    }
}

/// `PutOutboundRequestBatch`'s refusal body is double-encoded: the clause arrives
/// nested in a `{ "message": … }` JSON document. Its sibling batch operation's does
/// not, so the envelope belongs to that one operation and not to every batch.
///
/// The rows differ along which constraint of the batch failed, so the envelope is not
/// an artefact of one member.
#[tokio::test]
async fn the_outbound_batch_states_its_refusal_in_a_json_document() {
    let router = router();
    let instance = "double-encoded-instance";
    let campaign = create_campaign(&router, REGION, instance, "double-encoded", None).await;

    let telephony_parameters =
        json!({"telephony": {"destinationPhoneNumber": "+15555550100", "attributes": {}}});
    // (case, the request the batch sends, the clause the document must carry)
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "a client token outside its pattern",
            json!({"outboundRequests": [{
                "clientToken": "bad token! not allowed",
                "expirationTime": FAR_FUTURE,
                "channelSubtypeParameters": telephony_parameters,
            }]}),
            clause(
                "bad token! not allowed",
                "outboundRequests.1.member.clientToken",
                &pattern_requirement(CLIENT_TOKEN_PATTERN_BODY),
            ),
        ),
        (
            "a second request's client token",
            json!({"outboundRequests": [
                {
                    "clientToken": "well-formed-token",
                    "expirationTime": FAR_FUTURE,
                    "channelSubtypeParameters": telephony_parameters,
                },
                {
                    "clientToken": "also bad!",
                    "expirationTime": FAR_FUTURE,
                    "channelSubtypeParameters": telephony_parameters,
                },
            ]}),
            clause(
                "also bad!",
                "outboundRequests.2.member.clientToken",
                &pattern_requirement(CLIENT_TOKEN_PATTERN_BODY),
            ),
        ),
    ];

    for (case, body, inner) in rows {
        let (status, response) = call(
            &router,
            REGION,
            "PUT",
            &format!("/v2/campaigns/{campaign}/outbound-requests"),
            &body,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {response}"
        );
        assert!(
            error_code(&response).ends_with("ValidationException"),
            "case {case:?}: expected ValidationException, got {}",
            error_code(&response)
        );
        let message = error_message(&response);
        let expected = format!("{{ \"message\": \"{}\" }}", one_error(&inner));
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // The sibling batch is not double-encoded: its clause arrives bare.
    let (status, response) = call(
        &router,
        REGION,
        "PUT",
        &format!("/v2/campaigns/{campaign}/profile-outbound-requests"),
        &json!({"profileOutboundRequests": [{
            "clientToken": "bad token! not allowed",
            // `ProfileId`'s own `@pattern` is 32 hex characters, so this row's only
            // offending member is the token.
            "profileId": "0123456789abcdef0123456789abcdef",
            "expirationTime": FAR_FUTURE,
        }]}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"the profile batch's client token\": expected HTTP 400, got {status} {response}"
    );
    let message = error_message(&response);
    assert!(
        message.starts_with("1 validation error detected:"),
        "case \"the profile batch's client token\": expected a bare clause, got {message:?}"
    );
}

/// The positive control: a request every one of those constraints admits is SERVED, so
/// the refusals above are the values and not the operations being broken.
#[tokio::test]
async fn a_request_inside_every_constraint_is_served() {
    let router = router();
    let instance = "served-validation-instance";
    onboard_instance(&router, REGION, instance).await;
    let campaign = create_campaign(&router, REGION, instance, "served-validation", None).await;

    // (case, method, path, body)
    let served: Vec<(&str, &str, String, Value)> = vec![
        (
            "ListCampaigns filtered by the onboarded instance",
            "POST",
            "/v2/campaigns-summary".to_string(),
            json!({"filters": {"instanceIdFilter": {"value": instance, "operator": "Eq"}}}),
        ),
        (
            "GetCampaignStateBatch for the stored campaign",
            "POST",
            "/v2/campaigns-state".to_string(),
            json!({"campaignIds": [campaign.clone()]}),
        ),
        (
            "UpdateCampaignSchedule with an ISO 8601 refresh frequency",
            "POST",
            format!("/v2/campaigns/{campaign}/schedule"),
            json!({"schedule": {
                "startTime": "2099-01-01T00:00:00Z",
                "endTime": "2099-01-02T00:00:00Z",
                "refreshFrequency": "PT1H",
            }}),
        ),
    ];

    for (case, method, path, body) in served {
        let (status, response) = call(&router, REGION, method, &path, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} {response}"
        );
    }
}
