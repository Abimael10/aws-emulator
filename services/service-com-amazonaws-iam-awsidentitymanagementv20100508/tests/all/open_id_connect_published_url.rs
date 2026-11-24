// SPDX-License-Identifier: CC0-1.0

//! The `Url` a read of an IAM OIDC provider REPORTS, end to end over the awsQuery wire.
//!
//! RULE: a provider's URL is the value its registration carried — "the URL of the identity
//! provider … should correspond to the iss claim in the provider's OpenID Connect ID tokens"
//! (`CreateOpenIDConnectProvider$Url`) — stored with its `https://` scheme removed and
//! reported back exactly as stored, whatever host it names. It is caller input echoed back,
//! so no address of the emulator serving the request goes into it: a read made against a
//! router SERVED at a loopback address reports the registered issuer, not that address.
//!
//! The URL is also the provider's identity: its ARN is derived from it, and "you cannot
//! register the same provider multiple times in a single Amazon Web Services account", so the
//! second create for one issuer collides on the text the request carried.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: whether the
//! host is one AWS itself publishes an endpoint at, which service's endpoint it is, how many
//! path segments the issuer carries, whether it carries a path or a port at all, and whether
//! the host is a third-party one.
//!
//! The awsQuery harness is `tests/all/user_lifecycle.rs`'s, driven here against a router
//! SERVED at an address — which is what the production router is, and what would give a
//! rewrite a host to name.

use std::sync::Arc;

use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::user_lifecycle::{account_id, call, element};

/// The address the emulator under test is served at: a loopback address with a port, the one
/// form a client on this machine can reach.
const SERVED_BASE: &str = "https://127.0.0.1:9297";

/// Its `host:port` — the authority no reported `Url` may carry, since none of these
/// providers was registered under it.
const SERVED_AUTHORITY: &str = "127.0.0.1:9297";

/// A router serving only this service over one shared engine, SERVED at [`SERVED_BASE`] —
/// the state the production router is in once the data port is bound.
fn served_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.set_url_base(EndpointUrl::parse(SERVED_BASE).expect("a served endpoint"));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One row: the issuer URL the create registers, and the `Url` every read must report.
struct Reported {
    case: &'static str,
    /// The `Url` the create carries, scheme included, as a caller supplies one.
    registered: &'static str,
    /// What a read must report, with no scheme.
    reported: &'static str,
}

/// Rows spanning the hosts an issuer is registered under.
const REPORTED: &[Reported] = &[
    Reported {
        case: "a user-pool issuer of a service this emulator also emulates",
        registered: "https://cognito-idp.us-west-2.amazonaws.com/us-west-2_A1b2C3d4E",
        reported: "cognito-idp.us-west-2.amazonaws.com/us-west-2_A1b2C3d4E",
    },
    Reported {
        case: "an issuer of another AWS service, with several path segments",
        registered: "https://oidc.eks.eu-west-1.amazonaws.com/id/7A1B2C3D4E5F",
        reported: "oidc.eks.eu-west-1.amazonaws.com/id/7A1B2C3D4E5F",
    },
    Reported {
        case: "an AWS-hosted issuer carrying no path at all",
        registered: "https://oidc.eks.us-east-1.amazonaws.com",
        reported: "oidc.eks.us-east-1.amazonaws.com",
    },
    Reported {
        case: "a third-party issuer",
        registered: "https://server.example.com/oidc",
        reported: "server.example.com/oidc",
    },
    Reported {
        case: "a third-party host that merely ends in an AWS zone's label",
        registered: "https://amazonaws.com.example.net/oidc",
        reported: "amazonaws.com.example.net/oidc",
    },
    Reported {
        case: "an issuer whose host carries a port",
        registered: "https://server.example.com:8443/oidc",
        reported: "server.example.com:8443/oidc",
    },
];

/// RULE: the `Url` a read reports is the registered host-and-path, and the provider's ARN
/// names that same issuer.
#[tokio::test]
async fn a_read_reports_the_registered_issuer() {
    for row in REPORTED {
        // One account per row, so the provider a read reports is this row's.
        let router = served_router();
        let (status, created) = call(
            &router,
            "CreateOpenIDConnectProvider",
            &[("Url", row.registered)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateOpenIDConnectProvider must succeed, got {status}: {created}",
            row.case
        );

        // The provider's identity is the URL as it was REGISTERED, and the ARN is derived
        // from it.
        let arn = element(&created, "OpenIDConnectProviderArn");
        let expected_arn = format!(
            "arn:aws:iam::{}:oidc-provider/{}",
            account_id(),
            row.reported
        );
        assert_eq!(
            arn, expected_arn,
            "case {:?}: expected the ARN {expected_arn:?} of the registered issuer, got \
             {arn:?}",
            row.case
        );

        // Two reads, because what a provider reports must not move under a caller that read
        // it twice.
        for attempt in 1..=2 {
            let (status, body) = call(
                &router,
                "GetOpenIDConnectProvider",
                &[("OpenIDConnectProviderArn", arn.as_str())],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: read {attempt} must succeed, got {status}: {body}",
                row.case
            );
            let url = element(&body, "Url");
            assert_eq!(
                url, row.reported,
                "case {:?}: expected read {attempt} to report {:?}, got {url:?}",
                row.case, row.reported
            );
            // The address this emulator is served at is no part of a value the caller
            // supplied, so it appears in none of them.
            assert!(
                !url.contains(SERVED_AUTHORITY),
                "case {:?}: expected read {attempt} to carry none of this emulator's own \
                 address {SERVED_AUTHORITY:?}, got {url:?}",
                row.case
            );
        }
    }
}

/// RULE: the registered URL is the provider's identity, so registering the SAME issuer twice
/// collides on the issuer the request named.
#[tokio::test]
async fn the_registered_issuer_is_the_providers_identity() {
    let router = served_router();
    let issuer = "https://cognito-idp.us-west-2.amazonaws.com/us-west-2_Z9y8X7w6V";
    let (status, created) = call(&router, "CreateOpenIDConnectProvider", &[("Url", issuer)]).await;
    assert_eq!(
        status, 200,
        "the first create must succeed, got {status}: {created}"
    );

    let (status, refused) = call(&router, "CreateOpenIDConnectProvider", &[("Url", issuer)]).await;
    assert_eq!(
        status, 409,
        "a second create for the same issuer must be refused, got {status}: {refused}"
    );
    let message = element(&refused, "Message");
    let expected = format!("Provider with url {issuer} already exists.");
    assert_eq!(
        message, expected,
        "expected the refusal to name the issuer the request carried, got {message:?}"
    );
}
