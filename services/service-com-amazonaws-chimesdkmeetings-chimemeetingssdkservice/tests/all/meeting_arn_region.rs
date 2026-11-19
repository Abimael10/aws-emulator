// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the Region a meeting's `MeetingArn` names.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `chime` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: `Meeting$MeetingArn` names the CONTROL-PLANE resource, so its
//! Region segment is the Region of the endpoint the call was signed for, with the caller's
//! account id and the minted `MeetingId` after it — a string a client can rebuild from its
//! own Region and account. `MediaRegion`, documented as "The Region in which you create the
//! meeting", is an independent request parameter: it is reported as supplied and is part of
//! no value this service derives — the meeting's media endpoints (`MediaPlacement`) name
//! the address the caller reached this emulator at, not a Region's host. The ARN is also
//! the key the generic tagging routes address the meeting by, so the string a client builds
//! addresses the meeting it created.
//!
//! The table spans what the rule generalises over: the endpoint Region the request is
//! signed for, the `MediaRegion` the request asks for (equal to the endpoint Region,
//! different from it, in another partition), and which of the two creates minted the
//! meeting.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_chimesdkmeetings_chimemeetingssdkservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`engine::emulator_account_id`).
const ACCOUNT_ID: &str = "000000000000";

/// Every member of the modelled `MediaPlacement` shape that carries an address
/// (`smithy_query(query="shape",shape="MediaPlacement")`).
const MEDIA_PLACEMENT_MEMBERS: &[&str] = &[
    "AudioHostUrl",
    "AudioFallbackUrl",
    "SignalingUrl",
    "TurnControlUrl",
    "ScreenDataUrl",
    "ScreenSharingUrl",
    "ScreenViewingUrl",
    "EventIngestionUrl",
];

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    body: Value,
}

/// The emulator under test: ONE shared engine with this service mounted on it, reached the
/// way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Dispatch one request SIGNED FOR `endpoint_region`: the credential scope's Region is
    /// what the service reads as the endpoint Region, and the host is that Region's own
    /// endpoint for this service.
    async fn call(
        &self,
        endpoint_region: &str,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let host = format!("meetings-chime.{endpoint_region}.amazonaws.com");
        let authorization = format!(
            "AWS4-HMAC-SHA256 \
             Credential=AKIDTESTCHIME/20260101/{endpoint_region}/chime/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=0000"
        );
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host.as_str())
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply { status, body }
    }
}

/// A string member of a record.
fn string_member(record: &Value, member: &str) -> String {
    record
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("member `{member}` is not a string in {record:?}"))
        .to_string()
}

/// One row: the Region the request is signed for and the media Region it asks for.
struct Case {
    case: &'static str,
    endpoint_region: &'static str,
    media_region: &'static str,
}

/// The rows, drawn from the Regions `MediaRegion`'s own documentation lists (including a
/// GovCloud one), paired so that the two Regions are sometimes equal and sometimes not.
///
/// What each row pins is the Region SEGMENT of the ARN. The PARTITION segment is whatever
/// `engine::emulator_arn` mints for every emulated ARN of this workspace — `aws` — which
/// this suite does not vary and makes no claim about: real AWS would mint a GovCloud ARN in
/// the `aws-us-gov` partition, and the GovCloud rows are here for the Region label, not for
/// the partition.
fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "the media Region differs from the endpoint Region",
            endpoint_region: "us-west-2",
            media_region: "us-east-1",
        },
        Case {
            case: "the two Regions are the same",
            endpoint_region: "eu-west-2",
            media_region: "eu-west-2",
        },
        Case {
            case: "a media Region on another continent",
            endpoint_region: "us-east-1",
            media_region: "ap-southeast-2",
        },
        Case {
            case: "a GovCloud endpoint Region",
            endpoint_region: "us-gov-west-1",
            media_region: "sa-east-1",
        },
        Case {
            case: "a GovCloud media Region",
            endpoint_region: "ca-central-1",
            media_region: "us-gov-west-1",
        },
    ]
}

/// RULE: the ARN's Region segment is the endpoint Region; `MediaRegion` is reported as
/// supplied and scopes nothing else. Pinned for both creates, read back through
/// `GetMeeting`, and used as the key of a tagging round trip.
#[tokio::test]
async fn a_meetings_arn_names_the_endpoint_region_and_its_media_region_scopes_nothing() {
    let emulator = Emulator::new();

    for (index, row) in cases().into_iter().enumerate() {
        let Case {
            case,
            endpoint_region,
            media_region,
        } = row;

        // Both creates that mint a meeting, so the ARN cannot be derived two ways.
        let creates: Vec<(&str, &str, Value)> = vec![
            (
                "CreateMeeting",
                "/meetings",
                json!({
                    "ClientRequestToken": format!("tok-arn-{index}"),
                    "MediaRegion": media_region,
                    "ExternalMeetingId": format!("ext-arn-{index}"),
                }),
            ),
            (
                "CreateMeetingWithAttendees",
                "/meetings?operation=create-attendees",
                json!({
                    "ClientRequestToken": format!("tok-arn-{index}-a"),
                    "MediaRegion": media_region,
                    "ExternalMeetingId": format!("ext-arn-{index}-a"),
                    "Attendees": [{ "ExternalUserId": "user-arn" }],
                }),
            ),
        ];

        for (operation, path, request) in creates {
            let reply = emulator
                .call(endpoint_region, "POST", path, Some(&request))
                .await;
            assert_eq!(
                reply.status, 200,
                "case {case:?} ({operation}): expected 200, got {} with {:?}",
                reply.status, reply.body
            );
            let meeting = reply.body["Meeting"].clone();
            let meeting_id = string_member(&meeting, "MeetingId");

            // The ARN a client rebuilds from its OWN Region and account.
            let expected = format!("arn:aws:chime:{endpoint_region}:{ACCOUNT_ID}:meeting/{meeting_id}");
            assert_eq!(
                string_member(&meeting, "MeetingArn"),
                expected,
                "case {case:?} ({operation}): expected the ARN to name the endpoint Region, \
                 got {:?}",
                meeting["MeetingArn"]
            );
            // `MediaRegion` is reported as supplied, and neither Region reaches a media
            // endpoint: every one of them names the address this caller reached the
            // emulator at, which `meeting_projection.rs` pins in full.
            assert_eq!(
                string_member(&meeting, "MediaRegion"),
                media_region,
                "case {case:?} ({operation}): expected MediaRegion as supplied, got {:?}",
                meeting["MediaRegion"]
            );
            let placement = meeting["MediaPlacement"].clone();
            for member in MEDIA_PLACEMENT_MEMBERS {
                let url = string_member(&placement, member);
                assert!(
                    !url.contains(media_region) && !url.contains(endpoint_region),
                    "case {case:?} ({operation}): expected MediaPlacement.{member} to name \
                     no Region's host, got {url:?}"
                );
            }

            // The round trip: `GetMeeting` reads back the same ARN, and reading it twice
            // does not change it.
            for attempt in 1..=2u8 {
                let read = emulator
                    .call(endpoint_region, "GET", &format!("/meetings/{meeting_id}"), None)
                    .await;
                assert_eq!(
                    (read.status, read.body["Meeting"]["MeetingArn"].as_str()),
                    (200, Some(expected.as_str())),
                    "case {case:?} ({operation}, read {attempt}): expected GetMeeting to read \
                     back the same ARN, got {} with {:?}",
                    read.status,
                    read.body
                );
            }

            // The ARN is the key the tagging routes address the meeting by: a client that
            // BUILT this string reaches the meeting it created.
            let tagged = emulator
                .call(
                    endpoint_region,
                    "POST",
                    "/tags?operation=tag-resource",
                    Some(&json!({
                        "ResourceARN": expected,
                        "Tags": [{ "Key": "team", "Value": "collab" }],
                    })),
                )
                .await;
            assert_eq!(
                tagged.status, 204,
                "case {case:?} ({operation}): expected TagResource on the client-built ARN \
                 to answer 204, got {} with {:?}",
                tagged.status, tagged.body
            );
            let listed = emulator
                .call(
                    endpoint_region,
                    "GET",
                    &format!("/tags?arn={}", crate::resource_tagging::encode(&expected)),
                    None,
                )
                .await;
            assert_eq!(
                (listed.status, listed.body["Tags"].clone()),
                (200, json!([{ "Key": "team", "Value": "collab" }])),
                "case {case:?} ({operation}): expected the tag written under the client-built \
                 ARN to be listed, got {} with {:?}",
                listed.status,
                listed.body
            );
        }
    }
}
