// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS a user-authentication action tells the identity provider to send the
//! authorization answer back to: the `redirect_uri` of the redirect an unauthenticated
//! request is answered with.
//!
//! The rule under test: **the callback a user-authentication action announces is the load
//! balancer's own `/oauth2/idpresponse` endpoint on the ADDRESS THE CLIENT ADDRESSED — the
//! host it used and the port it dialled, one authority — so the identity provider sends the
//! client back to where it is already talking; it is never assembled out of two addresses,
//! and never carries the port of the listener the connection reached instead of the port the
//! client dialled.**
//!
//! Grounded in the model and in how this emulator is reached: an authentication action is an
//! HTTPS listener's ("[HTTPS listeners] Information for using Amazon Cognito to authenticate
//! users", `Action$AuthenticateCognitoConfig`), so the callback is an `https` URL, which
//! carries a port only where that port is not the one `https` implies; and the one address a
//! client on this machine reaches a load balancer of this emulator at is the `host:port` the
//! load balancer PUBLISHES (`LoadBalancer$DNSName`, "the public DNS name of the load
//! balancer"), which is not the port its listener declares. A callback on any other authority
//! names an address nothing need answer on, so the sign-in leg that comes back is lost.
//!
//! What makes the two readings different is the leg a load balancer's own socket forwards:
//! that leg is re-addressed to this API's own endpoint carrying the PORT OF THE LISTENER the
//! connection reached — which is what decides whose rules are evaluated — while the address
//! the client dialled travels beside it (`crate::service::listener_socket`). One of those two
//! ports selects the listener; the other is the client's own, and only the client's own can
//! be in a URL the client comes back to.
//!
//! The rows differ along the ADDRESSING the request reached the plane by and the port that
//! addressing names: the published address itself, the socket's own leg where the client
//! dialled a port of its own, where it named no port at all, where it named exactly the port
//! `https` implies, a leg re-addressed on a SECOND listener's port (so the listener that
//! answers and the port the callback carries are two different ports at once), this API's own
//! endpoint with no client host beside it (an in-process caller, whose addressing IS the
//! published name) on each of the two listeners' ports, and the reserved data-plane path under
//! the emulator's own endpoint.

use std::time::{Duration, SystemTime};

use crate::alb_data_plane::{create_load_balancer, send, settle};
use crate::provisioning_loop::{action, call, emulator, p};

/// The endpoint of THIS API, which is the host a request forwarded from a load balancer's own
/// socket to this plane carries, with the port of the listener the connection reached on it.
const SERVICE_ENDPOINT: &str = "elasticloadbalancingv2.amazonaws.com";

/// The host a client addresses the emulator's own endpoint by, which is the only address a
/// client on the machine this emulator runs on can reach.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// The two HTTPS listeners of the load balancer below, and the app client each names — which
/// is how a row tells WHICH listener answered it.
const FRONT_PORT: &str = "443";
const SIDE_PORT: &str = "8443";
const FRONT_CLIENT: &str = "front-door-client";
const SIDE_CLIENT: &str = "side-door-client";

/// The certificate the HTTPS listeners carry. The service that holds certificates is not
/// mounted on this router, and a certificate its owner does not answer about is not refused
/// (`crate::state::certificate_resolution`), so a well-formed ARN is what a listener names
/// here; the certificate actually PRESENTED on a handshake is `alb_https_listener_tls`.
const CERTIFICATE: &str =
    "arn:aws:acm:us-west-2:000000000000:certificate/00000000-0000-0000-0000-000000000000";

/// The user pool the actions authenticate against, and the domain prefix its hosted
/// authorization endpoint is served under.
const POOL_ARN: &str = "arn:aws:cognito-idp:us-west-2:111111111111:userpool/us-west-2_abc123";
const POOL_DOMAIN: &str = "sign-in";

/// The path on the load balancer's own name the authorization answer comes back to.
const CALLBACK_PATH: &str = "/oauth2/idpresponse";

/// The body the routing action behind the authentication action answers with, which no row
/// here sees: every row's request is one the authentication action answers itself.
const PROTECTED_BODY: &str = "protected";

/// The `authenticate-cognito` action naming `client_id`, followed by the routing action it
/// guards — the one action a rule must end with.
fn guarded(client_id: &str) -> Vec<(String, String)> {
    let member = "DefaultActions.member.1.AuthenticateCognitoConfig";
    vec![
        p("DefaultActions.member.1.Type", "authenticate-cognito"),
        p("DefaultActions.member.1.Order", "1"),
        p(&format!("{member}.UserPoolArn"), POOL_ARN),
        p(&format!("{member}.UserPoolClientId"), client_id),
        p(&format!("{member}.UserPoolDomain"), POOL_DOMAIN),
        p("DefaultActions.member.2.Type", "fixed-response"),
        p("DefaultActions.member.2.Order", "2"),
        p("DefaultActions.member.2.FixedResponseConfig.StatusCode", "200"),
        p(
            "DefaultActions.member.2.FixedResponseConfig.MessageBody",
            PROTECTED_BODY,
        ),
    ]
}

/// Create one HTTPS listener on `port` whose default actions are the authentication action
/// naming `client_id` and the routing action behind it.
async fn create_listener(
    router: &smithy_server_runtime::MultiServiceRouter,
    lb_arn: &str,
    port: &str,
    client_id: &str,
) {
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", lb_arn));
    form.push(p("Protocol", "HTTPS"));
    form.push(p("Port", port));
    form.push(p("Certificates.member.1.CertificateArn", CERTIFICATE));
    form.extend(guarded(client_id));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener on {port}: expected 200, got {} body {}",
        reply.status, reply.body
    );
}

/// The RAW (still percent-encoded) value of one query parameter of `location`.
fn parameter(location: &str, name: &str) -> Option<String> {
    let query = location.split_once('?')?.1;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(held, _)| *held == name)
        .map(|(_, value)| value.to_string())
}

/// One query parameter of `location` as its DECODED value, which is what the client reads
/// out of the authorization request. The decoding is the maintained crate's, the same one
/// the plane encodes with.
fn decoded(location: &str, name: &str) -> Option<String> {
    let raw = parameter(location, name)?;
    Some(
        percent_encoding::percent_decode_str(&raw)
            .decode_utf8_lossy()
            .to_string(),
    )
}

/// RULE: the callback a user-authentication action announces carries the authority the CLIENT
/// addressed — its host and the port it dialled — whichever way the request reached this
/// plane, while the port the request was ADDRESSED on goes on deciding which listener answers
/// it.
#[tokio::test]
async fn the_announced_callback_carries_the_address_the_client_dialled() {
    let (router, engine) = emulator();
    let (lb_arn, published) = create_load_balancer(&router, "auth-callback-alb").await;
    create_listener(&router, &lb_arn, FRONT_PORT, FRONT_CLIENT).await;
    create_listener(&router, &lb_arn, SIDE_PORT, SIDE_CLIENT).await;
    // The move to `active` the clock owes the load balancer, placed rather than slept for.
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    // The address this emulator opened for the load balancer, which is the one a client that
    // honours its published name dials: a `host:port` of this machine, on a port that is not
    // the one `https` implies, so a callback on it has to carry that port.
    assert!(
        published.parse::<std::net::SocketAddr>().is_ok(),
        "setup: expected the published address to be one a client can dial, got {published:?}"
    );
    let prefixed = format!("/_aws/elasticloadbalancing/{published}/hosted");
    let front_leg = format!("{SERVICE_ENDPOINT}:{FRONT_PORT}");
    let side_leg = format!("{SERVICE_ENDPOINT}:{SIDE_PORT}");
    let published_callback = format!("https://{published}{CALLBACK_PATH}");

    /// One row: the addressing the request reached the plane by, the callback the redirect
    /// must announce, and the app client whose listener must have answered it.
    struct Row<'a> {
        case: &'a str,
        /// The `Host` the request carries.
        host: &'a str,
        /// The path, within whatever that host is.
        target: &'a str,
        /// The other headers it carries — the address the client dialled, for a leg a load
        /// balancer's own socket forwarded.
        headers: &'a [(&'a str, &'a str)],
        /// The whole URL the `redirect_uri` parameter must carry.
        callback: &'a str,
        /// The app client of the listener that must answer, which is the port the addressing
        /// names.
        client_id: &'a str,
    }
    let rows: Vec<Row<'_>> = vec![
        Row {
            case: "the published address, dialled by a client",
            host: published.as_str(),
            target: "/hosted",
            headers: &[],
            callback: published_callback.as_str(),
            client_id: FRONT_CLIENT,
        },
        Row {
            case: "the socket's own leg, carrying the address the client dialled",
            host: front_leg.as_str(),
            target: prefixed.as_str(),
            headers: &[("x-forwarded-host", "shop.example.com:8443")],
            callback: "https://shop.example.com:8443/oauth2/idpresponse",
            client_id: FRONT_CLIENT,
        },
        Row {
            // An in-process caller of this plane, which carries no client host beside the
            // endpoint it addressed: the host it addressed is the name the load balancer
            // published, so the callback is that name's WHOLE authority — the port it carries
            // included, since that is where the published name answers.
            case: "this API's own endpoint, with no client host beside it",
            host: front_leg.as_str(),
            target: prefixed.as_str(),
            headers: &[],
            callback: published_callback.as_str(),
            client_id: FRONT_CLIENT,
        },
        Row {
            // The same addressing on the SECOND listener's port: the listener that answers is
            // the one that port names, and the callback is still the published authority.
            case: "this API's own endpoint on the second listener's port",
            host: side_leg.as_str(),
            target: prefixed.as_str(),
            headers: &[],
            callback: published_callback.as_str(),
            client_id: SIDE_CLIENT,
        },
        Row {
            case: "the same leg where the client named no port",
            host: front_leg.as_str(),
            target: prefixed.as_str(),
            headers: &[("x-forwarded-host", "shop.example.com")],
            callback: "https://shop.example.com/oauth2/idpresponse",
            client_id: FRONT_CLIENT,
        },
        Row {
            case: "the same leg where the client dialled the port https implies",
            host: front_leg.as_str(),
            target: prefixed.as_str(),
            headers: &[("x-forwarded-host", "shop.example.com:443")],
            callback: "https://shop.example.com/oauth2/idpresponse",
            client_id: FRONT_CLIENT,
        },
        Row {
            // The two ports are different here: the leg was re-addressed on the second
            // listener's port, which is the listener that must answer, while the client
            // dialled a third port — the one the callback must carry.
            case: "a leg re-addressed on the second listener's port",
            host: side_leg.as_str(),
            target: prefixed.as_str(),
            headers: &[("x-forwarded-host", "shop.example.com:9443")],
            callback: "https://shop.example.com:9443/oauth2/idpresponse",
            client_id: SIDE_CLIENT,
        },
        Row {
            case: "the reserved data-plane path under the emulator's own endpoint",
            host: EMULATOR_HOST,
            target: prefixed.as_str(),
            headers: &[],
            callback: "https://127.0.0.1:9297/oauth2/idpresponse",
            client_id: FRONT_CLIENT,
        },
    ];

    for row in &rows {
        let answered = send(&router, "GET", row.host, row.target, row.headers).await;
        assert_eq!(
            answered.status, 302,
            "case {:?}: expected HTTP 302, got {} with {:?}",
            row.case,
            answered.status,
            answered.header("location")
        );
        let location = answered.header("location").unwrap_or_default();
        assert_eq!(
            decoded(location, "redirect_uri").as_deref(),
            Some(row.callback),
            "case {:?}: expected the callback {:?}, got {location:?}",
            row.case,
            row.callback
        );
        // WHICH listener answered, which the port the request was ADDRESSED on decides: the
        // reading the callback's own port must not have taken over.
        assert_eq!(
            decoded(location, "client_id").as_deref(),
            Some(row.client_id),
            "case {:?}: expected the listener naming {:?} to answer, got {location:?}",
            row.case,
            row.client_id
        );
        // The callback leads back to the load balancer, never to an endpoint of this API's:
        // the client follows this URL, and no client resolves that name.
        assert!(
            !location.contains(SERVICE_ENDPOINT),
            "case {:?}: the callback must not name this API's own endpoint, got {location:?}",
            row.case
        );
    }
}
