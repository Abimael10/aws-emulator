// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of HOW a capability/meeting-feature conflict is WORDED.
//!
//! The rule under test: each of the three documented capability corner cases is refused
//! with the one fixed sentence the service serves for that error class — the same text
//! whichever capability value broke the rule, whichever meeting and attendee the request
//! named, and whichever operation supplied the set. Nothing of the request is spliced into
//! it, so the sentence a client matches on never varies per request.
//!
//! The table spans the class the rule generalises over: the whole cartesian product of the
//! modelled `MediaCapabilities` enum over the three capability members, four meeting
//! feature configurations that move which sets are admissible (the documented defaults, a
//! `Video:MaxResolution:None` meeting, a `Content:MaxResolution:None` meeting and one with
//! both), and every operation that judges a request-supplied set — `CreateAttendee`,
//! `UpdateAttendeeCapabilities`, `BatchUpdateAttendeeCapabilitiesExcept` and
//! `CreateMeetingWithAttendees`, the last of which reports the same refusal per item.
//!
//! The harness, the three sentences and the documented expectation [`expected_refusal`]
//! are shared with the sibling suite that spans the whole catalog of modelled maximum
//! resolutions, so the expected wording lives in exactly one place.
//!
//! The last suite below pins WHEN that judgement happens: a set the meeting does not admit
//! is refused for every request that carries one, so a create for an identity the meeting
//! already holds an attendee for is refused too — the same answer through each of the three
//! creation paths — while an admissible set is answered with the stored attendee and is
//! never applied to it.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_chimesdkmeetings_chimemeetingssdkservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header whose credential scope names the `chime` signing name
/// this service publishes (`OPERATIONS.md`, `## Wire protocol`). The scope is how the
/// router picks the claiming service; the signature itself is never checked here, since
/// the harness turns request authentication off.
const SIGNED_FOR_CHIME: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTCHIME/20260101/us-east-1/chime/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The values of the model's `MediaCapabilities` enum
/// (`smithy_query(query="shape",shape="MediaCapabilities",
/// model_path="services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/smithy-model/com-amazonaws-chimesdkmeetings-chimemeetingssdkservice.json")`
/// -> `SendReceive | Send | Receive | None`).
pub(crate) const MEDIA_CAPABILITIES: &[&str] = &["SendReceive", "Send", "Receive", "None"];

/// The refusal for a video capability asking to send or receive media in a meeting whose
/// video maximum resolution is `None`.
pub(crate) const VIDEO_MAX_RESOLUTION_NONE_MESSAGE: &str = "Attendee's video capability must be None \
     if MeetingFeatures is configured with Video:MaxResolution:None";

/// The same refusal for the content axis.
pub(crate) const CONTENT_MAX_RESOLUTION_NONE_MESSAGE: &str = "Attendee's content capability must be \
     None if MeetingFeatures is configured with Content:MaxResolution:None";

/// The refusal for content reception asked for without video reception.
pub(crate) const CONTENT_NEEDS_VIDEO_MESSAGE: &str = "Attendee's video capability must be set to \
     either SendReceive or Receive when setting content capability to SendReceive or \
     Receive.";

/// One response, reduced to what a client branches on.
#[derive(Debug)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) error_code: Option<String>,
    pub(crate) body: Value,
}

impl Reply {
    /// The refusal's `Message` member.
    pub(crate) fn message(&self) -> Option<String> {
        self.body
            .get("Message")
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// Send one request through the router and reduce the answer to a [`Reply`].
pub(crate) async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Reply {
    let payload = body.map_or_else(Vec::new, |value| {
        serde_json::to_vec(value).expect("a request body serializes")
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://meetings-chime.us-east-1.amazonaws.com{path}"))
        .header("authorization", SIGNED_FOR_CHIME)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    // restJson1 carries the wire error code in `x-amzn-errortype`; fall back to the body's
    // `__type` so a missing header never reads as "no error code".
    let error_code = header_code.or_else(|| {
        body.get("__type")
            .and_then(Value::as_str)
            .map(|wire| wire.rsplit('#').next().unwrap_or(wire).to_string())
    });
    Reply {
        status,
        error_code,
        body,
    }
}

/// The emulator under test: ONE shared engine with this service mounted on it, reached the
/// way the production router reaches it.
pub(crate) fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Create a meeting with `features` (when any) and answer its minted `MeetingId`.
pub(crate) async fn new_meeting(
    router: &MultiServiceRouter,
    token: &str,
    features: Option<Value>,
) -> String {
    let mut request = json!({
        "ClientRequestToken": token,
        "MediaRegion": "us-east-1",
        "ExternalMeetingId": format!("ext-{token}"),
    });
    if let Some(features) = features {
        request["MeetingFeatures"] = features;
    }
    let reply = call(router, "POST", "/meetings", Some(&request)).await;
    assert_eq!(
        reply.status, 200,
        "CreateMeeting {token:?}: expected 200, got {} with {:?}",
        reply.status, reply.body
    );
    reply
        .body
        .get("Meeting")
        .and_then(|meeting| meeting.get("MeetingId"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("CreateMeeting {token:?} returned no MeetingId"))
        .to_string()
}

/// Create an attendee of `meeting_id` with the meeting's default capabilities and answer
/// its minted `AttendeeId`.
pub(crate) async fn new_attendee(
    router: &MultiServiceRouter,
    meeting_id: &str,
    user: &str,
) -> String {
    let reply = call(
        router,
        "POST",
        &format!("/meetings/{meeting_id}/attendees"),
        Some(&json!({ "ExternalUserId": user })),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "CreateAttendee {user:?}: expected 200, got {} with {:?}",
        reply.status, reply.body
    );
    reply
        .body
        .get("Attendee")
        .and_then(|attendee| attendee.get("AttendeeId"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("CreateAttendee {user:?} returned no AttendeeId"))
        .to_string()
}

/// A capability set as the wire carries it.
pub(crate) fn capability_set(audio: &str, video: &str, content: &str) -> Value {
    json!({ "Audio": audio, "Video": video, "Content": content })
}

/// Whether a `MediaCapabilities` value asks to send or receive media at all.
fn sends_or_receives(capability: &str) -> bool {
    capability == "SendReceive" || capability == "Send" || capability == "Receive"
}

/// Whether a `MediaCapabilities` value asks to RECEIVE media.
fn receives(capability: &str) -> bool {
    capability == "SendReceive" || capability == "Receive"
}

/// The sentence a meeting whose effective maximum resolutions are `video_resolution` /
/// `content_resolution` refuses this set with, or `None` when it admits it.
///
/// The three rules are the corner cases the model's `AttendeeCapabilities` documentation
/// lists, restated here from that documentation so the table's expectation is independent
/// of the handler. No documented rule constrains `Audio`, so it is not a parameter: every
/// one of its four values is admissible in every meeting.
pub(crate) fn expected_refusal(
    video: &str,
    content: &str,
    video_resolution: &str,
    content_resolution: &str,
) -> Option<&'static str> {
    if video_resolution == "None" && sends_or_receives(video) {
        return Some(VIDEO_MAX_RESOLUTION_NONE_MESSAGE);
    }
    if content_resolution == "None" && sends_or_receives(content) {
        return Some(CONTENT_MAX_RESOLUTION_NONE_MESSAGE);
    }
    if receives(content) && !receives(video) {
        return Some(CONTENT_NEEDS_VIDEO_MESSAGE);
    }
    None
}

/// Ask `UpdateAttendeeCapabilities` to apply `capabilities` to one stored attendee.
pub(crate) async fn update_capabilities(
    router: &MultiServiceRouter,
    meeting_id: &str,
    attendee_id: &str,
    capabilities: &Value,
) -> Reply {
    call(
        router,
        "PUT",
        &format!("/meetings/{meeting_id}/attendees/{attendee_id}/capabilities"),
        Some(&json!({ "Capabilities": capabilities })),
    )
    .await
}

/// Ask `CreateAttendee` to mint an attendee holding `capabilities`.
pub(crate) async fn create_attendee_with(
    router: &MultiServiceRouter,
    meeting_id: &str,
    user: &str,
    capabilities: &Value,
) -> Reply {
    call(
        router,
        "POST",
        &format!("/meetings/{meeting_id}/attendees"),
        Some(&json!({ "ExternalUserId": user, "Capabilities": capabilities })),
    )
    .await
}

/// Ask `BatchUpdateAttendeeCapabilitiesExcept` to apply `capabilities` to the meeting.
async fn batch_update_except(
    router: &MultiServiceRouter,
    meeting_id: &str,
    excluded: &str,
    capabilities: &Value,
) -> Reply {
    call(
        router,
        "PUT",
        &format!("/meetings/{meeting_id}/attendees/capabilities?operation=batch-update-except"),
        Some(&json!({
            "ExcludedAttendeeIds": [{ "AttendeeId": excluded }],
            "Capabilities": capabilities,
        })),
    )
    .await
}

/// Read one attendee back.
pub(crate) async fn get_attendee(
    router: &MultiServiceRouter,
    meeting_id: &str,
    attendee_id: &str,
) -> Reply {
    call(
        router,
        "GET",
        &format!("/meetings/{meeting_id}/attendees/{attendee_id}"),
        None,
    )
    .await
}

/// (case, `MeetingFeatures`, the meeting's effective Video/Content `MaxResolution`). The
/// documented defaults are `HD` for video and `FHD` for content (`VideoFeatures`
/// /`ContentFeatures` `$MaxResolution`, "Defaults to …").
fn feature_configurations() -> Vec<(&'static str, Option<Value>, &'static str, &'static str)> {
    vec![
        ("default features", None, "HD", "FHD"),
        (
            "video maximum resolution None",
            Some(json!({ "Video": { "MaxResolution": "None" } })),
            "None",
            "FHD",
        ),
        (
            "content maximum resolution None",
            Some(json!({ "Content": { "MaxResolution": "None" } })),
            "HD",
            "None",
        ),
        (
            "both maximum resolutions None",
            Some(json!({
                "Video": { "MaxResolution": "None" },
                "Content": { "MaxResolution": "None" },
            })),
            "None",
            "None",
        ),
    ]
}

/// Every refused set is refused with the fixed sentence for ITS class — the same text for
/// every capability value that breaks the rule, for every meeting and attendee, and for
/// each of the three operations that take a whole set.
#[tokio::test]
async fn a_capability_conflict_is_refused_with_the_fixed_message_for_its_class() {
    let router = router();
    // How many distinct messages were served per class: the rule says one each.
    let mut served: BTreeSet<String> = BTreeSet::new();
    let mut refused = 0usize;
    let mut admitted = 0usize;

    for (index, (meeting_case, features, video_resolution, content_resolution)) in
        feature_configurations().into_iter().enumerate()
    {
        let meeting_id = new_meeting(&router, &format!("tok-msg-{index}"), features).await;
        let attendee_id = new_attendee(&router, &meeting_id, &format!("user-msg-{index}")).await;
        let excluded = new_attendee(&router, &meeting_id, &format!("kept-msg-{index}")).await;

        for audio in MEDIA_CAPABILITIES {
            for video in MEDIA_CAPABILITIES {
                for content in MEDIA_CAPABILITIES {
                    let requested = capability_set(audio, video, content);
                    let case =
                        format!("{meeting_case}, Audio {audio} Video {video} Content {content}");
                    let Some(expected) =
                        expected_refusal(video, content, video_resolution, content_resolution)
                    else {
                        admitted += 1;
                        let reply =
                            update_capabilities(&router, &meeting_id, &attendee_id, &requested)
                                .await;
                        assert_eq!(
                            (reply.status, reply.error_code.as_deref()),
                            (200, None),
                            "case {case:?}: an admitted set must not be refused, got {} {:?} \
                             with {:?}",
                            reply.status,
                            reply.error_code,
                            reply.body
                        );
                        continue;
                    };

                    refused += 1;
                    let attempts = vec![
                        (
                            "UpdateAttendeeCapabilities",
                            update_capabilities(&router, &meeting_id, &attendee_id, &requested)
                                .await,
                        ),
                        (
                            "CreateAttendee",
                            create_attendee_with(
                                &router,
                                &meeting_id,
                                &format!("user-{index}-{audio}-{video}-{content}"),
                                &requested,
                            )
                            .await,
                        ),
                        (
                            "BatchUpdateAttendeeCapabilitiesExcept",
                            batch_update_except(&router, &meeting_id, &excluded, &requested).await,
                        ),
                    ];

                    for (operation, reply) in attempts {
                        assert_eq!(
                            (reply.status, reply.error_code.as_deref()),
                            (400, Some("BadRequestException")),
                            "case {case:?} via {operation}: expected the modelled 400 \
                             BadRequestException, got {} {:?} with {:?}",
                            reply.status,
                            reply.error_code,
                            reply.body
                        );
                        assert_eq!(
                            reply.message().as_deref(),
                            Some(expected),
                            "case {case:?} via {operation}: expected message {expected:?}, \
                             got {:?}",
                            reply.message()
                        );
                        // Nothing of the request is in the sentence: not the ids it named,
                        // not the external user id it supplied.
                        let message = reply.message().unwrap_or_default();
                        for value in [meeting_id.as_str(), attendee_id.as_str(), "user-"] {
                            assert!(
                                !message.contains(value),
                                "case {case:?} via {operation}: the message must not carry the \
                                 request value {value:?}, got {message:?}"
                            );
                        }
                        served.insert(message);
                    }
                }
            }
        }
    }

    assert!(
        refused > 0 && admitted > 0,
        "the table must both refuse and admit sets, got {refused} refused and {admitted} admitted"
    );
    // One sentence per class and no others, over every value and every operation above.
    let expected_messages: BTreeSet<String> = [
        VIDEO_MAX_RESOLUTION_NONE_MESSAGE,
        CONTENT_MAX_RESOLUTION_NONE_MESSAGE,
        CONTENT_NEEDS_VIDEO_MESSAGE,
    ]
    .iter()
    .map(|message| (*message).to_string())
    .collect();
    assert_eq!(
        served, expected_messages,
        "the three classes must serve exactly their three sentences, got {served:?}"
    );
}

/// Ask `BatchCreateAttendee` to add `items` to a meeting this service already holds.
async fn batch_create_attendees(
    router: &MultiServiceRouter,
    meeting_id: &str,
    items: &Value,
) -> Reply {
    call(
        router,
        "POST",
        &format!("/meetings/{meeting_id}/attendees?operation=batch-create"),
        Some(&json!({ "Attendees": items })),
    )
    .await
}

/// Ask `CreateMeetingWithAttendees` for a meeting with `features` holding `items`.
async fn create_meeting_with_attendees(
    router: &MultiServiceRouter,
    token: &str,
    features: Option<Value>,
    items: &Value,
) -> Reply {
    let mut request = json!({
        "ClientRequestToken": token,
        "MediaRegion": "us-east-1",
        "ExternalMeetingId": format!("ext-{token}"),
        "Attendees": items,
    });
    if let Some(features) = features {
        request["MeetingFeatures"] = features;
    }
    call(router, "POST", "/meetings?operation=create-attendees", Some(&request)).await
}

/// The attendees of `meeting_id`, as `ListAttendees` reports them.
async fn list_attendees(router: &MultiServiceRouter, meeting_id: &str) -> Vec<Value> {
    let reply = call(
        router,
        "GET",
        &format!("/meetings/{meeting_id}/attendees?MaxResults=100"),
        None,
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "ListAttendees({meeting_id}): expected 200, got {} with {:?}",
        reply.status, reply.body
    );
    list_of(&reply, "Attendees")
}

/// A list member of a success body, empty when the body carries none.
fn list_of(reply: &Reply, member: &str) -> Vec<Value> {
    reply
        .body
        .get(member)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// One capability member of a set, as the wire carries it.
fn axis(set: &Value, member: &str) -> String {
    set.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("member `{member}` is not a string in {set:?}"))
        .to_string()
}

/// One row of the ordering table: the meeting's `MeetingFeatures`, its effective maximum
/// resolutions (so the expectation comes from the documentation and not from the handler),
/// and the capability set a REPEATED create for an identity the meeting already holds
/// carries.
struct RepeatCase {
    name: &'static str,
    features: Option<Value>,
    resolutions: (&'static str, &'static str),
    carried: Value,
}

/// A capability set is judged against the meeting's features for EVERY request that
/// carries one, so a create for an identity whose attendee the meeting already holds is
/// refused when the set is inadmissible — identically through `CreateAttendee`,
/// `BatchCreateAttendee` and `CreateMeetingWithAttendees` — and is answered with the stored
/// attendee, unchanged, when it is admissible.
///
/// The rows span what the rule generalises over: which of the three documented classes the
/// set breaks, which capability value breaks it, which meeting features make it
/// inadmissible, and sets the same meetings DO admit.
#[tokio::test]
async fn a_capability_set_is_judged_before_the_external_user_id_replay() {
    let cases = vec![
        RepeatCase {
            name: "a video capability a Video:MaxResolution:None meeting forbids",
            features: Some(json!({ "Video": { "MaxResolution": "None" } })),
            resolutions: ("None", "FHD"),
            carried: capability_set("SendReceive", "SendReceive", "None"),
        },
        RepeatCase {
            name: "a video capability asking only to receive, in the same meeting",
            features: Some(json!({ "Video": { "MaxResolution": "None" } })),
            resolutions: ("None", "FHD"),
            carried: capability_set("None", "Receive", "None"),
        },
        RepeatCase {
            name: "a content capability a Content:MaxResolution:None meeting forbids",
            features: Some(json!({ "Content": { "MaxResolution": "None" } })),
            resolutions: ("HD", "None"),
            carried: capability_set("Send", "SendReceive", "Send"),
        },
        RepeatCase {
            name: "content reception without video reception, under the default features",
            features: None,
            resolutions: ("HD", "FHD"),
            carried: capability_set("SendReceive", "None", "Receive"),
        },
        RepeatCase {
            name: "a set a meeting capping both axes at None forbids",
            features: Some(json!({
                "Video": { "MaxResolution": "None" },
                "Content": { "MaxResolution": "None" },
            })),
            resolutions: ("None", "None"),
            carried: capability_set("Send", "Send", "Send"),
        },
        RepeatCase {
            name: "a set the default features admit, which the replay must not apply",
            features: None,
            resolutions: ("HD", "FHD"),
            carried: capability_set("None", "Receive", "Receive"),
        },
        RepeatCase {
            name: "a set a Video:MaxResolution:None meeting admits",
            features: Some(json!({ "Video": { "MaxResolution": "None" } })),
            resolutions: ("None", "FHD"),
            carried: capability_set("Send", "None", "None"),
        },
    ];

    let router = router();
    let mut refused_rows = 0usize;
    let mut admitted_rows = 0usize;

    for (index, case) in cases.into_iter().enumerate() {
        let RepeatCase {
            name,
            features,
            resolutions,
            carried,
        } = case;
        let expected = expected_refusal(
            &axis(&carried, "Video"),
            &axis(&carried, "Content"),
            resolutions.0,
            resolutions.1,
        );
        let user = format!("user-order-{index}");

        // A meeting already holding exactly one attendee for this identity, created with
        // the meeting's own documented default capabilities.
        let meeting_id =
            new_meeting(&router, &format!("tok-order-{index}"), features.clone()).await;
        let attendee_id = new_attendee(&router, &meeting_id, &user).await;
        let stored = get_attendee(&router, &meeting_id, &attendee_id)
            .await
            .body
            .get("Attendee")
            .cloned()
            .unwrap_or_else(|| panic!("case {name:?}: GetAttendee returned no attendee"));

        // The single create for the identity the meeting already holds.
        let repeat = create_attendee_with(&router, &meeting_id, &user, &carried).await;
        match expected {
            Some(message) => {
                refused_rows += 1;
                assert_eq!(
                    (repeat.status, repeat.error_code.as_deref()),
                    (400, Some("BadRequestException")),
                    "case {name:?} via CreateAttendee: an inadmissible set must be refused \
                     with the modelled 400 BadRequestException however many attendees the \
                     meeting holds, got {} {:?} with {:?}",
                    repeat.status,
                    repeat.error_code,
                    repeat.body
                );
                assert_eq!(
                    repeat.message().as_deref(),
                    Some(message),
                    "case {name:?} via CreateAttendee: expected message {message:?}, got {:?}",
                    repeat.message()
                );
            }
            None => {
                admitted_rows += 1;
                assert_eq!(
                    (repeat.status, repeat.error_code.as_deref()),
                    (200, None),
                    "case {name:?} via CreateAttendee: an admissible set must replay the \
                     stored attendee, got {} {:?} with {:?}",
                    repeat.status,
                    repeat.error_code,
                    repeat.body
                );
                assert_eq!(
                    repeat.body.get("Attendee"),
                    Some(&stored),
                    "case {name:?} via CreateAttendee: expected the stored attendee \
                     {stored:?}, got {:?}",
                    repeat.body.get("Attendee")
                );
            }
        }

        // The same item through the batch, which must answer it the same way.
        let batch = batch_create_attendees(
            &router,
            &meeting_id,
            &json!([{ "ExternalUserId": user, "Capabilities": carried }]),
        )
        .await;
        assert_eq!(
            batch.status, 200,
            "case {name:?} via BatchCreateAttendee: a per-item refusal never fails the call, \
             got {} with {:?}",
            batch.status, batch.body
        );
        let batch_attendees = list_of(&batch, "Attendees");
        let batch_errors = list_of(&batch, "Errors");
        match expected {
            Some(message) => {
                assert_eq!(
                    (batch_attendees.len(), batch_errors.len()),
                    (0, 1),
                    "case {name:?} via BatchCreateAttendee: expected the item reported in \
                     Errors and nothing in Attendees, got {batch_attendees:?} and \
                     {batch_errors:?}"
                );
                assert_eq!(
                    (
                        batch_errors[0].get("ExternalUserId").and_then(Value::as_str),
                        batch_errors[0].get("ErrorCode").and_then(Value::as_str),
                        batch_errors[0].get("ErrorMessage").and_then(Value::as_str),
                    ),
                    (Some(user.as_str()), Some("BadRequest"), Some(message)),
                    "case {name:?} via BatchCreateAttendee: expected the identity, the \
                     refusal's code and {message:?}, got {:?}",
                    batch_errors[0]
                );
            }
            None => {
                assert_eq!(
                    (batch_attendees.as_slice(), batch_errors.len()),
                    (std::slice::from_ref(&stored), 0),
                    "case {name:?} via BatchCreateAttendee: an admissible set must replay \
                     the stored attendee {stored:?} and refuse nothing, got \
                     {batch_attendees:?} and {batch_errors:?}"
                );
            }
        }

        // The creation path that mints the meeting too: the identity is named TWICE in one
        // request, the second item carrying the row's set.
        let paired = create_meeting_with_attendees(
            &router,
            &format!("tok-order-pair-{index}"),
            features.clone(),
            &json!([
                { "ExternalUserId": user },
                { "ExternalUserId": user, "Capabilities": carried },
            ]),
        )
        .await;
        assert_eq!(
            paired.status, 200,
            "case {name:?} via CreateMeetingWithAttendees: expected 200, got {} with {:?}",
            paired.status, paired.body
        );
        let paired_attendees = list_of(&paired, "Attendees");
        let paired_errors = list_of(&paired, "Errors");
        match expected {
            Some(message) => {
                assert_eq!(
                    (paired_attendees.len(), paired_errors.len()),
                    (1, 1),
                    "case {name:?} via CreateMeetingWithAttendees: expected the first item \
                     created and the repeat refused, got {paired_attendees:?} and \
                     {paired_errors:?}"
                );
                assert_eq!(
                    (
                        paired_errors[0].get("ExternalUserId").and_then(Value::as_str),
                        paired_errors[0].get("ErrorCode").and_then(Value::as_str),
                        paired_errors[0].get("ErrorMessage").and_then(Value::as_str),
                    ),
                    (Some(user.as_str()), Some("BadRequest"), Some(message)),
                    "case {name:?} via CreateMeetingWithAttendees: expected the identity, \
                     the refusal's code and {message:?}, got {:?}",
                    paired_errors[0]
                );
            }
            None => {
                assert_eq!(
                    (paired_attendees.len(), paired_errors.len()),
                    (2, 0),
                    "case {name:?} via CreateMeetingWithAttendees: an admissible set must \
                     replay the item the first one minted and refuse nothing, got \
                     {paired_attendees:?} and {paired_errors:?}"
                );
                assert_eq!(
                    paired_attendees[0], paired_attendees[1],
                    "case {name:?} via CreateMeetingWithAttendees: one identity gets one \
                     attendee, reported for both items, got {:?} and {:?}",
                    paired_attendees[0], paired_attendees[1]
                );
            }
        }

        // None of the three calls applied the set it carried, and none minted a second
        // attendee for the identity: changing a stored set is `UpdateAttendeeCapabilities`.
        let after = get_attendee(&router, &meeting_id, &attendee_id).await;
        assert_eq!(
            after.body.get("Attendee"),
            Some(&stored),
            "case {name:?}: the stored attendee must still be {stored:?}, got {:?}",
            after.body.get("Attendee")
        );
        let listed = list_attendees(&router, &meeting_id).await;
        assert_eq!(
            listed.len(),
            1,
            "case {name:?}: the meeting must still hold exactly one attendee, got {listed:?}"
        );
    }

    assert!(
        refused_rows > 0 && admitted_rows > 0,
        "the table must span both outcomes, got {refused_rows} refused and {admitted_rows} \
         admitted rows"
    );
}

/// `CreateMeetingWithAttendees` reports the same refusal per ITEM, with the same sentence
/// and the same error code, and admits the items that break no rule.
///
/// The rows differ in which class the item breaks and in which capability value breaks it.
#[tokio::test]
async fn a_per_item_capability_conflict_carries_the_same_message() {
    let router = router();

    // (the item's capability set, the sentence it is refused with — `None` for an item the
    // meeting admits). The meeting below has both maximum resolutions `None`, so its
    // admissible sets are the ones asking for neither video nor content media.
    let rows: Vec<(Value, Option<&str>)> = vec![
        (
            capability_set("SendReceive", "None", "None"),
            None,
        ),
        (
            capability_set("Receive", "SendReceive", "None"),
            Some(VIDEO_MAX_RESOLUTION_NONE_MESSAGE),
        ),
        (
            capability_set("None", "Send", "None"),
            Some(VIDEO_MAX_RESOLUTION_NONE_MESSAGE),
        ),
        (
            capability_set("Send", "None", "Receive"),
            Some(CONTENT_MAX_RESOLUTION_NONE_MESSAGE),
        ),
        (
            capability_set("Send", "None", "Send"),
            Some(CONTENT_MAX_RESOLUTION_NONE_MESSAGE),
        ),
    ];

    let attendees: Vec<Value> = rows
        .iter()
        .enumerate()
        .map(|(index, (capabilities, _))| {
            json!({
                "ExternalUserId": format!("item-{index}"),
                "Capabilities": capabilities,
            })
        })
        .collect();

    let reply = call(
        &router,
        "POST",
        "/meetings?operation=create-attendees",
        Some(&json!({
            "ClientRequestToken": "tok-items",
            "MediaRegion": "us-east-1",
            "ExternalMeetingId": "ext-tok-items",
            "MeetingFeatures": {
                "Video": { "MaxResolution": "None" },
                "Content": { "MaxResolution": "None" },
            },
            "Attendees": attendees,
        })),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "CreateMeetingWithAttendees: expected 200, got {} with {:?}",
        reply.status, reply.body
    );

    let errors = reply
        .body
        .get("Errors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let created = reply
        .body
        .get("Attendees")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for (index, (capabilities, expected)) in rows.iter().enumerate() {
        let user = format!("item-{index}");
        let case = format!("item {user} with {capabilities}");
        match expected {
            Some(message) => {
                let reported = errors
                    .iter()
                    .find(|error| error.get("ExternalUserId").and_then(Value::as_str) == Some(&user))
                    .unwrap_or_else(|| panic!("case {case:?}: no per-item error was reported"));
                assert_eq!(
                    reported.get("ErrorCode").and_then(Value::as_str),
                    Some("BadRequest"),
                    "case {case:?}: expected the refusal's code, got {:?}",
                    reported.get("ErrorCode")
                );
                assert_eq!(
                    reported.get("ErrorMessage").and_then(Value::as_str),
                    Some(*message),
                    "case {case:?}: expected message {message:?}, got {:?}",
                    reported.get("ErrorMessage")
                );
            }
            None => {
                let admitted = created
                    .iter()
                    .find(|attendee| {
                        attendee.get("ExternalUserId").and_then(Value::as_str) == Some(&user)
                    })
                    .unwrap_or_else(|| panic!("case {case:?}: the item was not admitted"));
                assert_eq!(
                    admitted.get("Capabilities"),
                    Some(capabilities),
                    "case {case:?}: the admitted item must hold exactly its set, got {:?}",
                    admitted.get("Capabilities")
                );
                // Persisted, not just answered.
                let meeting_id = reply
                    .body
                    .get("Meeting")
                    .and_then(|meeting| meeting.get("MeetingId"))
                    .and_then(Value::as_str)
                    .expect("the answer carries the meeting");
                let attendee_id = admitted
                    .get("AttendeeId")
                    .and_then(Value::as_str)
                    .expect("an admitted attendee carries its id");
                let read_back = get_attendee(&router, meeting_id, attendee_id).await;
                assert_eq!(
                    read_back
                        .body
                        .get("Attendee")
                        .and_then(|attendee| attendee.get("Capabilities")),
                    Some(capabilities),
                    "case {case:?}: GetAttendee must read back the applied set, got {:?}",
                    read_back.body
                );
            }
        }
    }
}
