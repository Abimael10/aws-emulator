// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the `Meeting` projection every create mints and every
//! reader returns.
//!
//! Each case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `chime` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal return value.
//!
//! Three rules are under test, all about the projection rather than about one request:
//!
//! 1. Each `MediaPlacement` endpoint is the service's whole URL template, query string
//!    included, with only the meeting's id substituted in. For the screen-viewing endpoint
//!    that means the complete viewer connect query string — the two fixed viewer parameters
//!    followed by the meeting's call id — so a client that parses the URL finds every
//!    parameter the service always supplies.
//! 2. A list member of the projection is always present: `TenantIds` reports the ids the
//!    create named, and the EMPTY list when it named none, so a client that iterates the
//!    member never meets an absent one.
//! 3. Every one of those endpoints is an address THIS emulator answers on: each member
//!    names the `host[:port]` of the one endpoint this emulator serves its whole surface at,
//!    keeping its own scheme and its own path — never a host belonging to someone else, and
//!    never an address that depends on which host one request addressed. That rule has its
//!    own table of addressings further down.
//! 4. Each of those addresses is SERVED: a client that dials one is answered with an
//!    HTTP/1.1 status decided from this service's own state — never with nothing at all.
//!    That rule has its own table at the end of this file.
//!
//! The table spans the class rules 1 and 2 generalise over: the media Region, how many
//! tenant ids the create carries (none, one, three, and the modelled maximum of five), and
//! which of the two operations that mint a meeting was used — since both must project the
//! same record, and every row is read back through `GetMeeting`.

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

/// The modelled maximum length of `TenantIdList`
/// (`smithy_query(query="shape",shape="TenantIdList",
/// model_path="services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/smithy-model/com-amazonaws-chimesdkmeetings-chimemeetingssdkservice.json")`
/// -> `@length(min: 1, max: 5)`).
const MAX_TENANT_IDS: usize = 5;

/// The fixed viewer parameters the screen-viewing connect URL carries ahead of the
/// meeting's call id, as `name=value` pairs in the order the service emits them.
const VIEWER_PARAMETERS: &[(&str, &str)] = &[("passcode", "null"), ("viewer_uuid", "null")];

/// The query parameter the screen-viewing connect URL carries the meeting's own id in.
const CALL_ID_PARAMETER: &str = "X-BitHub-Call-Id";

/// Hosts no address this emulator announces may name: a value on one of these leaves the
/// sandbox, because nothing here answers on an Amazon production host.
const FOREIGN_DOMAINS: [&str; 3] = ["chime.aws", "amazonaws.com", "api.aws"];

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// A named member of a success body.
    fn member(&self, name: &str) -> Value {
        self.body
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("response has no `{name}` member: {self:?}"))
    }
}

/// The emulator under test: ONE shared engine with this service mounted on it, reached
/// the way the production router reaches it.
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

    /// The same emulator, SERVED at `base` — the base URL `OperationContext::endpoint`
    /// reports, which is the data port's own URL for a router served on a port and
    /// `UNSERVED_URL_BASE` for one that is not (what [`Self::new`] leaves it as).
    fn served_at(base: &str) -> Self {
        let mut emulator = Self::new();
        emulator.router.set_url_base(
            smithy_server_runtime::EndpointUrl::parse(base)
                .expect("the test endpoint base is a bare https URL"),
        );
        emulator
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        self.call_as(method, path, body, None).await
    }

    /// Dispatch one request from a client that ADDRESSED `host`; `None` for a client that
    /// names no host of its own, where the request URI carries this service's published
    /// endpoint — which is what an SDK sends.
    async fn call_as(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        host: Option<&str>,
    ) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://meetings-chime.us-east-1.amazonaws.com{path}"))
            .header("authorization", SIGNED_FOR_CHIME)
            .header("content-type", "application/json");
        if let Some(host) = host {
            builder = builder.header("host", host);
        }
        let request = builder
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

    async fn create_meeting(&self, request: &Value) -> Reply {
        self.call("POST", "/meetings", Some(request)).await
    }

    async fn create_meeting_with_attendees(&self, request: &Value) -> Reply {
        self.call("POST", "/meetings?operation=create-attendees", Some(request))
            .await
    }

    async fn get_meeting(&self, meeting_id: &str) -> Reply {
        self.call("GET", &format!("/meetings/{meeting_id}"), None)
            .await
    }

    async fn get_meeting_as(&self, meeting_id: &str, host: Option<&str>) -> Reply {
        self.call_as("GET", &format!("/meetings/{meeting_id}"), None, host)
            .await
    }
}

/// Which of the two operations that mint a meeting a row goes through.
#[derive(Debug, Clone, Copy)]
enum Minter {
    CreateMeeting,
    CreateMeetingWithAttendees,
}

/// One row: how the meeting is created, in which Region, and the tenant ids the request
/// carries (`None` = the member is absent from the request).
#[derive(Debug)]
struct Case {
    name: &'static str,
    minter: Minter,
    region: &'static str,
    tenant_ids: Option<Vec<&'static str>>,
}

fn string_member(record: &Value, member: &str) -> String {
    record
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("member `{member}` is not a string in {record:?}"))
        .to_string()
}

/// The query string of `url`, split into its `name=value` pairs in wire order.
fn query_pairs(url: &str) -> Vec<(String, String)> {
    let Some((_, query)) = url.split_once('?') else {
        return Vec::new();
    };
    query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) => (name.to_string(), value.to_string()),
            None => (pair.to_string(), String::new()),
        })
        .collect()
}

/// Both projection rules hold for every meeting this service mints, whichever operation
/// minted it, and the record a reader returns is the record the create returned.
#[tokio::test]
async fn a_minted_meeting_projects_a_full_connect_url_and_a_tenant_id_list() {
    let five: Vec<&str> = vec![
        "tenant-one",
        "tenant-two",
        "tenant-three",
        "tenant-four",
        "tenant-five",
    ];
    assert_eq!(
        five.len(),
        MAX_TENANT_IDS,
        "the widest row must carry the modelled maximum of {MAX_TENANT_IDS} tenant ids"
    );

    // Regions drawn from `MediaRegion`'s own documented list, including a GovCloud one,
    // since the Region is what fills the host's region label.
    let cases: Vec<Case> = vec![
        Case {
            name: "no tenant ids at all",
            minter: Minter::CreateMeeting,
            region: "us-west-2",
            tenant_ids: None,
        },
        Case {
            name: "no tenant ids, in another Region",
            minter: Minter::CreateMeeting,
            region: "eu-central-1",
            tenant_ids: None,
        },
        Case {
            name: "one tenant id",
            minter: Minter::CreateMeeting,
            region: "ap-northeast-1",
            tenant_ids: Some(vec!["tenant-only"]),
        },
        Case {
            name: "three tenant ids",
            minter: Minter::CreateMeeting,
            region: "sa-east-1",
            tenant_ids: Some(vec!["tenant-a", "tenant-b", "tenant-c"]),
        },
        Case {
            name: "the modelled maximum of five tenant ids",
            minter: Minter::CreateMeeting,
            region: "us-gov-west-1",
            tenant_ids: Some(five.clone()),
        },
        Case {
            name: "no tenant ids, minted with attendees",
            minter: Minter::CreateMeetingWithAttendees,
            region: "ca-central-1",
            tenant_ids: None,
        },
        Case {
            name: "two tenant ids, minted with attendees",
            minter: Minter::CreateMeetingWithAttendees,
            region: "eu-west-3",
            tenant_ids: Some(vec!["tenant-x", "tenant-y"]),
        },
    ];

    let emulator = Emulator::new();

    for (index, case) in cases.iter().enumerate() {
        let name = case.name;
        let mut request = json!({
            "ClientRequestToken": format!("tok-projection-{index}"),
            "MediaRegion": case.region,
            "ExternalMeetingId": format!("ext-projection-{index}"),
        });
        if let Some(ids) = &case.tenant_ids {
            request["TenantIds"] = json!(ids);
        }
        let reply = match case.minter {
            Minter::CreateMeeting => emulator.create_meeting(&request).await,
            Minter::CreateMeetingWithAttendees => {
                request["Attendees"] = json!([{ "ExternalUserId": format!("user-{index}") }]);
                emulator.create_meeting_with_attendees(&request).await
            }
        };
        assert_eq!(
            reply.status, 200,
            "case {name:?}: expected HTTP 200, got {} with {:?}",
            reply.status, reply.body
        );

        let meeting = reply.member("Meeting");
        let meeting_id = string_member(&meeting, "MeetingId");

        // Rule 2: the member is always there — the ids asked for, or the empty list.
        let expected_tenant_ids: Value = case
            .tenant_ids
            .as_ref()
            .map_or_else(|| json!([]), |ids| json!(ids));
        assert_eq!(
            meeting.get("TenantIds"),
            Some(&expected_tenant_ids),
            "case {name:?}: expected TenantIds {expected_tenant_ids:?}, got {:?}",
            meeting.get("TenantIds")
        );

        // Rule 1: the screen-viewing endpoint is the whole connect URL — the fixed viewer
        // parameters, then this meeting's call id — on this meeting's own path.
        let placement = meeting
            .get("MediaPlacement")
            .cloned()
            .unwrap_or_else(|| panic!("case {name:?}: no MediaPlacement in {meeting:?}"));
        let screen_viewing_url = string_member(&placement, "ScreenViewingUrl");
        let (endpoint, _) = screen_viewing_url
            .split_once('?')
            .unwrap_or_else(|| panic!("case {name:?}: no query string in {screen_viewing_url:?}"));
        assert!(
            endpoint.ends_with("/ws/connect"),
            "case {name:?}: expected the viewer connect endpoint, got {endpoint:?}"
        );
        let mut expected_pairs: Vec<(String, String)> = VIEWER_PARAMETERS
            .iter()
            .map(|(parameter, value)| ((*parameter).to_string(), (*value).to_string()))
            .collect();
        expected_pairs.push((CALL_ID_PARAMETER.to_string(), meeting_id.clone()));
        assert_eq!(
            query_pairs(&screen_viewing_url),
            expected_pairs,
            "case {name:?}: expected the full viewer connect query string, got \
             {screen_viewing_url:?}"
        );

        // The round trip: a reader returns the record the create minted, so both rules
        // hold for every later read of the same meeting.
        let read = emulator.get_meeting(&meeting_id).await;
        assert_eq!(
            read.status, 200,
            "case {name:?}: GetMeeting expected 200, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.member("Meeting"),
            meeting,
            "case {name:?}: GetMeeting must report what the create returned, got {:?}",
            read.member("Meeting")
        );
    }
}

/// `TenantIds` is serialized as a list on the WIRE — the body a client parses carries
/// `[]`, not a null and not a missing member — for a create that named no tenant ids, and
/// for the read that follows it.
#[tokio::test]
async fn an_absent_tenant_id_list_reaches_the_wire_as_an_empty_array() {
    let emulator = Emulator::new();
    let request = json!({
        "ClientRequestToken": "tok-wire-tenants",
        "MediaRegion": "us-east-1",
        "ExternalMeetingId": "ext-wire-tenants",
    });

    let created = emulator.create_meeting(&request).await;
    assert_eq!(
        created.status, 200,
        "CreateMeeting expected 200, got {} with {:?}",
        created.status, created.body
    );
    let meeting_id = string_member(&created.member("Meeting"), "MeetingId");
    let read = emulator.get_meeting(&meeting_id).await;

    for (case, body) in [("CreateMeeting", &created.body), ("GetMeeting", &read.body)] {
        let rendered = serde_json::to_string(body).expect("a response body serializes");
        assert!(
            rendered.contains("\"TenantIds\":[]"),
            "case {case:?}: expected the body to carry TenantIds as an empty array, got \
             {rendered:?}"
        );
    }
}

/// One addressing a client reaches this emulator through: the base this emulator is served
/// at, the `Host` the client sent, and the `host[:port]` every endpoint announced to that
/// client must name.
struct Addressing {
    case: &'static str,
    /// The base `OperationContext::endpoint` reports, or `None` for an emulator served on
    /// no port at all (the harness default).
    served_at: Option<&'static str>,
    /// The host the client addressed, or `None` for a client that names only this service's
    /// published endpoint on the request URI.
    host: Option<&'static str>,
    authority: &'static str,
}

/// The addressings rule 3 is asserted over: rows differ along what the rule generalises
/// over — the address this emulator is served at (none at all, a loopback address with a
/// port, an IPv6 literal with a port, a name with a port) and the host the client addressed
/// (a loopback address, a name, an IPv6 literal, one of AWS's own hostnames, none at all),
/// which no announced address depends on.
fn addressings() -> Vec<Addressing> {
    vec![
        Addressing {
            case: "an SDK client naming this service's endpoint, on an unserved emulator",
            served_at: None,
            host: None,
            // `smithy_server_runtime::UNSERVED_URL_BASE`: the one base an emulator served
            // on no port reports, and still this emulator's own.
            authority: "emulator.invalid",
        },
        Addressing {
            case: "a client on the loopback address of an unserved emulator",
            served_at: None,
            host: Some("127.0.0.1:9297"),
            authority: "emulator.invalid",
        },
        Addressing {
            case: "an SDK client on an emulator served on a loopback port",
            served_at: Some("https://127.0.0.1:40305"),
            host: None,
            authority: "127.0.0.1:40305",
        },
        Addressing {
            case: "a client that resolves AWS's own hosts, on an emulator served on a port",
            served_at: Some("https://127.0.0.1:40305"),
            host: Some("meetings-chime.us-east-1.amazonaws.com"),
            authority: "127.0.0.1:40305",
        },
        Addressing {
            case: "a client naming the loopback by name, on an emulator served elsewhere",
            served_at: Some("https://127.0.0.1:40305"),
            host: Some("localhost:9297"),
            authority: "127.0.0.1:40305",
        },
        Addressing {
            case: "a client on an IPv6 literal, on an emulator served on an IPv6 port",
            served_at: Some("https://[::1]:9443"),
            host: Some("[::1]:9443"),
            authority: "[::1]:9443",
        },
        Addressing {
            case: "an SDK client on an emulator served at a name",
            served_at: Some("https://emulator.test:8443"),
            host: None,
            authority: "emulator.test:8443",
        },
    ]
}

/// Every `MediaPlacement` member that carries a URL
/// (`smithy_query(query="shape",shape="MediaPlacement")`), with the scheme a client parses
/// out of it and the path that endpoint is dialed at under the host — the service's own path
/// for that endpoint, which carries the `MeetingId` where the endpoint belongs to one
/// meeting. `AudioHostUrl` is not here: it is the one member that is a bare `host:port` and
/// is asserted on its own.
fn announced_urls(meeting_id: &str) -> Vec<(&'static str, &'static str, String)> {
    vec![
        ("AudioFallbackUrl", "wss", format!("/calls/{meeting_id}")),
        ("SignalingUrl", "wss", format!("/control/{meeting_id}")),
        ("TurnControlUrl", "https", "/v2/turn_sessions".to_string()),
        ("ScreenDataUrl", "wss", format!("/v2/screen/{meeting_id}")),
        ("ScreenSharingUrl", "wss", format!("/v2/screen/{meeting_id}")),
        ("ScreenViewingUrl", "wss", "/ws/connect".to_string()),
        ("EventIngestionUrl", "https", "/v1/client-events".to_string()),
    ]
}

/// The authority (`host[:port]`) of an absolute URL and the path under it, with any query
/// string dropped. An IPv6 literal keeps its brackets, which hold its own colons.
fn split_url(case: &str, member: &str, url: &str) -> (String, String) {
    let after_scheme = url
        .split_once("://")
        .unwrap_or_else(|| panic!("case {case:?}: {member} must be an absolute URL, got {url:?}"))
        .1;
    let (authority, rest) = match after_scheme.find('/') {
        Some(end) => (&after_scheme[..end], &after_scheme[end..]),
        None => (after_scheme, ""),
    };
    let path = rest.split('?').next().unwrap_or(rest);
    (authority.to_string(), path.to_string())
}

/// Rule 3 over one whole `MediaPlacement`: every member names the address this emulator
/// answers on, keeps its own scheme, dials its own path, and names no host belonging to
/// someone else.
fn assert_announced_placement(case: &str, placement: &Value, meeting_id: &str, authority: &str) {
    let audio_host = string_member(placement, "AudioHostUrl");
    assert_eq!(
        audio_host, authority,
        "case {case:?}: expected AudioHostUrl to be the address this emulator answers on \
         ({authority:?}), got {audio_host:?}"
    );
    for (member, scheme, expected_path) in announced_urls(meeting_id) {
        let url = string_member(placement, member);
        assert!(
            url.starts_with(&format!("{scheme}://")),
            "case {case:?}: expected {member} to keep its {scheme} scheme, got {url:?}"
        );
        let (host, path) = split_url(case, member, &url);
        assert_eq!(
            host, authority,
            "case {case:?}: expected {member} to name the address this emulator answers on \
             ({authority:?}), got host {host:?} in {url:?}"
        );
        assert_eq!(
            path, expected_path,
            "case {case:?}: expected {member} to dial {expected_path:?} under that address, \
             got {url:?}"
        );
        // The same host read once more against the hosts that would take a client off this
        // machine: the equality above pins one deployment's address, this pins the class.
        for domain in FOREIGN_DOMAINS {
            assert!(
                !host.ends_with(domain),
                "case {case:?}: expected {member} to send no client to a {domain} host, got \
                 {url:?}"
            );
        }
    }
}

/// RULE 3: every `MediaPlacement` member a create or a read reports names an address THIS
/// emulator answers on — the `host[:port]` of the one endpoint it serves its whole surface
/// at — and the read of a meeting reports what its create reported.
///
/// Spanned over the addressing table and over both operations that mint a meeting, in media
/// Regions that differ from the endpoint Region, since no part of an announced address
/// depends on either.
#[tokio::test]
async fn every_announced_media_endpoint_names_an_address_this_emulator_answers_on() {
    for (index, addressing) in addressings().into_iter().enumerate() {
        let Addressing {
            case,
            served_at,
            host,
            authority,
        } = addressing;
        let emulator = match served_at {
            Some(base) => Emulator::served_at(base),
            None => Emulator::new(),
        };

        // Both creates that mint a meeting, in two media Regions, so neither the minting
        // operation nor the Region can change the address announced.
        let creates: Vec<(&str, &str, Value)> = vec![
            (
                "CreateMeeting",
                "/meetings",
                json!({
                    "ClientRequestToken": format!("tok-announced-{index}"),
                    "MediaRegion": "us-west-2",
                    "ExternalMeetingId": format!("ext-announced-{index}"),
                }),
            ),
            (
                "CreateMeetingWithAttendees",
                "/meetings?operation=create-attendees",
                json!({
                    "ClientRequestToken": format!("tok-announced-{index}-a"),
                    "MediaRegion": "ap-southeast-2",
                    "ExternalMeetingId": format!("ext-announced-{index}-a"),
                    "Attendees": [{ "ExternalUserId": "user-announced" }],
                }),
            ),
        ];

        for (operation, path, request) in creates {
            let case = format!("{case} ({operation})");
            let created = emulator.call_as("POST", path, Some(&request), host).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected HTTP 200, got {} with {:?}",
                created.status, created.body
            );
            let meeting = created.member("Meeting");
            let meeting_id = string_member(&meeting, "MeetingId");
            let placement = meeting
                .get("MediaPlacement")
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: no MediaPlacement in {meeting:?}"));
            assert_announced_placement(&case, &placement, &meeting_id, authority);

            // The read of the same meeting reports the same set.
            let read = emulator.get_meeting_as(&meeting_id, host).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: GetMeeting expected 200, got {} with {:?}",
                read.status, read.body
            );
            let read_placement = read.member("Meeting")["MediaPlacement"].clone();
            assert_announced_placement(&case, &read_placement, &meeting_id, authority);
            assert_eq!(
                read_placement, placement,
                "case {case:?}: expected GetMeeting to report the endpoints the create \
                 reported, got {read_placement:?}"
            );
        }
    }
}

/// Rule 3's other half: the announced endpoints are the DEPLOYMENT's one address, so two
/// clients that reached this emulator at different hosts are told the same set for one
/// meeting — which is what lets one client hand a `MeetingId` to another — and a read
/// changes nothing for the next reader.
#[tokio::test]
async fn two_callers_of_one_meeting_are_told_the_same_endpoints() {
    let emulator = Emulator::served_at("https://127.0.0.1:40305");
    let request = json!({
        "ClientRequestToken": "tok-two-callers",
        "MediaRegion": "eu-west-1",
        "ExternalMeetingId": "ext-two-callers",
    });
    let created = emulator
        .call_as("POST", "/meetings", Some(&request), Some("127.0.0.1:40305"))
        .await;
    assert_eq!(
        created.status, 200,
        "the shared meeting expected HTTP 200, got {} with {:?}",
        created.status, created.body
    );
    let meeting = created.member("Meeting");
    let meeting_id = string_member(&meeting, "MeetingId");
    let announced = meeting["MediaPlacement"].clone();

    // Rows: the hosts three clients of one emulator address it by, each reading twice, so
    // the second round proves a read through one host changed nothing for the others.
    let hosts = [
        "127.0.0.1:40305",
        "localhost:9297",
        "meetings-chime.us-east-1.amazonaws.com",
    ];
    for round in 1..=2u8 {
        for host in hosts {
            let case = format!("round {round} addressed by {host}");
            let read = emulator.get_meeting_as(&meeting_id, Some(host)).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: GetMeeting expected 200, got {} with {:?}",
                read.status, read.body
            );
            let placement = read.member("Meeting")["MediaPlacement"].clone();
            assert_announced_placement(&case, &placement, &meeting_id, "127.0.0.1:40305");
            assert_eq!(
                placement, announced,
                "case {case:?}: expected the endpoints the create announced, got \
                 {placement:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// Rule 4: the addresses rule 3 pins are SERVED.
// ---------------------------------------------------------------------------------------

/// The `MediaPlacement` members a client opens a SESSION on, which are the ones a dialled
/// address can be resolved to a meeting from: each carries the `MeetingId` — in its path, or
/// in the viewer endpoint's own query parameter.
///
/// The two `https` members are not here: `TurnControlUrl` and `EventIngestionUrl` carry no
/// `MeetingId` (`announced_urls` above shows both paths as fixed), so neither names a meeting
/// of this service at all.
const SESSION_MEMBERS: [&str; 5] = [
    "SignalingUrl",
    "AudioFallbackUrl",
    "ScreenDataUrl",
    "ScreenSharingUrl",
    "ScreenViewingUrl",
];

/// The RFC 6455 fields a client opening a WebSocket session sends, beside whatever credential
/// it presents. The key is any 16 base64-encoded bytes; this one is fixed so the case is
/// reproducible.
const HANDSHAKE_FIELDS: [(&str, &str); 4] = [
    ("connection", "Upgrade"),
    ("upgrade", "websocket"),
    ("sec-websocket-version", "13"),
    ("sec-websocket-key", "AAAAAAAAAAAAAAAAAAAAAA=="),
];

/// A well-formed `GuidString` (`^[a-fA-F0-9]{8}(?:-[a-fA-F0-9]{4}){3}-[a-fA-F0-9]{12}$`) that
/// no meeting of a fresh store was ever minted under, for the rows that dial a meeting this
/// emulator does not hold.
const UNKNOWN_MEETING_ID: &str = "7f3b1c2d-4e5f-4a6b-8c9d-0e1f2a3b4c5d";

/// The status that asks a client to switch protocols (RFC 7231 426 Upgrade Required), which is
/// what an ordinary request to an address dialled as a WebSocket session is answered with.
const UPGRADE_REQUIRED: u16 = 426;

/// The status a handshake presenting no join token of the meeting's attendees is answered
/// with: the one this service's `ForbiddenException` carries.
const NOT_AUTHENTICATED: u16 = 403;

/// The status an authenticated handshake is answered with: no Amazon Chime SDK media service
/// runs in this process, so the session itself is a protocol this server does not implement.
const NO_MEDIA_SERVICE: u16 = 501;

/// The status an address whose meeting this emulator does not hold is answered with.
const NO_SUCH_MEETING: u16 = 404;

/// One answer read off a dialled media address: the status line, and the sentence under it.
#[derive(Debug)]
struct Dialed {
    status: u16,
    body: String,
}

/// The `https` transport a `wss://` URL is dialled over: the same authority, path and query,
/// which is what the server sees as the request target of the opening handshake.
fn dialable(case: &str, announced: &str) -> String {
    let authority_and_path = announced.strip_prefix("wss://").unwrap_or_else(|| {
        panic!("case {case:?}: a session endpoint is announced as a wss:// URL, got {announced:?}")
    });
    format!("https://{authority_and_path}")
}

impl Emulator {
    /// Dial a published media address the way a client handed that URL does: no AWS signature
    /// — a client of a published media endpoint has no credential to sign with — and the
    /// fields it chose to send.
    async fn dial(&self, case: &str, announced: &str, fields: &[(&str, &str)]) -> Dialed {
        let mut builder = http::Request::builder()
            .method("GET")
            .uri(dialable(case, announced));
        for (name, value) in fields {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::new())
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = String::from_utf8_lossy(&response.into_body()).to_string();
        Dialed { status, body }
    }

    async fn create_attendee(&self, meeting_id: &str, external_user_id: &str) -> Reply {
        self.call(
            "POST",
            &format!("/meetings/{meeting_id}/attendees"),
            Some(&json!({ "ExternalUserId": external_user_id })),
        )
        .await
    }

    async fn delete_meeting(&self, meeting_id: &str) -> Reply {
        self.call("DELETE", &format!("/meetings/{meeting_id}"), None)
            .await
    }
}

/// What one dialling client PRESENTS, which is what decides the answer for a meeting this
/// emulator holds.
#[derive(Debug, Clone, Copy)]
enum Presents {
    /// No handshake at all — an ordinary request to the address.
    NoHandshake,
    /// A handshake and no credential of any kind.
    Nothing,
    /// A handshake offering a value that is nobody's join token.
    AnUnknownToken,
    /// A handshake offering the join token of an attendee of ANOTHER meeting.
    AnotherMeetingsToken,
    /// The meeting's own attendee's join token, offered as the session's subprotocol.
    OwnTokenAsSubprotocol,
    /// The same token, among the other subprotocols the client offers.
    OwnTokenAmongSubprotocols,
    /// The same token, carried in the query string of the dialled URL.
    OwnTokenInQuery,
}

/// The URL to dial and the fields to dial it with, for one presentation.
fn presented(
    presents: Presents,
    announced: &str,
    own_token: &str,
    other_token: &str,
) -> (String, Vec<(String, String)>) {
    let handshake: Vec<(String, String)> = HANDSHAKE_FIELDS
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    let offering = |offered: String| {
        let mut fields = handshake.clone();
        fields.push(("sec-websocket-protocol".to_string(), offered));
        fields
    };
    match presents {
        Presents::NoHandshake => (announced.to_string(), Vec::new()),
        Presents::Nothing => (announced.to_string(), handshake),
        Presents::AnUnknownToken => (
            announced.to_string(),
            offering("not-a-join-token-of-anybody".to_string()),
        ),
        Presents::AnotherMeetingsToken => (announced.to_string(), offering(other_token.to_string())),
        Presents::OwnTokenAsSubprotocol => (announced.to_string(), offering(own_token.to_string())),
        Presents::OwnTokenAmongSubprotocols => (
            announced.to_string(),
            offering(format!("a-session-protocol, {own_token}, another")),
        ),
        Presents::OwnTokenInQuery => {
            let separator = if announced.contains('?') { '&' } else { '?' };
            (
                format!("{announced}{separator}joinToken={own_token}"),
                handshake,
            )
        }
    }
}

/// RULE 4: every media session address a meeting publishes is one this emulator SERVES, and
/// what it answers is decided from this service's own state — the meeting the address names
/// and the join tokens its attendees hold — rather than from the shape of the request.
///
/// Spanned over every published session member and over what a client presents: no handshake
/// at all, a handshake with no credential, one offering a value nobody holds, one offering
/// ANOTHER meeting's attendee's token, and three ways of presenting this meeting's own
/// attendee's token. No row is ever answered with nothing, and no answer echoes the
/// `@sensitive` join token back.
#[tokio::test]
async fn every_published_media_address_answers_from_the_meetings_own_state() {
    let emulator = Emulator::served_at("https://127.0.0.1:40305");

    // Two meetings, so one meeting's join token can be presented to the other's address.
    let mut published: Vec<(String, Value, String)> = Vec::new();
    for (index, region) in ["us-east-1", "eu-west-1"].into_iter().enumerate() {
        let created = emulator
            .create_meeting(&json!({
                "ClientRequestToken": format!("tok-served-{index}"),
                "MediaRegion": region,
                "ExternalMeetingId": format!("ext-served-{index}"),
            }))
            .await;
        assert_eq!(
            created.status, 200,
            "setup: CreateMeeting expected 200, got {} with {:?}",
            created.status, created.body
        );
        let meeting = created.member("Meeting");
        let meeting_id = string_member(&meeting, "MeetingId");
        let attendee = emulator
            .create_attendee(&meeting_id, &format!("user-served-{index}"))
            .await;
        assert_eq!(
            attendee.status, 200,
            "setup: CreateAttendee expected 200, got {} with {:?}",
            attendee.status, attendee.body
        );
        let join_token = string_member(&attendee.member("Attendee"), "JoinToken");
        published.push((meeting_id, meeting["MediaPlacement"].clone(), join_token));
    }
    let (meeting_id, placement, own_token) = published[0].clone();
    let other_token = published[1].2.clone();
    assert_ne!(
        own_token, other_token,
        "setup: two meetings' attendees must hold different join tokens"
    );

    let presentations = [
        (Presents::NoHandshake, UPGRADE_REQUIRED),
        (Presents::Nothing, NOT_AUTHENTICATED),
        (Presents::AnUnknownToken, NOT_AUTHENTICATED),
        (Presents::AnotherMeetingsToken, NOT_AUTHENTICATED),
        (Presents::OwnTokenAsSubprotocol, NO_MEDIA_SERVICE),
        (Presents::OwnTokenAmongSubprotocols, NO_MEDIA_SERVICE),
        (Presents::OwnTokenInQuery, NO_MEDIA_SERVICE),
    ];

    for member in SESSION_MEMBERS {
        let announced = string_member(&placement, member);
        for (presents, expected) in presentations {
            let case = format!("{member} dialled presenting {presents:?}");
            let (url, fields) = presented(presents, &announced, &own_token, &other_token);
            let borrowed: Vec<(&str, &str)> = fields
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let dialed = emulator.dial(&case, &url, &borrowed).await;
            assert_eq!(
                dialed.status, expected,
                "case {case:?}: expected HTTP {expected} from the published address {url:?}, \
                 got {} with {:?}",
                dialed.status, dialed.body
            );
            assert!(
                !dialed.body.trim().is_empty(),
                "case {case:?}: expected the answer to word the outcome, got an empty body"
            );
            assert!(
                !dialed.body.contains(&own_token) && !dialed.body.contains(&other_token),
                "case {case:?}: a join token is sensitive and must not be echoed, got {:?}",
                dialed.body
            );
            // The address is a READ: the meeting it names is unchanged for the next caller.
            let read = emulator.get_meeting(&meeting_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the meeting must still be readable, got {} with {:?}",
                read.status, read.body
            );
            assert_eq!(
                read.member("Meeting")["MediaPlacement"], placement,
                "case {case:?}: dialling an address changed what the next caller is told"
            );
        }
    }
}

/// RULE 4, the other half: an address whose `MeetingId` names no meeting this emulator holds
/// is answered with the status for an absent resource and with the sentence this service words
/// that absence with — the one that tells a meeting which has ENDED from an id that never
/// named one here — so a client that kept a URL learns which of the two it is holding.
#[tokio::test]
async fn a_published_media_address_of_a_meeting_this_emulator_does_not_hold_is_refused() {
    let emulator = Emulator::served_at("https://127.0.0.1:40305");
    let created = emulator
        .create_meeting(&json!({
            "ClientRequestToken": "tok-served-gone",
            "MediaRegion": "ap-northeast-1",
            "ExternalMeetingId": "ext-served-gone",
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateMeeting expected 200, got {} with {:?}",
        created.status, created.body
    );
    let meeting = created.member("Meeting");
    let meeting_id = string_member(&meeting, "MeetingId");
    let placement = meeting["MediaPlacement"].clone();
    let attendee = emulator.create_attendee(&meeting_id, "user-gone").await;
    assert_eq!(
        attendee.status, 200,
        "setup: CreateAttendee expected 200, got {} with {:?}",
        attendee.status, attendee.body
    );
    let join_token = string_member(&attendee.member("Attendee"), "JoinToken");

    // An id this store never minted a meeting under, read on the SAME published addresses:
    // the id travels in the path of four members and in the query of the fifth, so swapping
    // it covers both forms.
    for member in SESSION_MEMBERS {
        let announced = string_member(&placement, member).replace(&meeting_id, UNKNOWN_MEETING_ID);
        let case = format!("{member} of a meeting that was never created");
        let (url, fields) = presented(Presents::Nothing, &announced, &join_token, &join_token);
        let borrowed: Vec<(&str, &str)> = fields
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let dialed = emulator.dial(&case, &url, &borrowed).await;
        assert_eq!(
            dialed.status, NO_SUCH_MEETING,
            "case {case:?}: expected HTTP {NO_SUCH_MEETING} for {url:?}, got {} with {:?}",
            dialed.status, dialed.body
        );
        assert!(
            dialed.body.contains(UNKNOWN_MEETING_ID),
            "case {case:?}: expected the sentence to name the id dialled, got {:?}",
            dialed.body
        );
    }

    // The same addresses once the meeting has ENDED: the token that authenticated an attendee
    // a moment ago no longer joins anything, and the answer says the meeting has ended rather
    // than that the id is unknown.
    let deleted = emulator.delete_meeting(&meeting_id).await;
    assert_eq!(
        deleted.status, 204,
        "setup: DeleteMeeting expected 204, got {} with {:?}",
        deleted.status, deleted.body
    );
    for member in SESSION_MEMBERS {
        let announced = string_member(&placement, member);
        for presents in [Presents::Nothing, Presents::OwnTokenAsSubprotocol] {
            let case = format!("{member} of a meeting that has ended, presenting {presents:?}");
            let (url, fields) = presented(presents, &announced, &join_token, &join_token);
            let borrowed: Vec<(&str, &str)> = fields
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let dialed = emulator.dial(&case, &url, &borrowed).await;
            assert_eq!(
                dialed.status, NO_SUCH_MEETING,
                "case {case:?}: expected HTTP {NO_SUCH_MEETING} for {url:?}, got {} with {:?}",
                dialed.status, dialed.body
            );
            assert!(
                !dialed.body.contains(&meeting_id),
                "case {case:?}: a meeting that has ended is reported by its lifecycle, not by \
                 the id, got {:?}",
                dialed.body
            );
        }
    }
}

/// RULE 4's bound: what this service answers on those addresses is bound to the `MeetingId`
/// form the model declares, so a path that merely SHARES one of these generic endpoint prefixes
/// — `/control/…` is another emulated service's own path space — is not answered as a meeting
/// of this one. A handshake whose id cannot be a `MeetingId` is nobody's media session here,
/// and an id in the pattern's upper-case form is, since the pattern admits both hex cases.
#[tokio::test]
async fn a_dialled_address_is_a_media_session_only_where_it_carries_a_meeting_id() {
    let emulator = Emulator::served_at("https://127.0.0.1:40305");
    let created = emulator
        .create_meeting(&json!({
            "ClientRequestToken": "tok-served-form",
            "MediaRegion": "us-west-2",
            "ExternalMeetingId": "ext-served-form",
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateMeeting expected 200, got {} with {:?}",
        created.status, created.body
    );
    let meeting = created.member("Meeting");
    let meeting_id = string_member(&meeting, "MeetingId");
    let signaling = string_member(&meeting["MediaPlacement"], "SignalingUrl");
    let (authority, _path) = split_url("setup", "SignalingUrl", &signaling);

    // Rows: the id a path carries under the signaling endpoint, and whether this service
    // answers that address as one of its meetings at all. The upper-case row is the pattern's
    // other hex case, which IS a `MeetingId`; the rest cannot be one.
    let rows: [(&str, String, bool); 5] = [
        (
            "another service's own /control path space",
            "support-case-42".to_string(),
            false,
        ),
        ("an empty segment", String::new(), false),
        (
            "a deeper path under the endpoint",
            format!("{UNKNOWN_MEETING_ID}/extra"),
            false,
        ),
        (
            "a GuidString with a group of the wrong length",
            "7f3b1c2d-4e5f-4a6b-8c9d-0e1f2a3b4c5".to_string(),
            false,
        ),
        (
            "the pattern's upper-case hex form",
            UNKNOWN_MEETING_ID.to_ascii_uppercase(),
            true,
        ),
    ];

    for (case, dialed_id, is_a_meeting_address) in rows {
        let url = format!("wss://{authority}/control/{dialed_id}");
        let fields: Vec<(&str, &str)> = HANDSHAKE_FIELDS.to_vec();
        let dialed = emulator.dial(case, &url, &fields).await;
        if is_a_meeting_address {
            assert_eq!(
                dialed.status, NO_SUCH_MEETING,
                "case {case:?}: expected HTTP {NO_SUCH_MEETING} for {url:?}, got {} with {:?}",
                dialed.status, dialed.body
            );
            assert!(
                dialed.body.contains(&dialed_id),
                "case {case:?}: expected the sentence to name the id dialled, got {:?}",
                dialed.body
            );
        } else {
            assert!(
                ![UPGRADE_REQUIRED, NOT_AUTHENTICATED, NO_MEDIA_SERVICE]
                    .contains(&dialed.status)
                    && !dialed.body.contains("media session"),
                "case {case:?}: expected {url:?} not to be answered as one of this service's \
                 media sessions, got {} with {:?}",
                dialed.status,
                dialed.body
            );
        }
    }

    // The meeting itself is untouched by any of it.
    let read = emulator.get_meeting(&meeting_id).await;
    assert_eq!(
        read.status, 200,
        "the meeting must still be readable, got {} with {:?}",
        read.status, read.body
    );
}

// ---------------------------------------------------------------------------------------
// Rule 4 over the WIRE: the published address is served on the socket a client dials.
// ---------------------------------------------------------------------------------------

/// How long one wire exchange may take before the test gives up: generous, because what is
/// waited for is the CONDITION (a response head on the socket) and not a duration — a loaded
/// host may take far longer than a fast one and still be correct.
const WIRE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Dial `url` on a real socket and write the request on its path, the way a client handed that
/// URL does, then read the status line the server answered with.
///
/// `upgrading` writes the RFC 6455 opening-handshake fields, which is what makes the request a
/// session handshake rather than an ordinary `GET` to the same address; `extra` carries whatever
/// the client presents beyond them.
///
/// Only the status LINE is read: the sentence under each status is asserted in the in-process
/// tables above, and what this exchange is for is that a client's bytes reach this service's
/// plane at all. The read loops until the response head has arrived rather than sleeping.
async fn answered_on_the_socket(
    case: &str,
    url: &str,
    upgrading: bool,
    extra: &[(&str, &str)],
) -> u16 {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let after_scheme = url
        .strip_prefix("wss://")
        .unwrap_or_else(|| panic!("case {case:?}: a session endpoint is a wss:// URL, got {url:?}"));
    let (authority, target) = match after_scheme.find('/') {
        Some(end) => (&after_scheme[..end], &after_scheme[end..]),
        None => (after_scheme, "/"),
    };
    // The port under the published authority, and the proof that authority is this machine's:
    // every address this emulator publishes is on the interface its listeners are bound on, so
    // the connection below is made to the loopback address named on its own line.
    let (host, port) = authority.split_once(':').unwrap_or_else(|| {
        panic!("case {case:?}: a published session address carries a port, got {authority:?}")
    });
    assert!(
        ["localhost", "127.0.0.1", "[::1]"].contains(&host),
        "case {case:?}: expected the published address to name this machine, got {host:?}"
    );
    let port: u16 = port
        .parse()
        .unwrap_or_else(|error| panic!("case {case:?}: {authority:?} names no port ({error})"));
    let mut request = format!("GET {target} HTTP/1.1\r\nHost: {authority}\r\n");
    if upgrading {
        for (name, value) in HANDSHAKE_FIELDS {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    for (name, value) in extra {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");

    let exchange = async {
        let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the published address {authority} must accept a connection, got {error}")
            });
        stream
            .write_all(request.as_bytes())
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the handshake must be sent, got {error}"));
        let mut answered = Vec::new();
        let mut chunk = [0u8; 1024];
        // Until the response HEAD has arrived — the condition this waits for — or the peer
        // closed, which would mean nothing answered.
        while !answered.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut chunk).await.unwrap_or_else(|error| {
                panic!("case {case:?}: the answer must be readable, got {error}")
            });
            assert!(
                read > 0,
                "case {case:?}: expected an HTTP/1.1 answer on {url:?}, got a closed connection \
                 after {:?}",
                String::from_utf8_lossy(&answered)
            );
            answered.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8_lossy(&answered).to_string()
    };
    let head = tokio::time::timeout(WIRE_DEADLINE, exchange)
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: {url:?} answered nothing within {WIRE_DEADLINE:?}"));

    let status_line = head.lines().next().unwrap_or_default().to_string();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected an HTTP/1.1 status line, got {status_line:?}")
        });
    assert!(
        status_line.starts_with("HTTP/1.1 "),
        "case {case:?}: expected an HTTP/1.1 status line, got {status_line:?}"
    );
    status
}

/// One exchange on the socket: the URL dialled, whether the request asks to upgrade, what it
/// presents, and the status the meeting's state earns it.
struct WireCase<'r> {
    case: &'r str,
    url: &'r str,
    upgrading: bool,
    presents: &'r [(&'r str, &'r str)],
    expected: u16,
}

/// RULE 4 over the wire: a client that dials the address `CreateMeeting` published — on a
/// socket, with an RFC 6455 opening handshake on the published path — is answered by this
/// service's media plane, with the status that meeting's state earns it. Nothing about those
/// answers depends on the request being dispatched in process.
///
/// The emulator is SERVED on a port of its own here and told the base it is reached at, so the
/// `MediaPlacement` the create publishes names this socket and the test dials exactly the value
/// it was handed. The rows span the three handshake verdicts and the ordinary request: no
/// credential, the attendee's own join token, a meeting this emulator does not hold, and a
/// request that does not ask to upgrade at all.
#[tokio::test]
async fn a_client_dialling_the_published_address_on_a_socket_is_answered_by_the_media_plane() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    let (endpoint, served) = emulator_server::serve_on_ephemeral_port(router).await;
    let authority = endpoint
        .strip_prefix("http://")
        .unwrap_or_else(|| panic!("setup: expected the served base, got {endpoint:?}"));
    // The base the emulator is reached at, recorded on the router the way the front door
    // records it when it binds (`emulator_server::EmulatorService::serving_at`). The front
    // door publishes an `https://` base and terminates TLS with the core's own certificate;
    // this test's socket speaks the same HTTP/1.1 in the clear on that authority, so the
    // exchange below needs no certificate of its own and the TLS half stays the core's.
    emulator_server::EmulatorService::serving_at(&served, &format!("https://{authority}"));
    // The control plane is reached through the SAME router that is serving that socket, so the
    // meeting the handshake names is the meeting this create persisted.
    let emulator = Emulator {
        router: served.clone(),
    };

    let created = emulator
        .create_meeting(&json!({
            "ClientRequestToken": "tok-served-wire",
            "MediaRegion": "us-east-1",
            "ExternalMeetingId": "ext-served-wire",
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateMeeting expected 200, got {} with {:?}",
        created.status, created.body
    );
    let meeting = created.member("Meeting");
    let meeting_id = string_member(&meeting, "MeetingId");
    let signaling = string_member(&meeting["MediaPlacement"], "SignalingUrl");
    assert_eq!(
        signaling,
        format!("wss://{authority}/control/{meeting_id}"),
        "setup: expected the published signaling address to name the socket this emulator is \
         served on, got {signaling:?}"
    );
    let attendee = emulator.create_attendee(&meeting_id, "user-wire").await;
    assert_eq!(
        attendee.status, 200,
        "setup: CreateAttendee expected 200, got {} with {:?}",
        attendee.status, attendee.body
    );
    let join_token = string_member(&attendee.member("Attendee"), "JoinToken");
    let subprotocol = [("sec-websocket-protocol", join_token.as_str())];
    let gone = signaling.replace(&meeting_id, UNKNOWN_MEETING_ID);

    // The rows: the three handshake verdicts a meeting's state decides, and the ordinary
    // request to the same address.
    let rows = [
        WireCase {
            case: "a handshake presenting no join token",
            url: signaling.as_str(),
            upgrading: true,
            presents: &[],
            expected: NOT_AUTHENTICATED,
        },
        WireCase {
            case: "a handshake presenting the attendee's join token",
            url: signaling.as_str(),
            upgrading: true,
            presents: &subprotocol,
            expected: NO_MEDIA_SERVICE,
        },
        WireCase {
            case: "a handshake for a meeting this emulator does not hold",
            url: gone.as_str(),
            upgrading: true,
            presents: &[],
            expected: NO_SUCH_MEETING,
        },
        WireCase {
            case: "an ordinary request to an address dialled as a session",
            url: signaling.as_str(),
            upgrading: false,
            presents: &[],
            expected: UPGRADE_REQUIRED,
        },
    ];

    for row in rows {
        let WireCase {
            case,
            url,
            upgrading,
            presents,
            expected,
        } = row;
        let status = answered_on_the_socket(case, url, upgrading, presents).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected HTTP {expected} on the socket at {url:?}, got {status}"
        );
    }

    // Dialling the address changed nothing a later caller reads.
    let read = emulator.get_meeting(&meeting_id).await;
    assert_eq!(
        read.status, 200,
        "the meeting must still be readable, got {} with {:?}",
        read.status, read.body
    );
    assert_eq!(
        string_member(&read.member("Meeting")["MediaPlacement"], "SignalingUrl"),
        signaling,
        "the published signaling address must be the one the create announced"
    );
}

/// RULE 4 with request authentication ON: a client of a published media address presents no AWS
/// signature — it holds the attendee's join token and nothing else — so the handshake is decided
/// by the meeting's own state rather than refused for want of a signature. That is what this
/// plane declaring no auth scheme buys, and it is the position the issue's own client is in.
///
/// The emulator verifies signatures for everything else here, which the unsigned control-plane
/// read at the end pins: without it this case would hold on an emulator that authenticates
/// nobody, and prove nothing about the plane.
#[tokio::test]
async fn an_unsigned_client_of_a_published_address_is_served_while_signatures_are_verified() {
    let mut emulator = Emulator::served_at("https://127.0.0.1:40305");
    let created = emulator
        .create_meeting(&json!({
            "ClientRequestToken": "tok-served-signed",
            "MediaRegion": "eu-central-1",
            "ExternalMeetingId": "ext-served-signed",
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateMeeting expected 200, got {} with {:?}",
        created.status, created.body
    );
    let meeting = created.member("Meeting");
    let meeting_id = string_member(&meeting, "MeetingId");
    let signaling = string_member(&meeting["MediaPlacement"], "SignalingUrl");
    let attendee = emulator.create_attendee(&meeting_id, "user-signed").await;
    assert_eq!(
        attendee.status, 200,
        "setup: CreateAttendee expected 200, got {} with {:?}",
        attendee.status, attendee.body
    );
    let join_token = string_member(&attendee.member("Attendee"), "JoinToken");

    // From here on the emulator verifies every request's signature, as one started with request
    // authentication does.
    emulator.router.set_request_authentication(true);

    for (case, presents, expected) in [
        (
            "an unsigned handshake presenting no join token",
            Presents::Nothing,
            NOT_AUTHENTICATED,
        ),
        (
            "an unsigned handshake presenting the attendee's join token",
            Presents::OwnTokenAsSubprotocol,
            NO_MEDIA_SERVICE,
        ),
    ] {
        let (url, fields) = presented(presents, &signaling, &join_token, &join_token);
        let borrowed: Vec<(&str, &str)> = fields
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let dialed = emulator.dial(case, &url, &borrowed).await;
        assert_eq!(
            dialed.status, expected,
            "case {case:?}: expected HTTP {expected} for an unsigned client of {url:?}, got {} \
             with {:?}",
            dialed.status, dialed.body
        );
    }

    // The same emulator, asked for the same meeting through a MODELLED route with no signature:
    // that one is refused, so the plane's answers above were not an unauthenticated emulator
    // answering everything.
    let control_plane = format!("wss://127.0.0.1:40305/meetings/{meeting_id}");
    let unsigned_read = emulator
        .dial("an unsigned control-plane read", &control_plane, &[])
        .await;
    assert_ne!(
        unsigned_read.status, 200,
        "an unsigned read of a modelled route must be refused while signatures are verified, \
         got {} with {:?}",
        unsigned_read.status, unsigned_read.body
    );
}


