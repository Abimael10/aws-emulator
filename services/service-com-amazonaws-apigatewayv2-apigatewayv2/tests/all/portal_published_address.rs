// SPDX-License-Identifier: CC0-1.0

//! The address a portal PUBLISHES, dialled over a real connection.
//!
//! The rule these cases assert, one level above the recorded case that filed the issue (one
//! portal created with `Authorization={None:{}}` and API Gateway's default domain, published,
//! probed, disabled and probed again):
//!
//! > An address this service reports for a client to FETCH is an address the endpoint the
//! > caller is talking to ANSWERS on, whatever endpoint that caller reached this emulator at.
//! > For a portal that address is `EndpointConfigurationResponse$PortalDefaultDomainName` —
//! > the host the portal's site is served on, "generated and managed by API Gateway" — so a
//! > client that reads it off `CreatePortal` or `GetPortal` and opens a connection to it is
//! > served the build of the portal's last settled publish at its root, is refused while the
//! > portal has nothing live, and reaches nothing at all once the portal is deleted.
//!
//! The sibling suites (`portal_data_plane`, `portal_publication_revocation`) assert WHAT that
//! address answers through this emulator's own front door. What is asserted HERE is the half
//! those cannot see: that the published value is an address on this machine, that a client
//! which dials exactly what it was told completes a connection there, and that the connection
//! stops being accepted when the portal goes — because a value naming a host nothing on this
//! machine resolves passes every in-process check and still leaves the portal's site
//! unreachable.
//!
//! The rows span what the rule generalises over: the endpoint the CREATE was addressed at
//! (this service's own AWS host, the emulator's own endpoint as an IP literal with a port, a
//! loopback name with a port), the portal's `EndpointConfiguration` alternative (API
//! Gateway's default domain and a domain the account brought with its certificate), and the
//! `Authorization` alternative (`None` and a `CognitoConfig`). The publication states each
//! row walks are the flow itself: nothing published, published, a path with no page,
//! disabled, re-probed, and deleted.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::net::TcpStream;

use service_com_amazonaws_apigatewayv2_apigatewayv2::state::PORTAL_PREVIEW_SEGMENT;

use crate::api_lifecycle::{as_str, call_at_host, emulator};
use crate::custom_domain_mapping_invoke::{request_as, Answer};
use crate::portal_lifecycle::{custom_colors, portal_content};

/// The Region every control-plane call here is signed for.
const REGION: &str = "eu-west-1";

/// The statuses a probe of the published address earns: the portal's page, the refusal a
/// portal with nothing live answers, and the answer for a path the portal has no page at.
const SERVED: u16 = 200;
const FORBIDDEN: u16 = 403;
const NOT_FOUND: u16 = 404;

/// How long a wait for a CONDITION may run before a case gives up, and how long a probe of a
/// deleted address may take to be refused: both far beyond the span a portal's build is owed,
/// so a loaded host reaches the same outcome as an idle one. Nothing here is timed — the
/// deadline is only what keeps a case that will never hold from hanging the suite.
const CONDITION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before it asks the address again: short next to the span a build is
/// owed, so a case observes the condition soon after it holds, and long enough that polling
/// does not spin on connections while it waits. It bounds nothing the case asserts —
/// [`CONDITION_DEADLINE`] is the only bound.
const PROBE_INTERVAL: Duration = Duration::from_millis(50);

/// One row: the endpoint the create was addressed at, and the portal it creates.
struct Row {
    case: &'static str,
    /// The `Host` the control-plane calls carry — the endpoint this client reached the
    /// emulator at.
    endpoint_host: &'static str,
    /// The request `CreatePortal` is given.
    request: fn() -> Value,
}

/// A portal on API Gateway's default domain, with no authorization.
fn default_domain_portal() -> Value {
    json!({
        "authorization": { "none": {} },
        "endpointConfiguration": { "none": {} },
        "portalContent": portal_content("Published Portal")
    })
}

/// A Cognito-authorized portal on a domain the account brought with its certificate: the
/// other alternative of both unions, which changes nothing about the address the portal's
/// site is published at.
fn brought_domain_portal() -> Value {
    json!({
        "authorization": {
            "cognitoConfig": {
                "appClientId": "7bd0s2k9rr1cv9m0lq3ucaf9bk",
                "userPoolArn": format!(
                    "arn:aws:cognito-idp:{REGION}:000000000000:userpool/{REGION}_pooL1"
                ),
                "userPoolDomain": "https://portal-users.auth.eu-west-1.amazoncognito.com"
            }
        },
        "endpointConfiguration": {
            "acmManaged": {
                "domainName": "developers.example.com",
                "certificateArn": format!(
                    "arn:aws:acm:{REGION}:000000000000:certificate/\
                     11111111-2222-3333-4444-555555555555"
                )
            }
        },
        "portalContent": {
            "displayName": "Brought Domain Portal",
            "description": "the catalogue of our public APIs",
            "theme": { "customColors": custom_colors() }
        }
    })
}

fn rows() -> &'static [Row] {
    &[
        Row {
            case: "created through this service's own AWS host, on the default domain",
            endpoint_host: "apigateway.eu-west-1.amazonaws.com",
            request: default_domain_portal,
        },
        Row {
            case: "created through the emulator's own endpoint, on a domain the account \
                   brought",
            endpoint_host: "127.0.0.1:9297",
            request: brought_domain_portal,
        },
        Row {
            case: "created through a loopback name with a port, on the default domain",
            endpoint_host: "localhost:9297",
            request: default_domain_portal,
        },
    ]
}

/// One control-plane call of this row, addressed at the endpoint the row names.
async fn control(
    emulator: &MultiServiceRouter,
    row: &Row,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    call_at_host(emulator, row.endpoint_host, REGION, method, uri, body).await
}

/// Assert `published` is an address on THIS machine that a client can open a connection to:
/// what the member exists for is a consumer dialling it.
fn assert_dialable(case: &str, published: &str) {
    let address = published
        .parse::<std::net::SocketAddr>()
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected portalDefaultDomainName to be an address a client \
                 can dial, got {published:?}: {error}"
            )
        });
    assert!(
        address.ip().is_loopback(),
        "case {case:?}: expected an address of this machine, got {published:?}"
    );
    assert_ne!(
        address.port(),
        0,
        "case {case:?}: expected a port a client can dial, got {published:?}"
    );
}

/// Dial `published` for `path` until `holds` is true of the answer, and answer that probe.
/// `waited_for` names the condition in the failure message.
///
/// The wait is for the CONDITION, never for a fixed time: the address is asked again until the
/// condition holds or [`CONDITION_DEADLINE`] passes, so neither a fast nor a loaded host
/// changes what the case observes.
async fn probe_until(
    case: &str,
    published: &str,
    path: &str,
    waited_for: &str,
    holds: impl Fn(&Answer) -> bool,
) -> Answer {
    let deadline = std::time::Instant::now() + CONDITION_DEADLINE;
    loop {
        let answer = request_as(case, published, path, published).await;
        if holds(&answer) {
            return answer;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: {published}{path} never answered {waited_for}, last answer \
             {answer:?}"
        );
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

/// Assert one probe is the portal's own page: the document, as the portal's site, naming the
/// portal it was built for.
fn assert_page(case: &str, answer: &Answer, portal_id: &str) {
    assert_eq!(
        answer.status, SERVED,
        "case {case:?}: expected the portal's page, got {answer:?}"
    );
    assert!(
        answer
            .body
            .contains(&format!("data-portal-id=\"{portal_id}\"")),
        "case {case:?}: the page must name the portal it was built for, got {answer:?}"
    );
}

/// Whether a client can still open a connection to `published`.
///
/// The port is the one the portal published and the host is the loopback literal this
/// emulator's listeners bind, so nothing here dials off this machine. Answers as soon as the
/// connection is accepted or refused; [`CONDITION_DEADLINE`] is only the bound that keeps a
/// dial nothing ever answers from hanging the suite.
async fn connects(published: &str) -> bool {
    let Ok(address) = published.parse::<std::net::SocketAddr>() else {
        return false;
    };
    let dialed = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, address.port()));
    matches!(
        tokio::time::timeout(CONDITION_DEADLINE, dialed).await,
        Ok(Ok(_))
    )
}

/// The whole flow a consumer of a published portal drives, over real connections to the
/// address the service itself reported.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_dials_the_published_address_is_served_the_portals_site() {
    for row in rows() {
        let case = row.case;
        let emulator = emulator();

        let (status, created) =
            control(&emulator, row, "POST", "/v2/portals", Some((row.request)())).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreatePortal, got {status} {created}"
        );
        let portal_id = as_str(&created, "portalId");
        let endpoint = created
            .get("endpointConfiguration")
            .cloned()
            .unwrap_or(Value::Null);
        let published = as_str(&endpoint, "portalDefaultDomainName");
        assert_dialable(case, &published);

        // The reader reports the same address, so a client that goes through `GetPortal` —
        // which is what a client that did not make the create does — dials what the create
        // published.
        let (status, read) = control(
            &emulator,
            row,
            "GET",
            &format!("/v2/portals/{portal_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetPortal, got {status} {read}"
        );
        let read_published = as_str(
            read.get("endpointConfiguration").unwrap_or(&Value::Null),
            "portalDefaultDomainName",
        );
        assert_eq!(
            read_published, published,
            "case {case:?}: expected GetPortal to report the published address, got {read}"
        );

        // Nothing has published this portal, so the address ANSWERS — a connection is
        // accepted and an HTTP answer comes back — with the refusal a site with nothing live
        // earns, rather than with the page or with nothing at all.
        let answer = request_as(case, &published, "/", &published).await;
        assert_eq!(
            answer.status, FORBIDDEN,
            "case {case:?}: expected {FORBIDDEN} from the published address before the first \
             publish, got {answer:?}"
        );

        // A publish only STARTS the build, so the page appears once that build has come due.
        let (status, body) = control(
            &emulator,
            row,
            "POST",
            &format!("/v2/portals/{portal_id}/publish"),
            Some(json!({})),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected 202 from PublishPortal, got {status} {body}"
        );
        let answer = probe_until(case, &published, "/", "the portal's page", |answer| {
            answer.status == SERVED
        })
        .await;
        assert_page(case, &answer, &portal_id);

        // A path the portal has no page at is its own class on that same address.
        let answer = request_as(case, &published, "/nowhere", &published).await;
        assert_eq!(
            answer.status, NOT_FOUND,
            "case {case:?}: expected {NOT_FOUND} for a path the portal has no page at, got \
             {answer:?}"
        );

        // The preview the portal announces is on the same address, and is served there once
        // its build has come due.
        let (status, body) = control(
            &emulator,
            row,
            "POST",
            &format!("/v2/portals/{portal_id}/preview"),
            None,
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected 202 from PreviewPortal, got {status} {body}"
        );
        let (status, read) = control(
            &emulator,
            row,
            "GET",
            &format!("/v2/portals/{portal_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetPortal, got {status} {read}"
        );
        let preview_url = read
            .get("preview")
            .map(|preview| as_str(preview, "previewUrl"))
            .unwrap_or_default();
        assert_eq!(
            preview_url,
            format!("https://{published}/{PORTAL_PREVIEW_SEGMENT}"),
            "case {case:?}: expected the preview URL on the published address, got {read}"
        );
        let answer = probe_until(
            case,
            &published,
            &format!("/{PORTAL_PREVIEW_SEGMENT}"),
            "the portal's preview",
            |answer| answer.status == SERVED,
        )
        .await;
        assert_page(&format!("{case}: the preview"), &answer, &portal_id);

        // The take-down REVOKES public access, so the very first probe after its 204 finds
        // nothing served — and every later probe answers the same.
        let (status, body) = control(
            &emulator,
            row,
            "DELETE",
            &format!("/v2/portals/{portal_id}/publish"),
            None,
        )
        .await;
        assert_eq!(
            status, 204,
            "case {case:?}: expected 204 from DisablePortal, got {status} {body}"
        );
        for probe in 1..=3 {
            let answer = request_as(case, &published, "/", &published).await;
            assert_eq!(
                answer.status, FORBIDDEN,
                "case {case:?}: probe {probe} after the disable must find nothing served, got \
                 {answer:?}"
            );
        }

        // And the address itself goes with the portal: a deleted portal's site is reached by
        // nobody, so the connection is not even accepted any more.
        let (status, body) = control(
            &emulator,
            row,
            "DELETE",
            &format!("/v2/portals/{portal_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 204,
            "case {case:?}: expected 204 from DeletePortal, got {status} {body}"
        );
        assert!(
            !connects(&published).await,
            "case {case:?}: expected {published} to accept no connection once the portal is \
             deleted"
        );
    }
}

/// One portal, one address: every portal of the account is published at an address of its
/// own, and each serves ITS own site rather than another portal's.
#[tokio::test(flavor = "multi_thread")]
async fn each_portals_published_address_serves_only_its_own_site() {
    let case = "two portals of one account, both published";
    let emulator = emulator();
    let row = &rows()[0];

    let mut portals: Vec<(String, String)> = Vec::new();
    for index in 0..2 {
        let (status, created) = control(
            &emulator,
            row,
            "POST",
            "/v2/portals",
            Some(json!({
                "authorization": { "none": {} },
                "endpointConfiguration": { "none": {} },
                "portalContent": portal_content(&format!("Portal {index}"))
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreatePortal, got {status} {created}"
        );
        let portal_id = as_str(&created, "portalId");
        let published = as_str(
            created
                .get("endpointConfiguration")
                .unwrap_or(&Value::Null),
            "portalDefaultDomainName",
        );
        assert_dialable(case, &published);
        let (status, body) = control(
            &emulator,
            row,
            "POST",
            &format!("/v2/portals/{portal_id}/publish"),
            Some(json!({})),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected 202 from PublishPortal, got {status} {body}"
        );
        portals.push((portal_id, published));
    }

    assert_ne!(
        portals[0].1, portals[1].1,
        "case {case:?}: expected each portal its own address, got {portals:?}"
    );
    for (portal_id, published) in &portals {
        let answer = probe_until(case, published, "/", "the portal's page", |answer| {
            answer.status == SERVED
        })
        .await;
        assert_page(case, &answer, portal_id);
        let other = portals
            .iter()
            .find(|(other_id, _)| other_id != portal_id)
            .map(|(other_id, _)| other_id.clone())
            .unwrap_or_default();
        assert!(
            !answer
                .body
                .contains(&format!("data-portal-id=\"{other}\"")),
            "case {case:?}: {published} must serve only its own portal's page, got {answer:?}"
        );
    }
}
