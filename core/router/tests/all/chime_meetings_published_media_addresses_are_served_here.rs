// SPDX-License-Identifier: CC0-1.0

//! Where a Chime SDK meeting's `MediaPlacement` sends a client, in an emulator with the
//! Amazon Chime family assembled on one engine.
//!
//! RULE: an address a service publishes for a caller to DIAL names the `host[:port]` of an
//! endpoint THIS emulator answers on, so a client holding only the published value reaches
//! the deployment that minted the resource. `MediaPlacement` is "A set of endpoints used by
//! clients to connect to the media service group for an Amazon Chime SDK meeting"
//! (`smithy_query(query="shape",shape="MediaPlacement",
//! model_path="services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/\
//! smithy-model/com-amazonaws-chimesdkmeetings-chimemeetingssdkservice.json")`), so every
//! URL member of it is such an address: a value naming a host in someone else's domain — or
//! one carrying no port, which makes the effective port the scheme's default rather than the
//! one this emulator is reached on — sends the client, with the join token and credentials
//! minted here, off this emulator entirely.
//!
//! The suite lives in the cross-service target because the published address names no
//! service: it is a bare `host[:port]` of the one front door the whole emulator is served at,
//! and SIX services share the `chime` SigV4 signing name, the legacy Amazon Chime endpoint
//! binding part of the `/meetings` space the Chime SDK Meetings model owns. So the half only
//! an assembled emulator shows is that the client's NEXT call, addressed to the published
//! value rather than to any service's endpoint host, is still served by the meeting's own
//! service — neither refused as claimed by nobody nor as claimed by two. The projection
//! itself (each member's scheme and path, and the read that equals the create) is the
//! Meetings crate's own
//! `services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/tests/all/\
//! meeting_projection.rs`; this reads the routing half of the same rule.
//!
//! The rows span what the rule generalises over: the base this emulator is reachable at (the
//! unserved default, a loopback address with a port, an IPv6 literal with a port, a DNS name
//! with a port) and the meeting's media Region, including a GovCloud one — neither of which
//! any published address may depend on. Every member of the set is read on each row, so one
//! meeting's endpoints are asserted to name one address and not merely the signaling one.
//!
//! Nothing here opens a socket. The request a published address is exercised with travels
//! through this same router in process, so what the rows can show is which address was
//! published and who serves a call addressed to it — not whether a port is listening, which an
//! in-process dispatch answers whatever the host was.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The `host[:port]` of a published endpoint is read by every suite here that holds one, so the
// reading is this target's one helper rather than a copy per suite.
use crate::aoss_published_collection_endpoint_dialled_by_a_stock_client::authority_of;

type Chime =
    service_com_amazonaws_chime_ucbuzzconsoleservice::EmulatedService<engine::InMemoryEngine>;
type Identity =
    service_com_amazonaws_chimesdkidentity_chimeidentityservice::EmulatedService<
        engine::InMemoryEngine,
    >;
type Meetings =
    service_com_amazonaws_chimesdkmeetings_chimemeetingssdkservice::EmulatedService<
        engine::InMemoryEngine,
    >;

/// A SigV4 `Authorization` header whose credential scope names the signing name all six
/// Amazon Chime services share, which is what routes a request to the family. The signature
/// itself is never checked here.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTCHIME/20260101/us-east-1/chime/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// Every member of `MediaPlacement` that carries a whole URL, from the model's own member
/// list (`smithy_query(query="shape",shape="MediaPlacement")`). `AudioHostUrl` is not here:
/// it is the one member that is a bare `host:port` and is read on its own below.
const ANNOUNCED_URL_MEMBERS: &[&str] = &[
    "AudioFallbackUrl",
    "EventIngestionUrl",
    "ScreenDataUrl",
    "ScreenSharingUrl",
    "ScreenViewingUrl",
    "SignalingUrl",
    "TurnControlUrl",
];

/// Domains no address this emulator publishes may name: a value under one of them leaves this
/// machine, because nothing here answers on a host of Amazon's own.
const FOREIGN_DOMAINS: &[&str] = &["chime.aws", "amazonaws.com", "api.aws"];

/// The schemes a published member is dialled over: a WebSocket session and an HTTPS request,
/// both on the TLS endpoint this emulator serves.
const DIALLED_SCHEMES: &[&str] = &["wss", "https"];

/// The Amazon Chime family members whose agreement this rule needs, mounted over ONE engine
/// as `core/router/src/lib.rs` mounts them, on an emulator reachable at `base`: `base` is what
/// `OperationContext::endpoint` answers, and so the address the Meetings service publishes for
/// a meeting's media endpoints.
///
/// The legacy Amazon Chime endpoint and Chime SDK Identity are mounted beside Meetings
/// because all three claim on the one `chime` signing name — the first binding part of the
/// `/meetings` space, the second the family's declared owner of the shared `/tags` space — so
/// a request addressed to a bare `host:port` that named no endpoint of its own could be
/// claimed by more than one of them.
///
/// Request authentication is off, as in the sibling suites here; the credential scope is what
/// routes a request.
fn emulator_at(base: &str) -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.set_url_base(
        smithy_server_runtime::EndpointUrl::parse(base)
            .unwrap_or_else(|| panic!("{base:?} is a usable emulator base URL")),
    );
    router.register_service(Chime::new(Arc::clone(&engine)));
    router.register_service(Identity::new(Arc::clone(&engine)));
    router.register_service(Meetings::new(engine));
    router
}

/// Send one signed request addressed to `authority` — the host a client has, which is all a
/// client holding a published address has — and read its status and JSON body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    authority: &str,
    path_and_query: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Vec::new, |value| {
        serde_json::to_vec(value).expect("a request body serializes")
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{authority}{path_and_query}"))
        .header("host", authority)
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A string member of a JSON record, naming what was read when it is not there.
fn string_member(case: &str, record: &Value, member: &str) -> String {
    record
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: member {member} is not a string in {record}"))
        .to_string()
}

/// An absolute URL as its scheme, its authority (`host[:port]`, an IPv6 literal keeping the
/// brackets that hold its own colons) and everything a client sends after it — the path with
/// its query string, which a published member carries as its own.
///
/// The authority alone is this target's shared [`authority_of`], which reads the `https` URL a
/// resource publishes; a `MediaPlacement` member carries its own scheme and its own path, so
/// the split those two parts need is what this adds.
fn split_url(case: &str, url: &str) -> (String, String, String) {
    let (scheme, rest) = url
        .split_once("://")
        .unwrap_or_else(|| panic!("case {case:?}: expected an absolute URL, got {url:?}"));
    let (authority, path_and_query) = match rest.find('/') {
        Some(end) => (&rest[..end], &rest[end..]),
        None => (rest, "/"),
    };
    (
        scheme.to_string(),
        authority.to_string(),
        path_and_query.to_string(),
    )
}

/// The port an authority names explicitly, or `None` where it names none and the effective
/// port is therefore whatever the scheme defaults to.
fn explicit_port(authority: &str) -> Option<u16> {
    authority
        .rsplit(':')
        .next()
        .and_then(|port| port.parse::<u16>().ok())
}

/// The RULE over one whole `MediaPlacement`: every member names `authority` — the address this
/// emulator answers on — keeps a scheme a client dials, and names no host belonging to
/// somebody else.
fn assert_published_on(case: &str, placement: &Value, authority: &str) {
    // The audio host is the one member that is the address itself, with no scheme and no path.
    assert_eq!(
        string_member(case, placement, "AudioHostUrl"),
        authority,
        "case {case:?}: expected AudioHostUrl to be the address this emulator answers on \
         ({authority:?}), got {placement}"
    );
    for member in ANNOUNCED_URL_MEMBERS {
        let url = string_member(case, placement, member);
        let (scheme, host, _) = split_url(case, &url);
        assert!(
            DIALLED_SCHEMES.contains(&scheme.as_str()),
            "case {case:?}: expected {member} to be dialled over one of {DIALLED_SCHEMES:?}, \
             got {url:?}"
        );
        assert_eq!(
            host, authority,
            "case {case:?}: expected {member} to name the address this emulator answers on \
             ({authority:?}), got host {host:?} in {url:?}"
        );
        for domain in FOREIGN_DOMAINS {
            assert!(
                !host.ends_with(domain),
                "case {case:?}: expected {member} to send no client to a {domain} host, got \
                 {url:?}"
            );
        }
    }
}

/// One row: the base this emulator is reachable at and the Region the meeting's media is
/// placed in.
struct Row {
    case: &'static str,
    base: &'static str,
    media_region: &'static str,
}

/// RULE: the media addresses a meeting publishes name the `host[:port]` this emulator is
/// reached on, carrying that port explicitly, and the client's next call — addressed to the
/// published value and to nothing else — is served the same meeting by the assembled
/// emulator.
#[tokio::test]
async fn a_client_holding_only_the_published_address_is_served_the_meeting_it_names() {
    // Regions from `MediaRegion`'s own documented list, including a GovCloud one, since the
    // media Region is what a production host template would interpolate.
    let rows = [
        Row {
            case: "an emulator served on no port at all",
            base: smithy_server_runtime::UNSERVED_URL_BASE,
            media_region: "us-east-1",
        },
        Row {
            case: "an emulator served on a loopback port",
            base: "https://127.0.0.1:39631",
            media_region: "eu-central-1",
        },
        Row {
            case: "an emulator served on an IPv6 port",
            base: "https://[::1]:9443",
            media_region: "us-gov-west-1",
        },
        Row {
            case: "an emulator served at a name with a port",
            base: "https://chime-emulator.test.invalid:9297",
            media_region: "ap-southeast-2",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let Row {
            case,
            base,
            media_region,
        } = *row;
        let router = emulator_at(base);
        let own_authority = authority_of(base);

        // The client addresses the emulator's own address, which names no service: exactly
        // what a client configured with an emulator endpoint sends.
        let (status, created) = call(
            &router,
            "POST",
            &own_authority,
            "/meetings",
            Some(&json!({
                "ClientRequestToken": format!("tok-published-{index}"),
                "MediaRegion": media_region,
                "ExternalMeetingId": format!("ext-published-{index}"),
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateMeeting to answer 200, got {status} with {created}"
        );

        let meeting = created["Meeting"].clone();
        let meeting_id = string_member(case, &meeting, "MeetingId");
        let placement = meeting["MediaPlacement"].clone();
        assert_published_on(case, &placement, &own_authority);

        // The signaling address in full: the scheme a client opens the session over, this
        // emulator's own authority, and this meeting's own control path under it.
        let signaling = string_member(case, &placement, "SignalingUrl");
        let (_, published_authority, published_path) = split_url(case, &signaling);
        let control_path = format!("/control/{meeting_id}");
        assert_eq!(
            (published_authority.as_str(), published_path.as_str()),
            (own_authority.as_str(), control_path.as_str()),
            "case {case:?}: expected the signaling address to be this emulator's own, on this \
             meeting's control path, got {signaling:?}"
        );
        // Whatever port this emulator is reached on is the port the published value carries:
        // never dropped, which would leave the client dialling the scheme's default instead.
        assert_eq!(
            explicit_port(&published_authority),
            explicit_port(&own_authority),
            "case {case:?}: expected the published address to carry the port this emulator is \
             reached on ({own_authority:?}), got {signaling:?}"
        );

        // The next call goes where the meeting said — the authority read back OUT of the
        // published value — and the assembled emulator serves it the same meeting.
        let (status, read) = call(
            &router,
            "GET",
            &published_authority,
            &format!("/meetings/{meeting_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the address the meeting published to serve GetMeeting, \
             got {status} with {read}"
        );
        assert_eq!(
            read["Meeting"]["MediaPlacement"], placement,
            "case {case:?}: expected the published address to report the endpoints the create \
             reported, got {read}"
        );
    }
}
