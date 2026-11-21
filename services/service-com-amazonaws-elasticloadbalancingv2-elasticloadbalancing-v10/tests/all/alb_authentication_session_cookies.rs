// SPDX-License-Identifier: CC0-1.0

//! What a COOKIE the client wrote itself does to the answer a rule's user-authentication
//! action gives, end to end over the wire.
//!
//! The rule under test: **a request whose user is not authenticated is answered as the
//! authentication action's `OnUnauthenticatedRequest` states — HTTP 401 for `deny`, the
//! routing action behind it for `allow`, and an HTTP 302 to the identity provider's
//! authorization endpoint otherwise — whatever cookie the client presents, because the
//! session an Application Load Balancer recognises is one it sealed itself: a cookie under
//! the action's `SessionCookieName`, under that name with the index of one part behind it,
//! under the documented default name, or under any other name is answered exactly as a
//! request carrying none, and never with the body behind the action.**
//!
//! Grounded in the model: `AuthenticateCognitoActionConfig$SessionCookieName` is "[t]he name
//! of the cookie used to maintain session information. The default is
//! AWSELBAuthSessionCookie", and `$OnUnauthenticatedRequest` documents the three behaviours
//! ("deny - Return an HTTP 401 Unauthorized error", "allow - Allow the request to be
//! forwarded to the target", "authenticate - Redirect the request to the IdP authorization
//! endpoint. This is the default value"); `Action$Order` is "[t]he order for the action ...
//! the action with the lowest value for order is performed first", so the authentication
//! action decides the answer before the routing action it guards; and the host a domain
//! PREFIX resolves to is Amazon Cognito's own regional name for it — "a prefix of myprefix
//! in the us-east-1 Region results in a domain of myprefix.auth.us-east-1.amazoncognito.com"
//! (`CreateUserPoolDomain$Domain`) — in the Region the pool's own ARN names.
//!
//! Why a cookie can never be the answer here: what seals a session is the code exchange the
//! load balancer performs against the identity provider's token endpoint, an outbound
//! connection this emulator makes none of, so no value a client presents under that name is
//! a session that verifies — which is the one thing a client must never be able to reach the
//! guarded body with.
//!
//! The rows span what the rule generalises over: the cookie's NAME (the configured one, that
//! name with a part index, the documented default, a name that only starts the same, a name
//! one letter off the default, an unrelated cookie, and no cookie at all), its VALUE
//! (present and empty), how MANY cookies travel with the request, and — across the rules
//! each cookie is sent to — each `OnUnauthenticatedRequest` value and an action stating a
//! session cookie name and one stating none. Every rule is written through the control
//! plane's own `CreateListener`/`CreateRule` and read back through `DescribeRules`, the
//! certificate the HTTPS listener carries is imported through Certificate Manager's own
//! `ImportCertificate`, and every request carries no AWS credential, exactly as a browser
//! sends it.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::{send, Answered};
use crate::alb_https_listener_tls::{emulator, import, make_active, mint};
use crate::provisioning_loop::{action, call, p, REGION};

/// The host the rows address the load balancer by, which is the name the callback the
/// authorization request carries is built from.
const CLIENT_HOST: &str = "shop.example.com";

/// The user pool the `authenticate-cognito` action names, in the Region this suite's calls
/// are signed for — the Region a domain PREFIX resolves under.
const POOL_ARN: &str = "arn:aws:cognito-idp:us-west-2:111111111111:userpool/us-west-2_abc123";

/// The app client the authorization request asks as.
const POOL_CLIENT_ID: &str = "7example0client0id0000000a";

/// The domain PREFIX of that pool, under which Amazon Cognito serves its hosted UI.
const POOL_DOMAIN_PREFIX: &str = "shop-sign-in";

/// The cookie name the rules below STATE, rather than leaving the action at the model's
/// default.
const STATED_SESSION_COOKIE: &str = "ShopSession";

/// The name `SessionCookieName` documents as its default: "The default is
/// AWSELBAuthSessionCookie".
const DEFAULT_SESSION_COOKIE: &str = "AWSELBAuthSessionCookie";

/// The cookie the redirect hands out, whose name is what a client checks for.
const NONCE_COOKIE_PREFIX: &str = "AWSALBAuthNonce";

/// The body the routing action behind each authentication action answers with, which a
/// client sees only when the action in front of it let the request through.
const PROTECTED_BODY: &str = "protected";

/// The body the listener's own default actions answer with, so a request no rule matches is
/// told apart from one a rule answered.
const UNMATCHED_BODY: &str = "unmatched";

/// A `fixed-response` action's fields at `index` answering `body`.
fn fixed_response(index: usize, body: &str) -> Vec<(String, String)> {
    vec![
        p(&format!("Actions.member.{index}.Type"), "fixed-response"),
        p(&format!("Actions.member.{index}.Order"), &index.to_string()),
        p(
            &format!("Actions.member.{index}.FixedResponseConfig.StatusCode"),
            "200",
        ),
        p(
            &format!("Actions.member.{index}.FixedResponseConfig.ContentType"),
            "text/plain",
        ),
        p(
            &format!("Actions.member.{index}.FixedResponseConfig.MessageBody"),
            body,
        ),
    ]
}

/// An `authenticate-cognito` action's fields at action order 1: the pool, its app client and
/// its hosted-UI domain, the behaviour for an unauthenticated request, and — where the rule
/// states them — the scope and the cookie its session would travel in.
fn authenticate_cognito(
    on_unauthenticated: &str,
    scope: Option<&str>,
    session_cookie_name: Option<&str>,
) -> Vec<(String, String)> {
    let member = "Actions.member.1.AuthenticateCognitoConfig";
    let mut fields = vec![
        p("Actions.member.1.Type", "authenticate-cognito"),
        p("Actions.member.1.Order", "1"),
        p(&format!("{member}.UserPoolArn"), POOL_ARN),
        p(&format!("{member}.UserPoolClientId"), POOL_CLIENT_ID),
        p(&format!("{member}.UserPoolDomain"), POOL_DOMAIN_PREFIX),
        p(
            &format!("{member}.OnUnauthenticatedRequest"),
            on_unauthenticated,
        ),
    ];
    if let Some(scope) = scope {
        fields.push(p(&format!("{member}.Scope"), scope));
    }
    if let Some(name) = session_cookie_name {
        fields.push(p(&format!("{member}.SessionCookieName"), name));
    }
    fields
}

/// One rule the suite writes: the path it matches, the behaviour its authentication action
/// states, and what every request to it must be answered with.
struct Guard {
    case: &'static str,
    priority: i32,
    path: &'static str,
    on_unauthenticated: &'static str,
    /// The `Scope` the action states, and `None` for a rule stating none — whose
    /// authorization request asks for the documented default instead.
    scope: Option<&'static str>,
    /// The `SessionCookieName` the action states, and `None` for a rule leaving it at the
    /// documented default.
    session_cookie_name: Option<&'static str>,
    status: u16,
    /// The scope the authorization request must ask for, for a rule that redirects.
    asked_scope: Option<&'static str>,
    /// The body, which only a request the routing action answered carries.
    body: &'static str,
}

/// The rules every cookie below is sent to: each `OnUnauthenticatedRequest` value, over an
/// action that states the cookie its session travels in and one that leaves it at the
/// default.
fn guards() -> Vec<Guard> {
    vec![
        Guard {
            case: "authenticate, with a stated session cookie name",
            priority: 10,
            path: "/named",
            on_unauthenticated: "authenticate",
            scope: Some("openid email"),
            session_cookie_name: Some(STATED_SESSION_COOKIE),
            status: 302,
            asked_scope: Some("openid email"),
            body: "",
        },
        Guard {
            case: "authenticate, with no session cookie name stated",
            priority: 20,
            path: "/default",
            on_unauthenticated: "authenticate",
            scope: None,
            session_cookie_name: None,
            status: 302,
            asked_scope: Some("openid"),
            body: "",
        },
        Guard {
            case: "deny",
            priority: 30,
            path: "/deny",
            on_unauthenticated: "deny",
            scope: Some("openid"),
            session_cookie_name: Some(STATED_SESSION_COOKIE),
            status: 401,
            asked_scope: None,
            body: "",
        },
        Guard {
            case: "allow",
            priority: 40,
            path: "/allow",
            on_unauthenticated: "allow",
            scope: Some("openid"),
            session_cookie_name: Some(STATED_SESSION_COOKIE),
            status: 200,
            asked_scope: None,
            body: PROTECTED_BODY,
        },
    ]
}

/// One cookie header a client sends, and `None` for a client sending none.
struct Carried {
    case: &'static str,
    cookie: Option<String>,
}

/// Every cookie the rows send: the configured name whole and split into parts, the documented
/// default name, names that only resemble either, an empty value, a request carrying many
/// cookies at once, and no cookie at all.
fn carried() -> Vec<Carried> {
    let many = (0..24)
        .map(|index| format!("crumb{index}=value{index}"))
        .chain(std::iter::once(format!(
            "{STATED_SESSION_COOKIE}-0=forged-by-the-client"
        )))
        .collect::<Vec<String>>()
        .join("; ");
    vec![
        Carried {
            case: "no cookie at all",
            cookie: None,
        },
        Carried {
            case: "the configured session cookie, whole",
            cookie: Some(format!("{STATED_SESSION_COOKIE}=forged-by-the-client")),
        },
        Carried {
            case: "the configured session cookie, split into parts",
            cookie: Some(format!(
                "{STATED_SESSION_COOKIE}-0=forged; {STATED_SESSION_COOKIE}-1=more"
            )),
        },
        Carried {
            case: "the configured session cookie with an empty value",
            cookie: Some(format!("{STATED_SESSION_COOKIE}=")),
        },
        Carried {
            case: "the default session cookie name's first part",
            cookie: Some(format!("{DEFAULT_SESSION_COOKIE}-0=forged")),
        },
        Carried {
            case: "a name one letter off the default",
            cookie: Some("AWSALBAuthSessionCookie-0=forged".to_string()),
        },
        Carried {
            case: "names that only start the same",
            cookie: Some(format!(
                "{STATED_SESSION_COOKIE}X=no; {DEFAULT_SESSION_COOKIE}X=no"
            )),
        },
        Carried {
            case: "a cookie of no interest to the action",
            cookie: Some("AWSALB=stickiness".to_string()),
        },
        Carried {
            case: "many cookies, the configured one among them",
            cookie: Some(many),
        },
    ]
}

/// The value of one query parameter of a URL, read as a client reads it.
fn parameter(location: &str, name: &str) -> Option<String> {
    let query = location.split_once('?').map(|(_, query)| query)?;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(held, _)| *held == name)
        .map(|(_, value)| {
            percent_encoding::percent_decode_str(&value.replace('+', " "))
                .decode_utf8_lossy()
                .into_owned()
        })
}

/// The `Set-Cookie` field carrying a nonce, where the answer handed one out.
fn nonce(answered: &Answered) -> Option<&str> {
    answered
        .header("set-cookie")
        .filter(|field| field.starts_with(NONCE_COOKIE_PREFIX))
}

/// Create the load balancer every rule below belongs to, and answer its ARN and the address
/// it published.
async fn create_load_balancer(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let mut form = action("CreateLoadBalancer");
    form.push(p("Name", name));
    form.push(p("Subnets.member.1", "subnet-11111111"));
    form.push(p("Subnets.member.2", "subnet-22222222"));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case create load balancer {name}: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let arn = reply
        .text("LoadBalancerArn")
        .unwrap_or_else(|| panic!("case create {name}: no LoadBalancerArn in {}", reply.body));
    let published = reply
        .text("DNSName")
        .unwrap_or_else(|| panic!("case create {name}: no DNSName in {}", reply.body));
    (arn, published)
}

/// Create the HTTPS:443 listener the rules hang off, whose own default actions answer a
/// request no rule matched, and answer its ARN.
async fn create_listener(
    router: &MultiServiceRouter,
    lb_arn: &str,
    certificate_arn: &str,
) -> String {
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", lb_arn));
    form.push(p("Protocol", "HTTPS"));
    form.push(p("Port", "443"));
    form.push(p("Certificates.member.1.CertificateArn", certificate_arn));
    for (name, value) in fixed_response(1, UNMATCHED_BODY) {
        form.push((name.replace("Actions.member.", "DefaultActions.member."), value));
    }
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case create listener: expected 200, got {} body {}",
        reply.status, reply.body
    );
    reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("case create listener: no ListenerArn in {}", reply.body))
}

/// Write one guard as a rule of that listener.
async fn create_rule(router: &MultiServiceRouter, listener_arn: &str, guard: &Guard) {
    let mut form = action("CreateRule");
    form.push(p("ListenerArn", listener_arn));
    form.push(p("Priority", &guard.priority.to_string()));
    form.push(p("Conditions.member.1.Field", "path-pattern"));
    form.push(p(
        "Conditions.member.1.Values.member.1",
        &format!("{path}*", path = guard.path),
    ));
    form.extend(authenticate_cognito(
        guard.on_unauthenticated,
        guard.scope,
        guard.session_cookie_name,
    ));
    form.extend(fixed_response(2, PROTECTED_BODY));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case rule {}: expected 200, got {} body {}",
        guard.case, reply.status, reply.body
    );
}

/// RULE: no cookie a client presents is a session this load balancer sealed, so every
/// request a user-authentication action guards is answered as its `OnUnauthenticatedRequest`
/// states whatever cookie travelled with it, and never with the body behind the action.
#[tokio::test]
async fn no_cookie_a_client_presents_gets_past_the_authentication_action() {
    let (router, engine) = emulator();
    let leaf = mint(&[CLIENT_HOST]);
    let certificate_arn = import(&router, "the listener's certificate", &leaf).await;
    let (lb_arn, published) = create_load_balancer(&router, "auth-cookies-lb").await;
    let listener_arn = create_listener(&router, &lb_arn, &certificate_arn).await;
    let guards = guards();
    for guard in &guards {
        create_rule(&router, &listener_arn, guard).await;
    }
    make_active(&engine, &lb_arn);

    // The authorization endpoint every redirect must be under: the pool's domain prefix
    // beneath Amazon Cognito's own regional name for it, in the Region the pool's ARN names.
    let hosted = format!(
        "https://{POOL_DOMAIN_PREFIX}.auth.{REGION}.amazoncognito.com/oauth2/authorize?"
    );
    let callback = format!("https://{CLIENT_HOST}/oauth2/idpresponse");
    // Every opaque value handed out, so a cookie presented twice is shown to establish
    // nothing that a later request is answered from.
    let mut opaque: Vec<String> = Vec::new();

    for guard in &guards {
        for carried in carried() {
            let case = format!("{} / {}", guard.case, carried.case);
            let target = format!(
                "/_aws/elasticloadbalancing/{published}{path}",
                path = guard.path
            );
            let headers: Vec<(&str, &str)> = carried
                .cookie
                .as_deref()
                .map(|cookie| vec![("cookie", cookie)])
                .unwrap_or_default();
            let answered = send(&router, "GET", CLIENT_HOST, &target, &headers).await;
            assert_eq!(
                answered.status, guard.status,
                "case {case:?}: expected HTTP {}, got {} body {:?}",
                guard.status, answered.status, answered.body
            );
            assert_eq!(
                answered.body, guard.body,
                "case {case:?}: expected body {:?}, got {:?}",
                guard.body, answered.body
            );
            let Some(asked_scope) = guard.asked_scope else {
                // A request the action answered itself, and one it let through: neither is
                // handed a nonce, because neither is one leg of a sign-in.
                assert!(
                    nonce(&answered).is_none(),
                    "case {case:?}: expected no {NONCE_COOKIE_PREFIX} cookie, got {:?}",
                    answered.header("set-cookie")
                );
                assert!(
                    answered.header("location").is_none(),
                    "case {case:?}: expected no redirect, got {:?}",
                    answered.header("location")
                );
                continue;
            };
            let location = answered
                .header("location")
                .unwrap_or_else(|| panic!("case {case:?}: the redirect carries no Location"));
            assert!(
                location.starts_with(&hosted),
                "case {case:?}: expected a Location under {hosted:?}, got {location:?}"
            );
            for (name, expected) in [
                ("client_id", POOL_CLIENT_ID),
                ("redirect_uri", callback.as_str()),
                ("response_type", "code"),
                ("scope", asked_scope),
            ] {
                let read = parameter(location, name);
                assert_eq!(
                    read.as_deref(),
                    Some(expected),
                    "case {case:?}: expected {name}={expected:?}, got {read:?}"
                );
            }
            let state = parameter(location, "state").unwrap_or_default();
            assert!(
                !state.is_empty(),
                "case {case:?}: expected a non-empty state, got {state:?} in {location:?}"
            );
            let handed = nonce(&answered)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected an {NONCE_COOKIE_PREFIX} cookie, got {:?}",
                        answered.header("set-cookie")
                    )
                })
                .to_string();
            opaque.push(state);
            opaque.push(handed);
        }
    }

    // Each redirect carried its own state and its own nonce: a cookie a client presented
    // established nothing, so nothing a later request is answered from was stored.
    let distinct: BTreeSet<&String> = opaque.iter().collect();
    assert_eq!(
        distinct.len(),
        opaque.len(),
        "case \"every redirect\": expected {} distinct state and nonce values, got {}",
        opaque.len(),
        distinct.len()
    );

    // The configuration the answers were computed from, read back through the operation that
    // reports it: the cookie name a rule stated is the action's own, not a value the data
    // plane invented.
    let mut form = action("DescribeRules");
    form.push(p("ListenerArn", &listener_arn));
    let described = call(&router, &form).await;
    assert_eq!(
        described.status, 200,
        "case DescribeRules: expected 200, got {} body {}",
        described.status, described.body
    );
    for expected in [
        STATED_SESSION_COOKIE,
        POOL_CLIENT_ID,
        POOL_DOMAIN_PREFIX,
        "authenticate-cognito",
    ] {
        assert!(
            described.body.contains(expected),
            "case DescribeRules: expected {expected:?} in {}",
            described.body
        );
    }
}
