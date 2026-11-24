// SPDX-License-Identifier: CC0-1.0

//! The `Url` a read of an IAM OIDC provider PUBLISHES, end to end over the awsQuery wire.
//!
//! RULE: a provider's URL is an address a relying party FETCHES — it "should correspond to
//! the iss claim in the provider's OpenID Connect ID tokens"
//! (`CreateOpenIDConnectProvider$Url`), and an OIDC client bootstraps a federation by
//! resolving that issuer and reading the discovery document published under it — so an
//! issuer hosted at one of AWS's own endpoints, which this emulator serves rather than the
//! real service, is published under the address THIS emulator is reached at, with the
//! issuer's own path kept. An issuer under any other host is an address outside this
//! emulator either way and is published exactly as it was registered. Neither form carries a
//! scheme, which is the host-and-path form IAM stores and reports.
//!
//! What the account holds is untouched: the URL is the provider's identity and its ARN is
//! derived from it, so the ARN a create answers with still names the registered issuer and a
//! second create for the same issuer still collides.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: whether the
//! host is one AWS publishes an endpoint at, which service's endpoint it is, how many path
//! segments the issuer carries, whether it carries a path at all, and whether the host is a
//! third-party one.
//!
//! The awsQuery harness is `tests/all/user_lifecycle.rs`'s, driven here against a router
//! SERVED at an address — which is what the production router is, and what makes the
//! endpoint this emulator publishes a real one.

use std::sync::Arc;

use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::user_lifecycle::{account_id, call, element};

/// The address the emulator under test is served at: a loopback address with a port, the one
/// form a client on this machine can reach.
const SERVED_BASE: &str = "https://127.0.0.1:9297";

/// Its `host:port`, which is what a published URL carries.
const SERVED_AUTHORITY: &str = "127.0.0.1:9297";

/// A router serving only this service over one shared engine, SERVED at [`SERVED_BASE`] —
/// the state the production router is in once the data port is bound, which is what gives a
/// published address a host to name.
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

/// One row: the issuer URL the create registers, and the `Url` every read must publish.
struct Published {
    case: &'static str,
    /// The `Url` the create carries, scheme included, as a caller supplies one.
    registered: &'static str,
    /// What a read must publish, with no scheme.
    published: &'static str,
}

/// Rows spanning the hosts an issuer is registered under.
const PUBLISHED: &[Published] = &[
    Published {
        case: "a user-pool issuer of a service this emulator serves",
        registered: "https://cognito-idp.us-west-2.amazonaws.com/us-west-2_A1b2C3d4E",
        published: "127.0.0.1:9297/us-west-2_A1b2C3d4E",
    },
    Published {
        case: "an issuer of another AWS service, with several path segments",
        registered: "https://oidc.eks.eu-west-1.amazonaws.com/id/7A1B2C3D4E5F",
        published: "127.0.0.1:9297/id/7A1B2C3D4E5F",
    },
    Published {
        case: "an AWS-hosted issuer carrying no path at all",
        registered: "https://oidc.eks.us-east-1.amazonaws.com",
        published: "127.0.0.1:9297",
    },
    Published {
        case: "a third-party issuer, which this emulator does not serve",
        registered: "https://server.example.com/oidc",
        published: "server.example.com/oidc",
    },
    Published {
        case: "a third-party host that merely ends in an AWS zone's label",
        registered: "https://amazonaws.com.example.net/oidc",
        published: "amazonaws.com.example.net/oidc",
    },
];

/// RULE: the `Url` a read publishes leads back to this emulator for an issuer hosted at one
/// of AWS's own endpoints, and is the registered host-and-path for any other issuer — while
/// the provider's ARN keeps naming the issuer as it was registered.
#[tokio::test]
async fn a_read_publishes_an_aws_hosted_issuer_at_this_emulators_address() {
    for row in PUBLISHED {
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

        // The provider's identity is the URL as it was REGISTERED: the ARN is derived from
        // it, so the rewrite reaches the published address and nothing else.
        let arn = element(&created, "OpenIDConnectProviderArn");
        let registered_path = row
            .registered
            .strip_prefix("https://")
            .expect("the row registers an https issuer");
        let expected_arn = format!(
            "arn:aws:iam::{}:oidc-provider/{registered_path}",
            account_id()
        );
        assert_eq!(
            arn, expected_arn,
            "case {:?}: expected the ARN {expected_arn:?} of the registered issuer, got \
             {arn:?}",
            row.case
        );

        // Two reads, because the published address must not move under a caller that read
        // the provider twice.
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
                url, row.published,
                "case {:?}: expected read {attempt} to publish {:?}, got {url:?}",
                row.case, row.published
            );
            // Whatever the host is, it is never one of AWS's own endpoints unless the
            // emulator is not the one serving it: a published address a client cannot reach
            // is what the rule exists to prevent.
            let host = url.split('/').next().unwrap_or(&url);
            let aws_hosted = ::service_common::data_plane_addressing::addresses_an_aws_endpoint(
                registered_path.split('/').next().unwrap_or(registered_path),
            );
            if aws_hosted {
                assert_eq!(
                    host, SERVED_AUTHORITY,
                    "case {:?}: expected the published host to be this emulator's \
                     {SERVED_AUTHORITY:?}, got {host:?}",
                    row.case
                );
            }
        }
    }
}

/// RULE: the registered URL stays the provider's identity, so registering the SAME issuer
/// twice collides on the issuer the request named — the published rewrite is a property of
/// the report and of nothing else.
#[tokio::test]
async fn the_registered_issuer_is_still_the_providers_identity() {
    let router = served_router();
    let issuer = "https://cognito-idp.us-west-2.amazonaws.com/us-west-2_Z9y8X7w6V";
    let (status, created) =
        call(&router, "CreateOpenIDConnectProvider", &[("Url", issuer)]).await;
    assert_eq!(
        status, 200,
        "the first create must succeed, got {status}: {created}"
    );

    let (status, refused) =
        call(&router, "CreateOpenIDConnectProvider", &[("Url", issuer)]).await;
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
