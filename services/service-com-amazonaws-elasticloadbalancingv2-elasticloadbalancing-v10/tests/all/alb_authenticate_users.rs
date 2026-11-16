// SPDX-License-Identifier: CC0-1.0

//! The USER AUTHENTICATION a listener's rule performs, end to end over the wire: what a
//! client whose user is not authenticated is answered by an Application Load Balancer whose
//! rule carries an `authenticate-cognito` or `authenticate-oidc` action in front of the
//! action it guards.
//!
//! The rule under test: **a rule whose actions begin with a user-authentication action
//! answers a request whose user is not authenticated as that action's
//! `OnUnauthenticatedRequest` states — `deny` with HTTP 401, `allow` by performing the
//! routing action behind it, and `authenticate` (the default) with an HTTP 302 to the
//! identity provider's own authorization endpoint carrying this request's
//! authorization-code request and a nonce cookie — and no cookie a client presents is a
//! session that gets it past that action.**
//!
//! Grounded in the model: `AuthenticateCognitoActionConfig$OnUnauthenticatedRequest` and
//! the identical `AuthenticateOidcActionConfig` member document the three values ("deny -
//! Return an HTTP 401 Unauthorized error", "allow - Allow the request to be forwarded to
//! the target", "authenticate - Redirect the request to the IdP authorization endpoint.
//! This is the default value"); `Action$Order` is "[t]he order for the action ... the action
//! with the lowest value for order is performed first"; `AuthenticateOidcActionConfig$Scope`
//! and its Cognito twin are "[t]he set of user claims to be requested from the IdP. The
//! default is `openid`"; and the endpoint a client is sent to is
//! `AuthenticateOidcActionConfig$AuthorizationEndpoint` outright, or — for a user pool —
//! `UserPoolDomain`, "[t]he domain prefix or fully-qualified domain name of the Amazon
//! Cognito user pool", under the regional name Amazon Cognito gives a prefix
//! (`CreateUserPoolDomain$Domain`: "a prefix of myprefix in the us-east-1 Region results in
//! a domain of myprefix.auth.us-east-1.amazoncognito.com").
//!
//! Every case drives the real wire path: the listener, its default actions and every rule
//! are written through the control plane's own `CreateListener`/`CreateRule`, the
//! certificate the HTTPS listener carries is imported through Certificate Manager's own
//! `ImportCertificate`, and the requests carry no AWS credential — exactly as a browser
//! sends them. Nothing here is served from a record a test invented.
//!
//! The rows span what the rule generalises over: each `OnUnauthenticatedRequest` value and
//! an action stating none, a user pool named by domain prefix and by fully-qualified domain
//! name, a pool in another Region, an OIDC provider whose authorization endpoint carries a
//! query of its own, a stated and an unstated `Scope`, a request carrying a forged session
//! cookie and one carrying none, a host with and without a port, a rule carrying no
//! authentication action at all, and the one authentication action this emulator cannot
//! perform. One further case dials the address the load balancer PUBLISHES over TLS, which
//! is how a browser reaches it.

use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::alb_data_plane::{send, Answered};
use crate::alb_https_listener_tls::{
    client_config, dial, emulator, import, make_active, mint, EXCHANGE_TIMEOUT,
};
use crate::provisioning_loop::{action, call, p};

/// The host the rows below address the load balancer by: a name a client resolves to it,
/// which is the name the callback the identity provider returns to is built from.
const CLIENT_HOST: &str = "shop.example.com";

/// The user pool an `authenticate-cognito` action names, in the Region this suite's calls
/// are signed for (`provisioning_loop::REGION`) — the Region a domain PREFIX resolves under.
const POOL_ARN: &str = "arn:aws:cognito-idp:us-west-2:111111111111:userpool/us-west-2_abc123";

/// A user pool of ANOTHER Region, so the regional name a prefix resolves to is read from
/// the pool's own ARN rather than from the load balancer's Region.
const ELSEWHERE_POOL_ARN: &str =
    "arn:aws:cognito-idp:eu-central-1:111111111111:userpool/eu-central-1_def456";

/// The app client of that pool, which the authorization request asks as.
const POOL_CLIENT_ID: &str = "1h57kf5cpq17m0eml12example";

/// The OAuth 2.0 client an `authenticate-oidc` action asks as instead.
const OIDC_CLIENT_ID: &str = "oidc-client";

/// The domain PREFIX of that pool, and the fully-qualified domain name a second rule names
/// instead.
const POOL_DOMAIN_PREFIX: &str = "sign-in";
const POOL_DOMAIN_NAME: &str = "auth.example.com";

/// The body each rule's routing action answers with, which a client sees only when the
/// authentication action in front of it let the request through.
const PROTECTED_BODY: &str = "protected";

/// The cookie the recorded flow forges: one part of a session an Application Load Balancer
/// would have sealed itself, which nothing here can have issued.
const FORGED_SESSION: &str = "AWSELBAuthSessionCookie-0=forged-by-the-client";

/// The cookie the redirect hands out, whose name is what a client checks for.
const NONCE_COOKIE_PREFIX: &str = "AWSALBAuthNonce";

/// A `fixed-response` action's fields at `index`, which is the routing action every rule
/// below ends with.
fn fixed_response(index: usize, body: &str) -> Vec<(String, String)> {
    vec![
        p(&format!("Actions.member.{index}.Type"), "fixed-response"),
        p(&format!("Actions.member.{index}.Order"), &index.to_string()),
        p(
            &format!("Actions.member.{index}.FixedResponseConfig.StatusCode"),
            "200",
        ),
        p(
            &format!("Actions.member.{index}.FixedResponseConfig.MessageBody"),
            body,
        ),
        p(
            &format!("Actions.member.{index}.FixedResponseConfig.ContentType"),
            "text/plain",
        ),
    ]
}

/// An `authenticate-cognito` action's fields at action order 1: the pool, its app client,
/// the domain the hosted authorization endpoint is served under, and — where the case states
/// them — the scope and the behaviour for an unauthenticated request.
fn authenticate_cognito(
    pool_arn: &str,
    domain: &str,
    scope: Option<&str>,
    on_unauthenticated: Option<&str>,
) -> Vec<(String, String)> {
    let member = "Actions.member.1.AuthenticateCognitoConfig";
    let mut fields = vec![
        p("Actions.member.1.Type", "authenticate-cognito"),
        p("Actions.member.1.Order", "1"),
        p(&format!("{member}.UserPoolArn"), pool_arn),
        p(&format!("{member}.UserPoolClientId"), POOL_CLIENT_ID),
        p(&format!("{member}.UserPoolDomain"), domain),
    ];
    if let Some(scope) = scope {
        fields.push(p(&format!("{member}.Scope"), scope));
    }
    if let Some(behaviour) = on_unauthenticated {
        fields.push(p(&format!("{member}.OnUnauthenticatedRequest"), behaviour));
    }
    fields
}

/// An `authenticate-oidc` action's fields at action order 1, over the four endpoints the
/// model requires of it.
fn authenticate_oidc(authorization_endpoint: &str, on_unauthenticated: Option<&str>) -> Vec<(String, String)> {
    let member = "Actions.member.1.AuthenticateOidcConfig";
    let mut fields = vec![
        p("Actions.member.1.Type", "authenticate-oidc"),
        p("Actions.member.1.Order", "1"),
        p(&format!("{member}.Issuer"), "https://idp.example.com"),
        p(
            &format!("{member}.AuthorizationEndpoint"),
            authorization_endpoint,
        ),
        p(
            &format!("{member}.TokenEndpoint"),
            "https://idp.example.com/oauth2/token",
        ),
        p(
            &format!("{member}.UserInfoEndpoint"),
            "https://idp.example.com/oauth2/userinfo",
        ),
        p(&format!("{member}.ClientId"), OIDC_CLIENT_ID),
        p(&format!("{member}.ClientSecret"), "oidc-secret"),
    ];
    if let Some(behaviour) = on_unauthenticated {
        fields.push(p(&format!("{member}.OnUnauthenticatedRequest"), behaviour));
    }
    fields
}

/// One condition on a path pattern, which every rule below carries so it matches one path.
fn path_pattern(pattern: &str) -> Vec<(String, String)> {
    vec![
        p("Conditions.member.1.Field", "path-pattern"),
        p("Conditions.member.1.Values.member.1", pattern),
    ]
}

/// Create a load balancer and answer its ARN and the address it published.
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
    let dns_name = reply
        .text("DNSName")
        .unwrap_or_else(|| panic!("case create {name}: no DNSName in {}", reply.body));
    (arn, dns_name)
}

/// Create the HTTPS:443 listener whose DEFAULT actions are the authentication action under
/// test followed by the fixed response it guards, and answer its ARN.
async fn create_listener(
    router: &MultiServiceRouter,
    lb_arn: &str,
    certificate_arn: &str,
    default_actions: &[(String, String)],
) -> String {
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", lb_arn));
    form.push(p("Protocol", "HTTPS"));
    form.push(p("Port", "443"));
    form.push(p("Certificates.member.1.CertificateArn", certificate_arn));
    // The listener's default actions are written under the member name `CreateListener`
    // carries them at; a rule's are `Actions`, so the one spelling serves both.
    for (name, value) in default_actions {
        form.push((
            name.replace("Actions.member.", "DefaultActions.member."),
            value.clone(),
        ));
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

/// Add one rule at `priority` carrying `fields`, which must be accepted.
async fn add_rule(
    router: &MultiServiceRouter,
    case: &str,
    listener_arn: &str,
    priority: i32,
    fields: &[(String, String)],
) {
    let mut form = action("CreateRule");
    form.push(p("ListenerArn", listener_arn));
    form.push(p("Priority", &priority.to_string()));
    form.extend(fields.iter().cloned());
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case rule {case}: expected 200, got {} body {}",
        reply.status, reply.body
    );
}

/// One rule the suite writes: the path it matches, and the actions behind that path.
struct Rule {
    case: &'static str,
    priority: i32,
    pattern: &'static str,
    actions: Vec<(String, String)>,
}

/// Every rule the table below is answered from, in the order they are written.
fn rules() -> Vec<Rule> {
    let guarded = |mut authentication: Vec<(String, String)>| {
        authentication.extend(fixed_response(2, PROTECTED_BODY));
        authentication
    };
    vec![
        Rule {
            case: "a user pool named by domain prefix",
            priority: 10,
            pattern: "/hosted*",
            actions: guarded(authenticate_cognito(
                POOL_ARN,
                POOL_DOMAIN_PREFIX,
                Some("openid email"),
                Some("authenticate"),
            )),
        },
        Rule {
            case: "deny",
            priority: 20,
            pattern: "/deny*",
            actions: guarded(authenticate_cognito(
                POOL_ARN,
                POOL_DOMAIN_PREFIX,
                Some("openid email"),
                Some("deny"),
            )),
        },
        Rule {
            case: "allow",
            priority: 30,
            pattern: "/allow*",
            actions: guarded(authenticate_cognito(
                POOL_ARN,
                POOL_DOMAIN_PREFIX,
                Some("openid email"),
                Some("allow"),
            )),
        },
        Rule {
            case: "an action stating no behaviour and no scope",
            priority: 40,
            pattern: "/default*",
            actions: guarded(authenticate_cognito(
                POOL_ARN,
                POOL_DOMAIN_PREFIX,
                None,
                None,
            )),
        },
        Rule {
            case: "a user pool named by fully-qualified domain name",
            priority: 50,
            pattern: "/named*",
            actions: guarded(authenticate_cognito(
                POOL_ARN,
                POOL_DOMAIN_NAME,
                Some("openid"),
                Some("authenticate"),
            )),
        },
        Rule {
            case: "a user pool of another Region",
            priority: 60,
            pattern: "/elsewhere*",
            actions: guarded(authenticate_cognito(
                ELSEWHERE_POOL_ARN,
                POOL_DOMAIN_PREFIX,
                Some("openid"),
                Some("authenticate"),
            )),
        },
        Rule {
            case: "an OIDC provider whose endpoint carries a query of its own",
            priority: 70,
            pattern: "/oidc*",
            actions: guarded(authenticate_oidc(
                "https://idp.example.com/oauth2/v1/authorize?audience=api",
                Some("authenticate"),
            )),
        },
        Rule {
            case: "an OIDC provider that denies",
            // A lower priority value than the rule for `/oidc*` above, whose pattern this
            // path matches too: "rules are evaluated in priority order, from the lowest
            // value to the highest value".
            priority: 65,
            pattern: "/oidc-deny*",
            actions: guarded(authenticate_oidc(
                "https://idp.example.com/oauth2/v1/authorize",
                Some("deny"),
            )),
        },
        Rule {
            case: "an action this emulator cannot perform",
            priority: 90,
            pattern: "/jwt*",
            actions: {
                let member = "Actions.member.1.JwtValidationConfig";
                let mut fields = vec![
                    p("Actions.member.1.Type", "jwt-validation"),
                    p("Actions.member.1.Order", "1"),
                    p(
                        &format!("{member}.JwksEndpoint"),
                        "https://idp.example.com/.well-known/jwks.json",
                    ),
                    p(&format!("{member}.Issuer"), "https://idp.example.com"),
                ];
                fields.extend(fixed_response(2, PROTECTED_BODY));
                fields
            },
        },
        Rule {
            case: "no authentication action at all",
            priority: 100,
            pattern: "/open*",
            actions: fixed_response(1, PROTECTED_BODY),
        },
    ]
}

/// What one request must be answered with.
struct Row {
    case: &'static str,
    /// The request path, under the load balancer's own name.
    path: &'static str,
    /// The host the client addressed, which the callback the redirect names is built from.
    host: &'static str,
    /// The cookie the client carried, where it carried one.
    cookie: Option<&'static str>,
    status: u16,
    /// The authorization endpoint the `Location` must be under, for a row that is
    /// redirected.
    endpoint: Option<&'static str>,
    /// The client the authorization request must ask as, for such a row: a user pool's app
    /// client, or the OAuth 2.0 client an OIDC action names.
    client_id: &'static str,
    /// The scope the authorization request must ask for, for such a row.
    scope: Option<&'static str>,
    /// The callback the authorization request must name, for such a row.
    callback: &'static str,
    /// The body, which only a row the routing action answered carries.
    body: &'static str,
}

/// A row that is redirected to `endpoint`, asking for `scope`.
fn redirected(
    case: &'static str,
    path: &'static str,
    endpoint: &'static str,
    scope: &'static str,
) -> Row {
    Row {
        case,
        path,
        host: CLIENT_HOST,
        cookie: None,
        status: 302,
        endpoint: Some(endpoint),
        client_id: POOL_CLIENT_ID,
        scope: Some(scope),
        callback: "https://shop.example.com/oauth2/idpresponse",
        body: "",
    }
}

/// A row answered with `status` and `body`, carrying no redirect.
fn answered(case: &'static str, path: &'static str, status: u16, body: &'static str) -> Row {
    Row {
        case,
        path,
        host: CLIENT_HOST,
        cookie: None,
        status,
        endpoint: None,
        client_id: "",
        scope: None,
        callback: "",
        body,
    }
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

/// Assert everything one row requires of the answer it got.
fn assert_row(row: &Row, answered: &Answered) {
    assert_eq!(
        answered.status, row.status,
        "case {:?}: expected HTTP {}, got {} body {:?}",
        row.case, row.status, answered.status, answered.body
    );
    assert_eq!(
        answered.body, row.body,
        "case {:?}: expected body {:?}, got {:?}",
        row.case, row.body, answered.body
    );
    let nonce = answered
        .header("set-cookie")
        .filter(|field| field.starts_with(NONCE_COOKIE_PREFIX));
    match row.endpoint {
        Some(endpoint) => {
            let location = answered
                .header("location")
                .unwrap_or_else(|| panic!("case {:?}: the redirect carries no Location", row.case));
            assert!(
                location.starts_with(endpoint),
                "case {:?}: expected a Location under {endpoint:?}, got {location:?}",
                row.case
            );
            let expected: &[(&str, &str)] = &[
                ("client_id", row.client_id),
                ("redirect_uri", row.callback),
                ("response_type", "code"),
                ("scope", row.scope.unwrap_or_default()),
            ];
            for (name, value) in expected {
                let read = parameter(location, name);
                assert_eq!(
                    read.as_deref(),
                    Some(*value),
                    "case {:?}: expected {name}={value:?}, got {read:?}",
                    row.case
                );
            }
            let state = parameter(location, "state").unwrap_or_default();
            assert!(
                !state.is_empty(),
                "case {:?}: expected a non-empty state, got {state:?} in {location:?}",
                row.case
            );
            assert!(
                nonce.is_some(),
                "case {:?}: expected an {NONCE_COOKIE_PREFIX} cookie, got {:?}",
                row.case,
                answered.header("set-cookie")
            );
        }
        None => {
            assert!(
                nonce.is_none(),
                "case {:?}: expected no {NONCE_COOKIE_PREFIX} cookie, got {nonce:?}",
                row.case
            );
            assert!(
                answered.header("location").is_none(),
                "case {:?}: expected no redirect, got {:?}",
                row.case,
                answered.header("location")
            );
        }
    }
}

/// RULE: a rule whose actions begin with a user-authentication action answers a request
/// whose user is not authenticated as that action's `OnUnauthenticatedRequest` states, and
/// the redirect it sends carries this request's own authorization-code request.
#[tokio::test]
async fn an_unauthenticated_request_is_answered_as_on_unauthenticated_request_states() {
    let (router, engine) = emulator();
    let leaf = mint(&[CLIENT_HOST]);
    let certificate_arn = import(&router, "the listener's certificate", &leaf).await;
    let (lb_arn, dns_name) = create_load_balancer(&router, "auth-users-lb").await;
    // The listener's own DEFAULT actions carry the same pair, so a request no rule matches
    // is decided by the authentication action too.
    let mut default_actions = authenticate_cognito(
        POOL_ARN,
        POOL_DOMAIN_PREFIX,
        Some("openid email"),
        Some("authenticate"),
    );
    default_actions.extend(fixed_response(2, PROTECTED_BODY));
    let listener_arn =
        create_listener(&router, &lb_arn, &certificate_arn, &default_actions).await;
    for rule in rules() {
        let mut fields = path_pattern(rule.pattern);
        fields.extend(rule.actions);
        add_rule(&router, rule.case, &listener_arn, rule.priority, &fields).await;
    }
    make_active(&engine, &lb_arn);

    // The authorization endpoints the rows above are sent to, each built from the action
    // alone: a prefix under Amazon Cognito's regional name for it, a fully-qualified
    // domain name as it stands, and an OIDC provider's own URL with its query kept.
    let hosted = "https://sign-in.auth.us-west-2.amazoncognito.com/oauth2/authorize?";
    let named = "https://auth.example.com/oauth2/authorize?";
    let elsewhere = "https://sign-in.auth.eu-central-1.amazoncognito.com/oauth2/authorize?";
    let oidc = "https://idp.example.com/oauth2/v1/authorize?audience=api&";
    let rows: Vec<Row> = vec![
        redirected("the hosted authorization endpoint", "/hosted", hosted, "openid email"),
        // The same request carrying a session cookie it wrote itself: still the redirect,
        // and never the body behind the action.
        Row {
            cookie: Some(FORGED_SESSION),
            ..redirected(
                "a forged session cookie",
                "/hosted",
                hosted,
                "openid email",
            )
        },
        // The scope a configuration states nothing for is the documented default.
        redirected("no scope stated", "/default", hosted, "openid"),
        redirected("a fully-qualified domain name", "/named", named, "openid"),
        redirected("the pool's own Region", "/elsewhere", elsewhere, "openid"),
        Row {
            // An OIDC action asks as the OAuth 2.0 client it names, not as a pool's app
            // client.
            client_id: OIDC_CLIENT_ID,
            ..redirected("an OIDC provider", "/oidc", oidc, "openid")
        },        // The listener's default actions carry the same authentication action.
        redirected(
            "a path no rule matches",
            "/anything-else",
            hosted,
            "openid email",
        ),
        // `deny` is the 401, with no nonce handed out.
        answered("deny", "/deny", 401, ""),
        Row {
            cookie: Some(FORGED_SESSION),
            ..answered("deny with a forged session cookie", "/deny", 401, "")
        },
        answered("an OIDC provider that denies", "/oidc-deny", 401, ""),
        // `allow` performs the action behind it, which is the body a client sees.
        answered("allow", "/allow", 200, PROTECTED_BODY),
        // A rule with no authentication action is unchanged, and the one action this
        // emulator cannot perform is still not bypassed.
        answered("no authentication action", "/open", 200, PROTECTED_BODY),
        answered("an action this emulator cannot perform", "/jwt", 500, ""),
        // The callback names the host the CLIENT addressed, port and all.
        Row {
            host: "shop.example.com:8443",
            callback: "https://shop.example.com:8443/oauth2/idpresponse",
            ..redirected(
                "a host carrying a port",
                "/hosted",
                hosted,
                "openid email",
            )
        },
    ];

    for row in &rows {
        let target = format!("/_aws/elasticloadbalancing/{dns_name}{path}", path = row.path);
        let headers: Vec<(&str, &str)> = row
            .cookie
            .map(|cookie| vec![("cookie", cookie)])
            .unwrap_or_default();
        let answer = send(&router, "GET", row.host, &target, &headers).await;
        assert_row(row, &answer);
    }

    // The configuration every answer above was computed from, read back through the
    // operation that reports it: the action the create persisted, echoed with the pool, its
    // app client and its domain.
    let mut form = action("DescribeRules");
    form.push(p("ListenerArn", &listener_arn));
    let described = call(&router, &form).await;
    for expected in [
        POOL_ARN,
        POOL_CLIENT_ID,
        POOL_DOMAIN_PREFIX,
        POOL_DOMAIN_NAME,
        "authenticate-cognito",
        "deny",
    ] {
        assert!(
            described.body.contains(expected),
            "case DescribeRules: expected {expected:?} in {}",
            described.body
        );
    }
    assert_eq!(
        described.status, 200,
        "case DescribeRules: expected 200, got {} body {}",
        described.status, described.body
    );

    // Two redirects of the same request carry two states and two nonces: neither is stored,
    // so serving one request leaves nothing a later one reads.
    let target = format!("/_aws/elasticloadbalancing/{dns_name}/hosted");
    let first = send(&router, "GET", CLIENT_HOST, &target, &[]).await;
    let second = send(&router, "GET", CLIENT_HOST, &target, &[]).await;
    for (name, read) in [
        (
            "state",
            (
                parameter(first.header("location").unwrap_or_default(), "state"),
                parameter(second.header("location").unwrap_or_default(), "state"),
            ),
        ),
        (
            "nonce",
            (
                first.header("set-cookie").map(ToString::to_string),
                second.header("set-cookie").map(ToString::to_string),
            ),
        ),
    ] {
        assert!(
            read.0.is_some() && read.0 != read.1,
            "case two requests: each must carry its own {name}, got {:?} and {:?}",
            read.0,
            read.1
        );
    }
}

/// RULE (the same answers on the address a browser dials): the published `DNSName` serves
/// these answers over TLS, on the HTTPS listener the certificate belongs to.
#[tokio::test]
async fn the_published_address_answers_an_unauthenticated_request_over_tls() {
    let (router, engine) = emulator();
    let host = "alb-auth.example.com";
    let leaf = mint(&[host]);
    let certificate_arn = import(&router, "the listener's certificate", &leaf).await;
    let (lb_arn, published) = create_load_balancer(&router, "auth-users-tls-lb").await;
    let mut default_actions = authenticate_cognito(
        POOL_ARN,
        POOL_DOMAIN_PREFIX,
        Some("openid email"),
        Some("authenticate"),
    );
    default_actions.extend(fixed_response(2, PROTECTED_BODY));
    let listener_arn =
        create_listener(&router, &lb_arn, &certificate_arn, &default_actions).await;
    let mut deny = path_pattern("/deny*");
    deny.extend(authenticate_cognito(
        POOL_ARN,
        POOL_DOMAIN_PREFIX,
        Some("openid email"),
        Some("deny"),
    ));
    deny.extend(fixed_response(2, PROTECTED_BODY));
    add_rule(&router, "deny", &listener_arn, 1, &deny).await;
    let mut open = path_pattern("/open*");
    open.extend(fixed_response(1, PROTECTED_BODY));
    add_rule(&router, "open", &listener_arn, 2, &open).await;
    make_active(&engine, &lb_arn);

    let cases: &[Dialled] = &[
        Dialled {
            case: "a path with no authentication action",
            path: "/open",
            cookie: None,
            status: "HTTP/1.1 200",
            carries: &[PROTECTED_BODY],
        },
        Dialled {
            case: "an unauthenticated request",
            path: "/",
            cookie: None,
            status: "HTTP/1.1 302",
            carries: &[
                "sign-in.auth.us-west-2.amazoncognito.com/oauth2/authorize?",
                "response_type=code",
                NONCE_COOKIE_PREFIX,
            ],
        },
        Dialled {
            case: "a forged session cookie",
            path: "/",
            cookie: Some(FORGED_SESSION),
            status: "HTTP/1.1 302",
            carries: &["/oauth2/authorize?", NONCE_COOKIE_PREFIX],
        },
        Dialled {
            case: "a denied path",
            path: "/deny",
            cookie: None,
            status: "HTTP/1.1 401",
            carries: &[],
        },
    ];
    for row in cases {
        let answer = tls_exchange(row.case, &published, host, row.path, row.cookie).await;
        assert!(
            answer.starts_with(row.status),
            "case {:?}: expected {:?}, got {answer:?}",
            row.case,
            row.status
        );
        for expected in row.carries {
            assert!(
                answer.contains(expected),
                "case {:?}: expected {expected:?} in the answer, got {answer:?}",
                row.case
            );
        }
        if row.status.ends_with("401") {
            assert!(
                !answer.contains(NONCE_COOKIE_PREFIX),
                "case {:?}: a denied request is handed no nonce, got {answer:?}",
                row.case
            );
        }
    }
}

/// One request dialled over TLS at the published address: what was asked, and what the
/// answer must be.
struct Dialled {
    case: &'static str,
    path: &'static str,
    /// The cookie the client carried, where it carried one.
    cookie: Option<&'static str>,
    /// The status line the answer must open with.
    status: &'static str,
    /// Everything else the answer must carry.
    carries: &'static [&'static str],
}

/// Dial the published address, complete a TLS handshake asking for `host`, send one request
/// for `path` carrying `cookie`, and answer what came back.
async fn tls_exchange(
    case: &str,
    published: &str,
    host: &str,
    path: &str,
    cookie: Option<&str>,
) -> String {
    let conn = dial(published).await;
    let asked_for = rustls::pki_types::ServerName::try_from(host.to_string())
        .unwrap_or_else(|error| panic!("case {case}: {host} is no server name: {error}"));
    let connector = tokio_rustls::TlsConnector::from(client_config());
    let mut stream = tokio::time::timeout(EXCHANGE_TIMEOUT, connector.connect(asked_for, conn))
        .await
        .unwrap_or_else(|_| panic!("case {case}: the handshake did not complete in time"))
        .unwrap_or_else(|error| panic!("case {case}: the handshake must complete, got {error}"));
    let carried = cookie
        .map(|cookie| format!("Cookie: {cookie}\r\n"))
        .unwrap_or_default();
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{carried}Connection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case}: the request was not sent: {error}"));
    let mut answer = Vec::new();
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.read_to_end(&mut answer))
        .await
        .unwrap_or_else(|_| panic!("case {case}: the listener did not answer in time"))
        .unwrap_or_else(|error| panic!("case {case}: the answer was not read: {error}"));
    String::from_utf8_lossy(&answer).to_string()
}
