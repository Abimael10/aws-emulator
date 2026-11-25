// SPDX-License-Identifier: CC0-1.0

//! The ISSUER an identity pool's `Logins` entry names a user pool by, across the
//! cognito-identity / cognito-idp boundary.
//!
//! Two services and one engine, so this is a router suite: the token is minted by the Amazon
//! Cognito user pools service (signing name `cognito-idp`), the exchange that redeems it is
//! Amazon Cognito Identity's (`cognito-identity`), and the session it vends is assumed
//! through IAM and STS.
//!
//! The RULE the rows assert, one level above the recorded case:
//!
//! > A user pool publishes its token issuer at an address this emulator answers on — the
//! > `host[:port]` the create reached, with the pool's id as the whole path — and that issuer
//! > is the value an identity pool matches a `Logins` provider name against: the announced
//! > issuer without its scheme names the pool, and so does the documented
//! > `cognito-idp.<region>.<domain>/<UserPoolId>` form of the same pool, which is what a
//! > client configured from AWS's own documentation is holding; a name carrying another
//! > Region names no pool of this one.
//!
//! The `Logins` key is "`cognito-idp.<region>.<domain>/<YOUR_USER_POOL_ID>`" (`GetId.Logins`
//! in the cognito-identity model), which is the pool's issuer without its scheme; the issuer
//! itself is token content this emulator's models do not describe, and
//! `IssuerConfigurationType.Type`
//! (`smithy_query(query="shape",shape="IssuerConfigurationType")`) is the only place a
//! Cognito model states one.
//!
//! The rows span what the rule generalises over: the spelling the identity pool is
//! configured with and the `Logins` entry is keyed by (the issuer the pool announced for
//! this emulator's endpoint, and the documented `amazonaws.com` spelling of the same pool's
//! issuer), and a name that must NOT resolve (the documented spelling of another Region).
//! Each admitted row goes the whole way: `GetId`, then `GetCredentialsForIdentity`, then the
//! vended session's own `sts:GetCallerIdentity`.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason `boot_serve.rs` does.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::json;
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

// The fixture this suite shares with the sibling that pins role mapping: the wire helpers,
// the role creation and the JOSE reading are that suite's `pub(crate)` ones rather than a
// second copy here.
use crate::cognito_group_role_mapping::{
    caller_arn, claims_of, create_role, ok, rpc, Answer, BOOT_STACK_SIZE, GOOD_PASSWORD, REGION,
};

/// The base URL this emulator is SERVED at in this suite — the data port a client of the
/// emulator is configured with, which is the deployment every client of it meets.
const SERVED_BASE: &str = "https://127.0.0.1:9297";

/// The user the pool signs in.
const USERNAME: &str = "logins-user";

/// The path under an issuer its OpenID Connect discovery document is published at (OpenID
/// Connect Discovery 1.0 §4).
const OPENID_CONFIGURATION: &str = "/.well-known/openid-configuration";

/// The status and error code the exchange refuses a login whose token names an issuer the
/// provider name does not with. The sentence that travels with it is pinned by the
/// cognito-identity crate's own suites; what a row here asserts is the class.
const ISSUER_MISMATCH_STATUS: u16 = 400;
const NOT_AUTHORIZED: &str = "NotAuthorizedException";

/// A Region no pool of this suite lives in, to pin that a provider name's Region label is
/// read rather than ignored.
const FOREIGN_REGION: &str = "ap-northeast-3";

/// An ANONYMOUS GET of `url`, addressed at the authority the URL itself names — which is
/// what a relying party holding a token does with the locator its `iss` claim is.
fn fetch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    url: &str,
) -> Answer {
    let rest = url
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("case {case:?}: {url:?} must be an https URL"));
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{authority}{path}"))
        .header("host", authority)
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body,
    }
}

/// One row: the provider name the identity pool is configured with and the `Logins` entry is
/// keyed by, and whether the pool's own token is admitted under it.
struct Row {
    case: &'static str,
    /// How the row spells the user pool's issuer, as a `Logins` provider name (no scheme).
    spelling: Spelling,
    admitted: bool,
}

/// Which spelling of a user-pool issuer a row uses for its provider name.
enum Spelling {
    /// The issuer the pool ANNOUNCED, as its tokens carry it, without its scheme.
    Announced,
    /// The documented `cognito-idp.<region>.amazonaws.com/<id>` spelling of the same pool's
    /// issuer, in the Region the pool lives in.
    Documented,
    /// The documented spelling of ANOTHER Region, which names no pool of this one.
    DocumentedElsewhere,
}

const ROWS: &[Row] = &[
    Row {
        case: "the issuer the pool announced for this emulator's endpoint",
        spelling: Spelling::Announced,
        admitted: true,
    },
    Row {
        case: "the documented spelling of the same pool's issuer",
        spelling: Spelling::Documented,
        admitted: true,
    },
    Row {
        case: "the documented spelling of another Region, which names no pool of this one",
        spelling: Spelling::DocumentedElsewhere,
        admitted: false,
    },
];

#[test]
fn an_identity_pool_admits_the_login_its_user_pools_issuer_names() {
    std::thread::Builder::new()
        .name("cognito-identity-announced-issuer".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            // Signature verification belongs to the emulator, not to this check.
            router.set_request_authentication(false);
            // This emulator IS served on a port, so it has an endpoint of its own whose zone
            // and port every pool created through it publishes its issuer in.
            router.set_url_base(
                EndpointUrl::parse(SERVED_BASE).expect("the suite's served base must parse"),
            );
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");
            let idp = "AWSCognitoIdentityProviderService";
            let identity = "AWSCognitoIdentityService";

            // ---- the user pool, its app client and one confirmed user ----
            let user_pool_id = ok(
                &router,
                &runtime,
                Some("cognito-idp"),
                &format!("{idp}.CreateUserPool"),
                &json!({ "PoolName": "announced-issuer-pool" }),
            )["UserPool"]["Id"]
                .as_str()
                .expect("CreateUserPool reports the pool's Id")
                .to_string();
            let client_id = ok(
                &router,
                &runtime,
                Some("cognito-idp"),
                &format!("{idp}.CreateUserPoolClient"),
                &json!({
                    "UserPoolId": &user_pool_id,
                    "ClientName": "announced-issuer-app",
                    "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH"],
                }),
            )["UserPoolClient"]["ClientId"]
                .as_str()
                .expect("CreateUserPoolClient reports the client's ClientId")
                .to_string();
            ok(
                &router,
                &runtime,
                Some("cognito-idp"),
                &format!("{idp}.AdminCreateUser"),
                &json!({
                    "UserPoolId": &user_pool_id,
                    "Username": USERNAME,
                    "MessageAction": "SUPPRESS",
                }),
            );
            ok(
                &router,
                &runtime,
                Some("cognito-idp"),
                &format!("{idp}.AdminSetUserPassword"),
                &json!({
                    "UserPoolId": &user_pool_id,
                    "Username": USERNAME,
                    "Password": GOOD_PASSWORD,
                    "Permanent": true,
                }),
            );

            // ---- the token, and the issuer it announces ----
            let id_token = ok(
                &router,
                &runtime,
                None,
                &format!("{idp}.InitiateAuth"),
                &json!({
                    "ClientId": &client_id,
                    "AuthFlow": "USER_PASSWORD_AUTH",
                    "AuthParameters": { "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD },
                }),
            )["AuthenticationResult"]["IdToken"]
                .as_str()
                .expect("the sign-in must issue an IdToken")
                .to_string();
            let announced = claims_of(&id_token)["iss"]
                .as_str()
                .expect("an id token carries a string iss claim")
                .to_string();

            // The issuer the pool announced is the address this emulator is served at, with
            // the pool's id as its whole path — a locator this emulator answers, which is what
            // makes it a value a relying party can use.
            let expected = format!("https://127.0.0.1:9297/{user_pool_id}");
            assert_eq!(
                announced, expected,
                "the pool must announce iss {expected:?}, got {announced:?}"
            );
            let discovery = fetch(
                &router,
                &runtime,
                "the announced issuer",
                &format!("{announced}{OPENID_CONFIGURATION}"),
            );
            assert_eq!(
                discovery.status,
                200,
                "the discovery document under the announced issuer must be served, got {} with \
                 {:?}",
                discovery.status,
                discovery.text()
            );
            assert_eq!(
                discovery.json()["issuer"].as_str(),
                Some(announced.as_str()),
                "the discovery document must state the announced issuer, got {:?}",
                discovery.text()
            );

            // ---- one identity pool per spelling, each over this user pool ----
            for (index, row) in ROWS.iter().enumerate() {
                let case = row.case;
                let provider_name = match row.spelling {
                    Spelling::Announced => announced
                        .strip_prefix("https://")
                        .expect("the announced issuer is an https URL")
                        .to_string(),
                    Spelling::Documented => {
                        format!("cognito-idp.{REGION}.amazonaws.com/{user_pool_id}")
                    }
                    Spelling::DocumentedElsewhere => {
                        format!("cognito-idp.{FOREIGN_REGION}.amazonaws.com/{user_pool_id}")
                    }
                };
                let identity_pool_id = ok(
                    &router,
                    &runtime,
                    Some("cognito-identity"),
                    &format!("{identity}.CreateIdentityPool"),
                    &json!({
                        "IdentityPoolName": format!("announced-issuer-identities-{index}"),
                        "AllowUnauthenticatedIdentities": false,
                        "CognitoIdentityProviders": [
                            { "ProviderName": &provider_name, "ClientId": &client_id },
                        ],
                    }),
                )["IdentityPoolId"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: CreateIdentityPool reports an IdentityPoolId")
                    })
                    .to_string();
                let authenticated = create_role(
                    &router,
                    &runtime,
                    &format!("announced-issuer-auth-{index}"),
                    &identity_pool_id,
                );
                ok(
                    &router,
                    &runtime,
                    Some("cognito-identity"),
                    &format!("{identity}.SetIdentityPoolRoles"),
                    &json!({
                        "IdentityPoolId": &identity_pool_id,
                        "Roles": { "authenticated": &authenticated },
                    }),
                );

                // ---- the exchange: an UNSIGNED GetId, then the vend ----
                let logins = json!({ &provider_name: &id_token });
                let got_id = rpc(
                    &router,
                    &runtime,
                    None,
                    &format!("{identity}.GetId"),
                    &json!({ "IdentityPoolId": &identity_pool_id, "Logins": &logins }),
                );
                if !row.admitted {
                    assert_eq!(
                        got_id.status,
                        ISSUER_MISMATCH_STATUS,
                        "case {case:?}: a login the pool's issuer does not name must be \
                         refused, got {} with {:?}",
                        got_id.status,
                        got_id.text()
                    );
                    let refusal = got_id.text();
                    assert!(
                        refusal.contains(NOT_AUTHORIZED),
                        "case {case:?}: expected a {NOT_AUTHORIZED}, got {refusal:?}"
                    );
                    continue;
                }
                assert_eq!(
                    got_id.status,
                    200,
                    "case {case:?}: GetId under a name the pool's issuer names must be served, \
                     got {} with {:?}",
                    got_id.status,
                    got_id.text()
                );
                let identity_id = got_id.json()["IdentityId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: GetId reports an IdentityId"))
                    .to_string();
                let vended = rpc(
                    &router,
                    &runtime,
                    None,
                    &format!("{identity}.GetCredentialsForIdentity"),
                    &json!({ "IdentityId": &identity_id, "Logins": &logins }),
                );
                assert_eq!(
                    vended.status,
                    200,
                    "case {case:?}: the vend must be served, got {} with {:?}",
                    vended.status,
                    vended.text()
                );
                let body = vended.json();
                let access_key_id = body["Credentials"]["AccessKeyId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    !access_key_id.is_empty(),
                    "case {case:?}: the vend must carry an AccessKeyId, got {body}"
                );

                // The vended session is the identity pool's authenticated role, which is
                // what makes the admission a REAL exchange rather than a reported one.
                let arn = caller_arn(&router, &runtime, case, &access_key_id);
                assert!(
                    arn.contains(&format!("announced-issuer-auth-{index}")),
                    "case {case:?}: the vended session must be the pool's authenticated \
                     role, got {arn:?}"
                );
            }
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}
