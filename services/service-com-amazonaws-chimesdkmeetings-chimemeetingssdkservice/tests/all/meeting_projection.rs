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
