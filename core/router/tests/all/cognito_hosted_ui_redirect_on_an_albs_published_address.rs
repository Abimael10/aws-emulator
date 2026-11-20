// SPDX-License-Identifier: CC0-1.0

//! What a browser that DIALS the address an Application Load Balancer publishes is answered
//! with when the listener it reaches guards its routing action with an `authenticate-cognito`
//! action naming a user pool, a hosted-UI domain and an app client Amazon Cognito itself
//! holds — on the emulator as it is ASSEMBLED.
//!
//! RULE: **the address a load balancer publishes as its `DNSName` is an address this emulator
//! answers on, so a client that honours that name reaches the listener — and the listener's
//! `authenticate-cognito` action answers a request whose user is not authenticated as its
//! `OnUnauthenticatedRequest` states: an HTTP 302 to the authorization endpoint of the user
//! pool's own hosted-UI domain, asking as the pool's app client with the authorization-code
//! response type and a callback on the very address the client dialled.**
//!
//! Grounded in the models: `LoadBalancer$DNSName` is "the public DNS name of the load
//! balancer" — the name a client resolves and sends its traffic to, which
//! `CreateLoadBalancer` and `DescribeLoadBalancers` both publish — and
//! `CreateLoadBalancerInput$Scheme` documents that the nodes of an `internet-facing` load
//! balancer "have public IP addresses", so that is the family and scheme a client on this
//! machine addresses directly. `AuthenticateCognitoActionConfig$OnUnauthenticatedRequest`
//! documents the three behaviours ("deny - Return an HTTP 401 Unauthorized error", "allow -
//! Allow the request to be forwarded to the target", "authenticate - Redirect the request to
//! the IdP authorization endpoint. This is the default value") and `$Scope` "[t]he set of
//! user claims to be requested from the IdP. The default is `openid`"; the host a domain
//! PREFIX resolves to is Amazon Cognito's own regional name for it — "a prefix value of
//! myprefix for a user pool in the us-east-1 Region results in a domain of
//! myprefix.auth.us-east-1.amazoncognito.com" (`CreateUserPoolDomain$Domain`) — in the Region
//! the pool's own ARN names.
//!
//! Why this suite lives on the assembled router rather than in either crate: the pool, its
//! hosted-UI domain and its app clients are Amazon Cognito's resources, created through that
//! service's own API, while the socket the published address is served on belongs to the load
//! balancer — and the two only exist together when one emulator mounts both. Every value an
//! assertion compares against comes from a service's own answer: the app client id and the
//! domain prefix from Amazon Cognito, the Region from the pool ARN Amazon Cognito minted, the
//! address from the load balancer. Every request here is a REAL TLS connection to the
//! published authority, carrying no AWS credential, exactly as a browser sends it.
//!
//! The rows differ along what the rule generalises over: each `OnUnauthenticatedRequest`
//! value and an action stating none, a stated and an unstated `Scope`, two app clients of one
//! pool, and a pool in another Region (whose domain prefix therefore resolves under that
//! Region). Every row names its pool's domain by PREFIX, because this service's
//! `CreateUserPoolDomain$Domain` pattern (`^[a-z0-9](?:[a-z0-9\-]{0,61}[a-z0-9])?$`) admits
//! no dotted name, so a fully-qualified user pool domain is no resource Amazon Cognito holds
//! here; the ELBv2 crate's own `alb_authenticate_users` suite covers that spelling of the
//! member.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element reader this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, form_encode};
// The assembled emulator, the EC2 network an internet-facing load balancer needs and the
// published-address assertion, from the suite that made them `pub(crate)` for exactly this —
// a client DIALLING the address a load balancer published
// (`elbv2_published_address_enforces_its_web_acl`). Their own calls are signed for the Region
// below, which is the Region this suite signs for too.
use crate::elbv2_published_address_enforces_its_web_acl::{build_router, dialable_port, network};

/// The Region the network, the load balancer, the certificate and the first user pool belong
/// to — the Region the shared helpers above sign their own calls for.
const REGION: &str = "us-east-1";

/// The Region the LAST pool is created in, so the host its domain prefix resolves to is read
/// from the pool's own ARN rather than from the load balancer's Region.
const OTHER_REGION: &str = "eu-central-1";

/// The API version the ELBv2 query protocol carries.
const ELB_VERSION: &str = "2015-12-01";

/// The body the routing action behind each authentication action answers with, which a client
/// sees only when the action in front of it let the request through.
const PROTECTED_BODY: &str = "protected";

/// The path on the load balancer's own name the authorization answer comes back to, which is
/// the callback an app client registers.
const CALLBACK_PATH: &str = "/oauth2/idpresponse";

/// The path a user pool's domain serves its authorization endpoint at.
const AUTHORIZE_PATH: &str = "/oauth2/authorize";

/// The name of the cookie the redirect hands out, which is what ties the answer the identity
/// provider sends to the request that was redirected.
const NONCE_COOKIE: &str = "AWSALBAuthNonce";

/// How long one dial of the published address waits for its answer, and how long a wait for
/// work the clock owes may take before it is a failure, with how long between two looks.
/// Generous on purpose: nothing here measures a wait, only what came back — so a loaded host
/// is slow rather than failing.
const DIAL_DEADLINE: Duration = Duration::from_secs(60);
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// A SigV4 `Authorization` whose credential scope names `service` in `region`, which is what
/// routes the request.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One form-encoded (query-protocol) call of `service`, as its status and its XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service, REGION))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One member of a form body.
fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One ELBv2 call that must succeed. The credential scope names THIS model's own routing key,
/// which is how an ELBv2 request is addressed at the model rather than at the Classic one it
/// shares an endpoint with.
async fn elbv2(router: &MultiServiceRouter, action: &str, members: &[(String, String)]) -> String {
    let (status, body) = query(
        router,
        "elasticloadbalancingv2",
        ELB_VERSION,
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: ELBv2 {action} must answer 200, got {status} with body {body}"
    );
    body
}

/// One `awsJson1_1` call that must succeed, against the service `host` and `target` name, in
/// `region`.
async fn json_call(
    router: &MultiServiceRouter,
    service: &str,
    region: &str,
    target: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target}.{operation}"))
        .header("authorization", authorization(service, region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "setup: {service} {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon Cognito user pools call that must succeed, in `region`.
async fn cognito(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    json_call(
        router,
        "cognito-idp",
        region,
        "AWSCognitoIdentityProviderService",
        operation,
        body,
    )
    .await
}

/// One Certificate Manager call that must succeed.
async fn acm(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    json_call(
        router,
        "acm",
        REGION,
        "CertificateManager",
        operation,
        body,
    )
    .await
}

/// A required, non-empty string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{what} must carry a non-empty {member:?}, got {body}"))
}

/// What one user pool of this suite contributes to the authorization request: the ARN the
/// action names it by, the hosted-UI domain prefix Amazon Cognito holds for it, and the app
/// client the request asks as.
struct PoolClient {
    /// The case this pool and client stand for, quoted by every assertion about them.
    case: &'static str,
    pool_arn: String,
    domain: String,
    client_id: String,
}

impl PoolClient {
    /// The host the authorization endpoint of this pool's hosted-UI domain is served at: the
    /// prefix Amazon Cognito holds under its own regional name for a domain, in the Region the
    /// pool's ARN names (`CreateUserPoolDomain$Domain`).
    fn hosted_ui_host(&self) -> String {
        format!(
            "{domain}.auth.{region}.amazoncognito.com",
            domain = self.domain,
            region = region_of(&self.pool_arn)
        )
    }
}

/// The Region an ARN names, which is its fourth colon-separated field.
fn region_of(arn: &str) -> String {
    arn.split(':')
        .nth(3)
        .unwrap_or_else(|| panic!("setup: {arn} is no ARN, so it names no Region"))
        .to_string()
}

/// Create one user pool in `region` with a hosted-UI domain under `prefix` and one app client
/// per name in `clients`, registering `callback` as the URL the authorization answer comes
/// back to — and answer one record per app client.
///
/// Everything the records carry is read back out of Amazon Cognito's own answers: the pool
/// ARN the create minted, the domain prefix `DescribeUserPoolDomain` reports for the domain,
/// and each `ClientId`. The callback registered on the client is asserted here, so a row that
/// later compares the redirect's `redirect_uri` against it is comparing two values this
/// emulator produced independently.
async fn hosted_ui_pool(
    router: &MultiServiceRouter,
    region: &str,
    case: &'static str,
    pool_name: &str,
    prefix: &str,
    callback: &str,
    clients: &[&str],
) -> Vec<PoolClient> {
    let pool = cognito(router, region, "CreateUserPool", &json!({ "PoolName": pool_name })).await;
    let pool_id = as_str("CreateUserPool", &pool["UserPool"], "Id");
    let pool_arn = as_str("CreateUserPool", &pool["UserPool"], "Arn");

    cognito(
        router,
        region,
        "CreateUserPoolDomain",
        &json!({ "UserPoolId": pool_id, "Domain": prefix }),
    )
    .await;
    let described = cognito(
        router,
        region,
        "DescribeUserPoolDomain",
        &json!({ "Domain": prefix }),
    )
    .await;
    let domain = as_str(
        "DescribeUserPoolDomain",
        &described["DomainDescription"],
        "Domain",
    );
    assert_eq!(
        domain, prefix,
        "case {case:?}: Amazon Cognito must hold the domain the create named, got {described}"
    );

    let mut records = Vec::new();
    for client_name in clients {
        let created = cognito(
            router,
            region,
            "CreateUserPoolClient",
            &json!({
                "UserPoolId": pool_id,
                "ClientName": client_name,
                // The authorization server the hosted UI is, as a client that completes the
                // authorization-code grant configures it.
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code"],
                "AllowedOAuthScopes": ["openid", "email"],
                "SupportedIdentityProviders": ["COGNITO"],
                "CallbackURLs": [callback],
            }),
        )
        .await;
        let client = &created["UserPoolClient"];
        let client_id = as_str("CreateUserPoolClient", client, "ClientId");
        let registered: Vec<String> = client["CallbackURLs"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no CallbackURLs in {client}"))
            .iter()
            .map(|url| url.as_str().unwrap_or_default().to_string())
            .collect();
        assert!(
            registered.iter().any(|url| url == callback),
            "case {case:?}: the app client must hold the callback it registered, expected \
             {callback:?} among {registered:?}"
        );
        records.push(PoolClient {
            case,
            pool_arn: pool_arn.clone(),
            domain: domain.clone(),
            client_id,
        });
    }
    records
}

/// A private certificate authority ARN of the form `CertificateAuthorityArn`'s pattern
/// requires. A request naming one needs no domain validation ("if you are requesting a private
/// certificate, domain validation is not required", `RequestCertificate` doc), so it is the
/// request that reaches `ISSUED` — the only status a listener can present a certificate in.
fn private_ca_arn() -> String {
    format!(
        "arn:aws:acm-pca:{REGION}:000000000000:certificate-authority\
         /12345678-1234-1234-1234-123456789012"
    )
}

/// Request one private certificate, look at `DescribeCertificate` until Certificate Manager
/// reports it `ISSUED`, and answer the ARN it minted.
async fn issued_certificate(router: &MultiServiceRouter, domain: &str) -> String {
    let requested = acm(
        router,
        "RequestCertificate",
        &json!({ "DomainName": domain, "CertificateAuthorityArn": private_ca_arn() }),
    )
    .await;
    let arn = as_str("RequestCertificate", &requested, "CertificateArn");
    // The issuance is a transition the clock owes the request, materialised by whichever call
    // next touches that service's store — so the wait is for the status itself.
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = acm(
            router,
            "DescribeCertificate",
            &json!({ "CertificateArn": arn }),
        )
        .await;
        let reported = described["Certificate"]["Status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if reported == "ISSUED" {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "setup: expected Certificate Manager to report ISSUED for {arn}, got {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}


/// Create one internet-facing Application Load Balancer and answer its ARN and the address it
/// publishes — reported once the load balancer carries the state the clock owes it, which is
/// what a client waits for before sending traffic.
async fn load_balancer(
    router: &MultiServiceRouter,
    name: &str,
    subnets: &[String],
    security_group: &str,
) -> (String, String) {
    let created = elbv2(
        router,
        "CreateLoadBalancer",
        &[
            p("Name", name),
            p("Scheme", "internet-facing"),
            p("Subnets.member.1", &subnets[0]),
            p("Subnets.member.2", &subnets[1]),
            p("SecurityGroups.member.1", security_group),
        ],
    )
    .await;
    let arn = element(&created, "LoadBalancerArn");
    let published = element(&created, "DNSName");
    wait_until_active(router, &arn).await;
    // The address is the load balancer's own, not one call's answer: the read reports the
    // same `DNSName` the create published, which is the address a client that describes the
    // load balancer later is handed.
    let described = elbv2(
        router,
        "DescribeLoadBalancers",
        &[p("LoadBalancerArns.member.1", &arn)],
    )
    .await;
    assert_eq!(
        element(&described, "DNSName"),
        published,
        "case {name}: DescribeLoadBalancers must report the address the create published, got \
         {described}"
    );
    (arn, published)
}

/// Look at `DescribeLoadBalancers` until the load balancer reports the state the clock owes
/// it, which is what a client waits for before sending traffic.
async fn wait_until_active(router: &MultiServiceRouter, arn: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let body = elbv2(
            router,
            "DescribeLoadBalancers",
            &[p("LoadBalancerArns.member.1", arn)],
        )
        .await;
        if body.contains("<Code>active</Code>") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the load balancer never became active, last answer {body}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// An `authenticate-cognito` action's members at action order 1: the pool, its app client and
/// the hosted-UI domain, plus the scope and the behaviour where the row states them.
fn authenticate_cognito(
    pool: &PoolClient,
    scope: Option<&str>,
    on_unauthenticated: Option<&str>,
) -> Vec<(String, String)> {
    let member = "Actions.member.1.AuthenticateCognitoConfig";
    let mut members = vec![
        p("Actions.member.1.Type", "authenticate-cognito"),
        p("Actions.member.1.Order", "1"),
        p(&format!("{member}.UserPoolArn"), &pool.pool_arn),
        p(&format!("{member}.UserPoolClientId"), &pool.client_id),
        p(&format!("{member}.UserPoolDomain"), &pool.domain),
    ];
    if let Some(scope) = scope {
        members.push(p(&format!("{member}.Scope"), scope));
    }
    if let Some(behaviour) = on_unauthenticated {
        members.push(p(&format!("{member}.OnUnauthenticatedRequest"), behaviour));
    }
    members
}

/// The `fixed-response` action at action order 2, which is the routing action every rule here
/// ends with: "[e]ach rule must include exactly one of the following routing actions ... and
/// it must be the last action to be performed" (`Action`).
fn fixed_response() -> Vec<(String, String)> {
    vec![
        p("Actions.member.2.Type", "fixed-response"),
        p("Actions.member.2.Order", "2"),
        p("Actions.member.2.FixedResponseConfig.StatusCode", "200"),
        p("Actions.member.2.FixedResponseConfig.ContentType", "text/plain"),
        p("Actions.member.2.FixedResponseConfig.MessageBody", PROTECTED_BODY),
    ]
}

/// The authentication action under test followed by the routing action it guards.
fn guarded(
    pool: &PoolClient,
    scope: Option<&str>,
    on_unauthenticated: Option<&str>,
) -> Vec<(String, String)> {
    let mut actions = authenticate_cognito(pool, scope, on_unauthenticated);
    actions.extend(fixed_response());
    actions
}

/// Create the HTTPS:443 listener whose DEFAULT actions are `actions`, and answer its ARN.
async fn create_listener(
    router: &MultiServiceRouter,
    lb_arn: &str,
    certificate_arn: &str,
    actions: &[(String, String)],
) -> String {
    let mut members = vec![
        p("LoadBalancerArn", lb_arn),
        p("Protocol", "HTTPS"),
        p("Port", "443"),
        p("Certificates.member.1.CertificateArn", certificate_arn),
    ];
    // A listener's own actions are written under the member name `CreateListener` carries
    // them at; a rule's are `Actions`, so one spelling serves both.
    members.extend(actions.iter().map(|(name, value)| {
        (
            name.replace("Actions.member.", "DefaultActions.member."),
            value.clone(),
        )
    }));
    element(&elbv2(router, "CreateListener", &members).await, "ListenerArn")
}

/// Add one rule at `priority` matching `pattern` and carrying `actions`.
async fn create_rule(
    router: &MultiServiceRouter,
    listener_arn: &str,
    priority: i32,
    pattern: &str,
    actions: &[(String, String)],
) {
    let mut members = vec![
        p("ListenerArn", listener_arn),
        p("Priority", &priority.to_string()),
        p("Conditions.member.1.Field", "path-pattern"),
        p("Conditions.member.1.PathPatternConfig.Values.member.1", pattern),
    ];
    members.extend(actions.iter().cloned());
    elbv2(router, "CreateRule", &members).await;
}

/// What one answer on the published address carried.
struct Dialled {
    status: u16,
    location: String,
    set_cookie: String,
    body: String,
}

/// Dial the published address over TLS and send one request for `path`, as a browser handed
/// that address does: no AWS credential, no redirect followed, the answer as it arrived.
///
/// The exchange runs on a blocking thread, so the listener it addresses — a task of this same
/// runtime — is served while the call waits for the answer. The client does not verify the
/// certificate the listener presents, which is what a client of a local emulator does; what is
/// under test here is what the listener ANSWERS, not which certificate it holds (the ELBv2
/// crate's own `alb_https_listener_tls` suite asserts that).
async fn dial(case: &str, published: &str, path: &str) -> Dialled {
    let url = format!("https://{published}{path}");
    let named = case.to_string();
    let dialled = url.clone();
    tokio::task::spawn_blocking(move || {
        let client = ::reqwest::blocking::Client::builder()
            .danger_accept_invalid_certs(true)
            .redirect(::reqwest::redirect::Policy::none())
            .timeout(DIAL_DEADLINE)
            .build()
            .expect("a TLS client");
        let answered = client.get(&dialled).send().unwrap_or_else(|error| {
            panic!("case {named:?}: the published address {dialled} answered nothing: {error}")
        });
        let status = answered.status().as_u16();
        let header = |name: &str| {
            answered
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string()
        };
        let location = header("location");
        let set_cookie = header("set-cookie");
        let body = answered.text().unwrap_or_default();
        Dialled {
            status,
            location,
            set_cookie,
            body,
        }
    })
    .await
    .unwrap_or_else(|error| panic!("case {case:?}: the dial of {url} did not finish: {error}"))
}

/// The RAW (still percent-encoded) value of one query parameter of `location`, or `None` when
/// the URL carries no such parameter.
fn parameter(location: &str, name: &str) -> Option<String> {
    let query = location.split_once('?')?.1;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(held, _)| *held == name)
        .map(|(_, value)| value.to_string())
}

/// One row: the path the client asks for, the action the rule behind it carries, and what the
/// answer on the published address must be.
struct Row {
    case: &'static str,
    /// The path the client dials, which is also what the rule behind it matches.
    path: &'static str,
    /// Which pool and app client the action names, as an index into the pools below.
    pool: usize,
    /// The `Scope` the action states, where it states one.
    scope: Option<&'static str>,
    /// The `OnUnauthenticatedRequest` the action states, where it states one.
    on_unauthenticated: Option<&'static str>,
    status: u16,
    /// The scope the authorization request must ask for — the stated one, or the documented
    /// default for an action stating none. `None` for a row answered without a redirect.
    asked_scope: Option<&'static str>,
}

/// RULE: a load balancer publishes an address this emulator answers on, and the
/// `authenticate-cognito` action of the listener reached there answers an unauthenticated
/// request as its `OnUnauthenticatedRequest` states — redirecting to the authorization
/// endpoint of the user pool's own hosted-UI domain, as the pool's app client, with the
/// callback on the address the client dialled.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_published_address_redirects_an_unauthenticated_request_to_the_pools_hosted_ui() {
    let router = build_router();
    let certificate = issued_certificate(&router, "hosted-ui-alb.example.com").await;
    let (subnets, security_group) = network(&router).await;
    let (lb_arn, published) =
        load_balancer(&router, "hosted-ui-alb", &subnets, &security_group).await;
    // The published address is asserted to be one of this emulator's own BEFORE anything is
    // dialled: a name in Elastic Load Balancing's own zone would resolve to nothing here, so
    // no client of this load balancer could reach it at all.
    let port = dialable_port("the address the create published", &published);
    // The callback the identity provider returns the authorization answer to: the load
    // balancer's own authentication endpoint on the address the client dialled.
    let callback = format!("https://{published}{CALLBACK_PATH}");

    // Two app clients of one pool in the load balancer's own Region, and one pool in another
    // Region — every one of them created through Amazon Cognito's own API, with the callback
    // above registered on each app client.
    let mut pools = hosted_ui_pool(
        &router,
        REGION,
        "a pool in the load balancer's own Region",
        "hosted-ui-pool",
        "sign-in",
        &callback,
        &["storefront", "back-office"],
    )
    .await;
    pools.extend(
        hosted_ui_pool(
            &router,
            OTHER_REGION,
            "a pool in another Region",
            "elsewhere-pool",
            "elsewhere-sign-in",
            &callback,
            &["storefront"],
        )
        .await,
    );

    let rows: &[Row] = &[
        Row {
            case: "an action stating neither a scope nor a behaviour",
            path: "/",
            pool: 0,
            scope: None,
            on_unauthenticated: None,
            status: 302,
            asked_scope: Some("openid"),
        },
        Row {
            case: "the behaviour stated as authenticate, with a scope of its own",
            path: "/hosted",
            pool: 0,
            scope: Some("openid email"),
            on_unauthenticated: Some("authenticate"),
            status: 302,
            asked_scope: Some("openid email"),
        },
        Row {
            case: "a second app client of the same pool",
            path: "/back-office",
            pool: 1,
            scope: Some("openid"),
            on_unauthenticated: Some("authenticate"),
            status: 302,
            asked_scope: Some("openid"),
        },
        Row {
            case: "a pool of another Region, whose prefix resolves under that Region",
            path: "/elsewhere",
            pool: 2,
            scope: Some("openid email"),
            on_unauthenticated: None,
            status: 302,
            asked_scope: Some("openid email"),
        },
        Row {
            case: "the behaviour stated as deny",
            path: "/deny",
            pool: 0,
            scope: Some("openid"),
            on_unauthenticated: Some("deny"),
            status: 401,
            asked_scope: None,
        },
        Row {
            case: "the behaviour stated as allow",
            path: "/allow",
            pool: 0,
            scope: Some("openid"),
            on_unauthenticated: Some("allow"),
            status: 200,
            asked_scope: None,
        },
    ];

    // The first row's action is the listener's own DEFAULT; every other row is a rule of its
    // own, matched by the path it dials.
    let first = &rows[0];
    let listener = create_listener(
        &router,
        &lb_arn,
        &certificate,
        &guarded(&pools[first.pool], first.scope, first.on_unauthenticated),
    )
    .await;
    for (index, row) in rows.iter().enumerate().skip(1) {
        create_rule(
            &router,
            &listener,
            i32::try_from(index).expect("a rule priority") * 10,
            row.path,
            &guarded(&pools[row.pool], row.scope, row.on_unauthenticated),
        )
        .await;
    }

    for row in rows {
        let pool = &pools[row.pool];
        let answered = dial(row.case, &published, row.path).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {} on {published}, got {} with {:?} and body {:?}",
            row.case, row.status, answered.status, answered.location, answered.body
        );
        let Some(asked_scope) = row.asked_scope else {
            // A request the action answered itself without sending the client anywhere, and
            // one it let through to the action behind it: neither is handed a nonce, because
            // neither is one leg of a sign-in.
            assert!(
                !answered.set_cookie.contains(NONCE_COOKIE),
                "case {:?}: expected no nonce cookie, got {:?}",
                row.case,
                answered.set_cookie
            );
            if row.status == 200 {
                assert!(
                    answered.body.contains(PROTECTED_BODY),
                    "case {:?}: expected the guarded action's own body {PROTECTED_BODY:?}, got \
                     {:?}",
                    row.case,
                    answered.body
                );
            }
            continue;
        };
        let endpoint = format!(
            "https://{host}{AUTHORIZE_PATH}?",
            host = pool.hosted_ui_host()
        );
        assert!(
            answered.location.starts_with(&endpoint),
            "case {:?} ({}): expected a redirect to {endpoint:?}, got {:?}",
            row.case,
            pool.case,
            answered.location
        );
        // Every parameter is compared against the value the OTHER service answered with (the
        // app client id Amazon Cognito minted, the callback on the address the load balancer
        // published), percent-encoded as a query component.
        for (name, expected) in [
            ("client_id", pool.client_id.clone()),
            ("redirect_uri", callback.clone()),
            ("response_type", "code".to_string()),
            ("scope", asked_scope.to_string()),
        ] {
            let read = parameter(&answered.location, name);
            assert_eq!(
                read.as_deref(),
                Some(form_encode(&expected).as_str()),
                "case {:?}: expected {name}={expected:?} in {:?}",
                row.case,
                answered.location
            );
        }
        assert!(
            parameter(&answered.location, "state").is_some_and(|state| !state.is_empty()),
            "case {:?}: the authorization request must carry a state value tying the answer to \
             it, got {:?}",
            row.case,
            answered.location
        );
        assert!(
            answered.set_cookie.contains(NONCE_COOKIE),
            "case {:?}: the redirect must hand out the {NONCE_COOKIE} cookie, got {:?}",
            row.case,
            answered.set_cookie
        );
    }

    // The address stayed the load balancer's own throughout: the port every row dialled is the
    // one the published address still names.
    let described = elbv2(
        &router,
        "DescribeLoadBalancers",
        &[p("LoadBalancerArns.member.1", &lb_arn)],
    )
    .await;
    assert_eq!(
        dialable_port("the address a later read reports", &element(&described, "DNSName")),
        port,
        "case \"a later read\": expected the published address to keep naming the port it is \
         served on, got {described}"
    );
}
