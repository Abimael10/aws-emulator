// SPDX-License-Identifier: CC0-1.0

//! The redirect configuration an app client may be left with, over the wire,
//! driven through the router exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! Four rules are under test, each held by `CreateUserPoolClient` and
//! `UpdateUserPoolClient` alike, decided over the configuration the write would
//! LEAVE the app client with, and before anything is stored:
//!
//! * a URL an app client registers for a browser redirect is an ABSOLUTE URI —
//!   "a redirect URI must meet the following requirements: … Be an absolute
//!   URI"
//!   (`smithy_query(query="doc",shape="CreateUserPoolClientRequest",member="CallbackURLs")`),
//!   because the authorization server sends the browser to the URL as it was
//!   registered, with no base to resolve it against — so a `CallbackURLs`,
//!   `LogoutURLs` or `DefaultRedirectURI` entry that names no scheme is refused
//!   with a 400 `InvalidParameterException` naming that URL;
//! * a URL an app client registers for a browser redirect carries no FRAGMENT
//!   component — "a redirect URI must meet the following requirements: … Not
//!   include a fragment component"
//!   (`smithy_query(query="doc",shape="CreateUserPoolClientRequest",member="CallbackURLs")`),
//!   because the authorization server appends its response to the URI it
//!   redirects to and a browser keeps a fragment to itself — so a `CallbackURLs`,
//!   `LogoutURLs` or `DefaultRedirectURI` entry carrying one is refused with a 400
//!   `InvalidParameterException` naming that URL;
//! * the DEFAULT redirect URI an app client is configured with is one of the
//!   callback URLs it registers — "the default redirect URI … must be in the
//!   `CallbackURLs` list"
//!   (`smithy_query(query="doc",shape="CreateUserPoolClientRequest",member="DefaultRedirectURI")`),
//!   because that default stands in for the `redirect_uri` an authorization
//!   request omits and "Amazon Cognito doesn't accept authorization requests with
//!   `redirect_uri` values that aren't in the list of `CallbackURLs`"
//!   (`CallbackURLs`) — so a `DefaultRedirectURI` the write would not register is
//!   refused with a 400 `InvalidParameterException` naming it;
//! * an app client whose authorization server allows a grant whose response is
//!   delivered by redirect — `code`, which "provides an authorization code as the
//!   response", and `implicit`, which issues the tokens "directly to your user"
//!   (`AllowedOAuthFlows`) — registers at least one callback URL to deliver it to,
//!   so a write that allows one while leaving `CallbackURLs` empty is refused with
//!   a 400 `InvalidOAuthFlowException`.
//!
//! Each table's rows differ along what the rule generalises over rather than on
//! one recorded request: the FORM of the URL (where the fragment sits, an empty
//! fragment, a query beside it, an application scheme, a loopback URL), WHICH
//! redirect member carries it, and which grant combination the client allows.
//! Every refusal is checked to have stored nothing: a refused create adds no app
//! client and a refused update leaves the stored one exactly as it was.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A request's SigV4 credential scope is what routes it to this service. The
/// signature is never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The sentence a write that allows a redirect-based grant with no callback URL
/// is refused with.
const CALLBACK_URL_REQUIRED: &str =
    "CallbackUrls can not be empty when code flow or implicit flow is selected";

/// A callback URL every row that is not about the URL's own form carries.
const GOOD_CALLBACK: &str = "https://app.example.com/cb";

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::EmulatedService::new(engine),
    );
    router.set_request_authentication(false);
    router
}

/// Call `operation` with `body` and return its status and JSON response body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "cognito-idp.us-east-1.amazonaws.com")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        )
        .header("authorization", AUTHORIZATION)
        .body(Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, payload)
}

/// Call `operation`, requiring success, and return its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// The `__type` of an error body, which is the wire error code.
fn error_code(payload: &Value) -> &str {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
}

/// The `message` of an error body.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// Create a pool named `name` and return its minted id.
async fn pool(router: &MultiServiceRouter, name: &str) -> String {
    ok(router, "CreateUserPool", &json!({ "PoolName": name })).await["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id for {name}"))
        .to_string()
}

/// How many app clients `user_pool_id` holds, as `ListUserPoolClients` reports.
async fn client_count(router: &MultiServiceRouter, user_pool_id: &str) -> usize {
    ok(
        router,
        "ListUserPoolClients",
        &json!({ "UserPoolId": user_pool_id }),
    )
    .await["UserPoolClients"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

/// The stored app client, as `DescribeUserPoolClient` renders it.
async fn described(router: &MultiServiceRouter, user_pool_id: &str, client_id: &str) -> Value {
    ok(
        router,
        "DescribeUserPoolClient",
        &json!({ "UserPoolId": user_pool_id, "ClientId": client_id }),
    )
    .await["UserPoolClient"]
        .clone()
}

/// Create one app client carrying a complete OAuth configuration, which every
/// update row is applied to, and return its id.
async fn standing_client(router: &MultiServiceRouter, user_pool_id: &str) -> String {
    ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "standing",
            "AllowedOAuthFlowsUserPoolClient": true,
            "AllowedOAuthFlows": ["code"],
            "AllowedOAuthScopes": ["openid"],
            "CallbackURLs": [GOOD_CALLBACK],
        }),
    )
    .await["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted client id"))
        .to_string()
}

/// Drive `members` through the create and the update path, asserting both are
/// refused with `(code, message)` and that neither wrote anything — or that both
/// are served, when `refusal` is `None`.
async fn assert_both_paths(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    standing_id: &str,
    case: &str,
    members: &Value,
    refusal: Option<(&str, &str)>,
) {
    let body = |extra: Vec<(&str, Value)>| -> Value {
        let mut body = members
            .as_object()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the row's members are an object"));
        for (member, value) in extra {
            body.insert(member.to_string(), value);
        }
        Value::Object(body)
    };

    let before = client_count(router, user_pool_id).await;
    let was = described(router, user_pool_id, standing_id).await;
    let (status, payload) = call(
        router,
        "CreateUserPoolClient",
        &body(vec![
            ("UserPoolId", json!(user_pool_id)),
            ("ClientName", json!("row-client")),
        ]),
    )
    .await;
    let after = client_count(router, user_pool_id).await;
    let (update_status, update_payload) = call(
        router,
        "UpdateUserPoolClient",
        &body(vec![
            ("UserPoolId", json!(user_pool_id)),
            ("ClientId", json!(standing_id)),
        ]),
    )
    .await;
    let now = described(router, user_pool_id, standing_id).await;

    let Some((code, message)) = refusal else {
        assert_eq!(
            status, 200,
            "case {case:?}: the configuration is one an app client can hold, got {status} with \
             {payload}"
        );
        assert_eq!(
            after,
            before + 1,
            "case {case:?}: an accepted create adds one app client, got {after}"
        );
        assert_eq!(
            update_status, 200,
            "case {case:?}: the update owes the same answer, got {update_status} with \
             {update_payload}"
        );
        // An accepted write STORES the redirect configuration it was given: every
        // member this update replaces is read back off the stored app client as
        // the request spelled it, and a member the request omitted is read back
        // absent — so a served row proves the write landed rather than only that
        // the call was not refused.
        for member in REDIRECT_MEMBERS {
            let submitted = members
                .get(member)
                .cloned()
                .unwrap_or(Value::Null);
            let stored = now.get(member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                stored, submitted,
                "case {case:?}: an accepted update stores {member} as submitted, got {stored}"
            );
        }
        return;
    };

    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (400, code, message),
        "case {case:?}: expected a 400 {code} carrying {message:?}, got {status} with {payload}"
    );
    assert_eq!(
        after, before,
        "case {case:?}: a refused create stores no app client, got {after}"
    );
    assert_eq!(
        (
            update_status,
            error_code(&update_payload),
            error_message(&update_payload)
        ),
        (400, code, message),
        "case {case:?}: the update owes the same refusal, got {update_status} with \
         {update_payload}"
    );
    assert_eq!(
        now, was,
        "case {case:?}: a refused update must leave the app client alone, got {now}"
    );
}

/// The URL members an app client registers for a browser redirect: the two lists
/// and the single URI
/// (`smithy_query(query="operation",operation="CreateUserPoolClient")`).
const REDIRECT_MEMBERS: [&str; 3] = ["CallbackURLs", "LogoutURLs", "DefaultRedirectURI"];

/// One URL row: the URL a redirect member carries, and whether a fragment is what
/// it carries.
struct UrlCase {
    case: &'static str,
    url: &'static str,
    has_a_fragment: bool,
}

/// The rows differ along WHERE the fragment sits and what sits beside it — after
/// a path, after a query, on the authority alone, an empty one, one spelled with
/// an encoded character — and, for the served rows, along the forms a redirect URL
/// takes without one: a path that merely contains an encoded `#`, a query, the
/// loopback URLs the member admits and the application schemes it also supports
/// ("app callback URLs such as `myapp://example` are also supported",
/// `smithy_query(query="doc",shape="CreateUserPoolClientRequest",member="CallbackURLs")`).
const URL_CASES: &[UrlCase] = &[
    UrlCase {
        case: "a fragment after a path",
        url: "https://alb.example.com/_dashboards/app/home#login",
        has_a_fragment: true,
    },
    UrlCase {
        case: "a fragment after a query",
        url: "https://app.example.com/cb?state=1#done",
        has_a_fragment: true,
    },
    UrlCase {
        case: "a fragment on the authority alone",
        url: "https://app.example.com#login",
        has_a_fragment: true,
    },
    UrlCase {
        case: "an empty fragment, which is still a fragment component",
        url: "https://app.example.com/cb#",
        has_a_fragment: true,
    },
    UrlCase {
        case: "a fragment on a loopback URL",
        url: "http://localhost:3000/cb#token",
        has_a_fragment: true,
    },
    UrlCase {
        case: "a fragment on an application scheme",
        url: "myapp://example#token",
        has_a_fragment: true,
    },
    UrlCase {
        case: "a plain HTTPS URL",
        url: "https://app.example.com/cb",
        has_a_fragment: false,
    },
    UrlCase {
        case: "a URL whose path carries an encoded number sign",
        url: "https://app.example.com/cb%23login",
        has_a_fragment: false,
    },
    UrlCase {
        case: "a URL carrying a query and no fragment",
        url: "https://app.example.com/cb?state=1",
        has_a_fragment: false,
    },
    UrlCase {
        case: "a loopback URL with no fragment",
        url: "http://127.0.0.1:8080/cb",
        has_a_fragment: false,
    },
    UrlCase {
        case: "an application scheme with no fragment",
        url: "myapp://example",
        has_a_fragment: false,
    },
];

#[tokio::test]
async fn a_registered_redirect_url_carries_no_fragment_component() {
    let router = router();
    let user_pool_id = pool(&router, "fragment-url-pool").await;
    let standing_id = standing_client(&router, &user_pool_id).await;

    for UrlCase {
        case,
        url,
        has_a_fragment,
    } in URL_CASES
    {
        for member in REDIRECT_MEMBERS {
            let mut members = Map::new();
            // The OAuth members a registered redirect URL belongs beside, so
            // every row carries a configuration the other rules admit.
            members.insert("AllowedOAuthFlowsUserPoolClient".to_string(), json!(true));
            members.insert("AllowedOAuthFlows".to_string(), json!(["code"]));
            members.insert("AllowedOAuthScopes".to_string(), json!(["openid"]));
            members.insert("CallbackURLs".to_string(), json!([GOOD_CALLBACK]));
            if member == "DefaultRedirectURI" {
                // "The default redirect URI … must be in the CallbackURLs list"
                // (`DefaultRedirectURI` in this service's model).
                members.insert("CallbackURLs".to_string(), json!([url]));
                members.insert(member.to_string(), json!(url));
            } else {
                members.insert(member.to_string(), json!([url]));
            }
            let case = format!("{case} in {member}");
            let message = format!("{url} cannot use fragment");
            assert_both_paths(
                &router,
                &user_pool_id,
                &standing_id,
                &case,
                &Value::Object(members),
                has_a_fragment.then_some(("InvalidParameterException", message.as_str())),
            )
            .await;
        }
    }
}

/// One row of the absolute-URI rule: the URL a redirect member carries, and
/// whether it names a scheme.
struct SchemeCase {
    case: &'static str,
    url: &'static str,
    names_a_scheme: bool,
}

/// The rows differ along the FORM of the relative reference a caller can send
/// instead of an absolute URI — a bare host, a host with a path, an
/// authority-relative URL, a rooted path, a bare path — and, for the served rows,
/// along the forms an absolute URI takes: HTTPS, a scheme spelled in upper case,
/// the loopback URLs the member admits, and the application schemes it also
/// supports ("app callback URLs such as `myapp://example` are also supported",
/// `smithy_query(query="doc",shape="CreateUserPoolClientRequest",member="CallbackURLs")`).
const SCHEME_CASES: &[SchemeCase] = &[
    SchemeCase {
        case: "a bare host",
        url: "app.example.com",
        names_a_scheme: false,
    },
    SchemeCase {
        case: "a host with a path",
        url: "internal-alb-1234567890.us-west-2.elb.amazonaws.com/_dashboards/app/home",
        names_a_scheme: false,
    },
    SchemeCase {
        case: "an authority-relative URL",
        url: "//app.example.com/cb",
        names_a_scheme: false,
    },
    SchemeCase {
        case: "a rooted path",
        url: "/cb",
        names_a_scheme: false,
    },
    SchemeCase {
        case: "a bare path",
        url: "cb/landing",
        names_a_scheme: false,
    },
    SchemeCase {
        case: "a host with a path and a query",
        url: "app.example.com/cb?state=1",
        names_a_scheme: false,
    },
    SchemeCase {
        case: "an HTTPS URL",
        url: "https://app.example.com/cb",
        names_a_scheme: true,
    },
    SchemeCase {
        case: "a scheme spelled in upper case",
        url: "HTTPS://app.example.com/cb",
        names_a_scheme: true,
    },
    SchemeCase {
        case: "a loopback URL over cleartext",
        url: "http://localhost:3000/cb",
        names_a_scheme: true,
    },
    SchemeCase {
        case: "an application scheme",
        url: "myapp://example",
        names_a_scheme: true,
    },
];

#[tokio::test]
async fn a_registered_redirect_url_is_an_absolute_uri() {
    let router = router();
    let user_pool_id = pool(&router, "scheme-url-pool").await;
    let standing_id = standing_client(&router, &user_pool_id).await;

    for SchemeCase {
        case,
        url,
        names_a_scheme,
    } in SCHEME_CASES
    {
        for member in REDIRECT_MEMBERS {
            let mut members = Map::new();
            // The OAuth members a registered redirect URL belongs beside, so
            // every row carries a configuration the other rules admit.
            members.insert("AllowedOAuthFlowsUserPoolClient".to_string(), json!(true));
            members.insert("AllowedOAuthFlows".to_string(), json!(["code"]));
            members.insert("AllowedOAuthScopes".to_string(), json!(["openid"]));
            members.insert("CallbackURLs".to_string(), json!([GOOD_CALLBACK]));
            if member == "DefaultRedirectURI" {
                // "The default redirect URI … must be in the CallbackURLs list"
                // (`DefaultRedirectURI` in this service's model).
                members.insert("CallbackURLs".to_string(), json!([url]));
                members.insert(member.to_string(), json!(url));
            } else {
                members.insert(member.to_string(), json!([url]));
            }
            let case = format!("{case} in {member}");
            let message = format!("{url} must contain a scheme.");
            assert_both_paths(
                &router,
                &user_pool_id,
                &standing_id,
                &case,
                &Value::Object(members),
                (!names_a_scheme).then_some(("InvalidParameterException", message.as_str())),
            )
            .await;
        }
    }
}

/// The sentence a write naming a default redirect URI the app client would not
/// register is refused with.
fn default_redirect_uri_refusal(url: &str) -> String {
    format!("The default redirect URI {url} is not in the callback URIs list.")
}

#[tokio::test]
async fn the_default_redirect_uri_is_one_of_the_registered_callback_urls() {
    let router = router();
    let user_pool_id = pool(&router, "default-redirect-pool").await;
    let standing_id = standing_client(&router, &user_pool_id).await;

    // The rows differ along HOW the default relates to the callback list it is
    // judged against: absent from a list that holds other URLs, differing from a
    // registered URL only by a trailing slash, judged against no list at all and
    // against an empty one, and — for the served rows — matching the first or the
    // second registered URL, in the schemes the member admits, or left unset.
    let rows: Vec<(&str, Value, Option<String>)> = vec![
        (
            "a default that is none of the registered callback URLs",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code"],
                "AllowedOAuthScopes": ["openid"],
                "CallbackURLs": [GOOD_CALLBACK],
                "DefaultRedirectURI": "https://other.example.com/cb",
            }),
            Some(default_redirect_uri_refusal("https://other.example.com/cb")),
        ),
        (
            "a default differing from a registered URL by a trailing slash",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code"],
                "AllowedOAuthScopes": ["openid"],
                "CallbackURLs": [GOOD_CALLBACK],
                "DefaultRedirectURI": "https://app.example.com/cb/",
            }),
            Some(default_redirect_uri_refusal("https://app.example.com/cb/")),
        ),
        (
            "a default with no callback member at all",
            json!({ "DefaultRedirectURI": GOOD_CALLBACK }),
            Some(default_redirect_uri_refusal(GOOD_CALLBACK)),
        ),
        (
            "a default with an empty callback list",
            json!({ "CallbackURLs": [], "DefaultRedirectURI": GOOD_CALLBACK }),
            Some(default_redirect_uri_refusal(GOOD_CALLBACK)),
        ),
        (
            "a default that is the first of two registered callback URLs",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code"],
                "AllowedOAuthScopes": ["openid"],
                "CallbackURLs": [GOOD_CALLBACK, "https://other.example.com/cb"],
                "DefaultRedirectURI": GOOD_CALLBACK,
            }),
            None,
        ),
        (
            "a default that is the second of two registered callback URLs",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code"],
                "AllowedOAuthScopes": ["openid"],
                "CallbackURLs": [GOOD_CALLBACK, "https://other.example.com/cb"],
                "DefaultRedirectURI": "https://other.example.com/cb",
            }),
            None,
        ),
        (
            "a registered default in an application scheme",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code"],
                "AllowedOAuthScopes": ["openid"],
                "CallbackURLs": ["myapp://example"],
                "DefaultRedirectURI": "myapp://example",
            }),
            None,
        ),
        (
            "a registered loopback default",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code"],
                "AllowedOAuthScopes": ["openid"],
                "CallbackURLs": ["http://localhost:3000/cb"],
                "DefaultRedirectURI": "http://localhost:3000/cb",
            }),
            None,
        ),
        (
            "callback URLs with no default at all",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code"],
                "AllowedOAuthScopes": ["openid"],
                "CallbackURLs": [GOOD_CALLBACK],
            }),
            None,
        ),
    ];

    for (case, members, refusal) in rows {
        assert_both_paths(
            &router,
            &user_pool_id,
            &standing_id,
            case,
            &members,
            refusal
                .as_deref()
                .map(|message| ("InvalidParameterException", message)),
        )
        .await;
    }
}

#[tokio::test]
async fn a_client_allowing_a_redirect_grant_registers_a_callback_url() {
    let router = router();
    let user_pool_id = pool(&router, "redirect-grant-pool").await;
    let standing_id = standing_client(&router, &user_pool_id).await;

    // The rows differ along WHICH grants the client allows (each redirect-based
    // one alone, both together, the machine-to-machine grant that redirects
    // nobody, and none at all), along whether the authorization server is turned
    // on, and along how the callback list is left (absent, empty, one URL, two).
    let rows: Vec<(&str, Value, bool)> = vec![
        (
            "the code grant with no callback member at all",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code"],
                "AllowedOAuthScopes": ["openid"],
            }),
            true,
        ),
        (
            "the code grant with an empty callback list",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code"],
                "AllowedOAuthScopes": ["openid"],
                "CallbackURLs": [],
            }),
            true,
        ),
        (
            "the implicit grant alone with an empty callback list",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["implicit"],
                "AllowedOAuthScopes": ["openid"],
                "CallbackURLs": [],
            }),
            true,
        ),
        (
            "both redirect grants with no callback member",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code", "implicit"],
                "AllowedOAuthScopes": ["openid", "email"],
            }),
            true,
        ),
        (
            "the code grant with one callback URL",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code"],
                "AllowedOAuthScopes": ["openid"],
                "CallbackURLs": [GOOD_CALLBACK],
            }),
            false,
        ),
        (
            "both redirect grants with two callback URLs",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code", "implicit"],
                "AllowedOAuthScopes": ["openid"],
                "CallbackURLs": [GOOD_CALLBACK, "https://other.example.com/cb"],
            }),
            false,
        ),
        (
            "the machine-to-machine grant alone, which redirects nobody",
            json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["client_credentials"],
                "AllowedOAuthScopes": ["openid"],
            }),
            false,
        ),
        (
            "a client whose authorization server is off, naming a redirect grant",
            json!({
                "AllowedOAuthFlowsUserPoolClient": false,
                "AllowedOAuthFlows": ["code"],
            }),
            false,
        ),
        (
            "a client with no OAuth members at all",
            json!({}),
            false,
        ),
    ];

    for (case, members, refused) in rows {
        assert_both_paths(
            &router,
            &user_pool_id,
            &standing_id,
            case,
            &members,
            refused.then_some(("InvalidOAuthFlowException", CALLBACK_URL_REQUIRED)),
        )
        .await;
    }
}

#[tokio::test]
async fn an_accepted_redirect_configuration_is_what_a_later_read_renders() {
    let router = router();
    let user_pool_id = pool(&router, "redirect-round-trip-pool").await;

    // The round trip: the URLs an app client may register are stored as submitted
    // and read back unchanged through `DescribeUserPoolClient`, fragment-free
    // paths, queries and encoded number signs included.
    let created = ok(
        &router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "round-trip",
            "AllowedOAuthFlowsUserPoolClient": true,
            "AllowedOAuthFlows": ["code", "implicit"],
            "AllowedOAuthScopes": ["openid"],
            "CallbackURLs": [GOOD_CALLBACK, "https://app.example.com/cb%23login"],
            "LogoutURLs": ["https://app.example.com/out?done=1"],
            "DefaultRedirectURI": GOOD_CALLBACK,
        }),
    )
    .await;
    let client_id = created["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted client id: {created}"))
        .to_string();
    let read = described(&router, &user_pool_id, &client_id).await;
    assert_eq!(
        (
            read["CallbackURLs"].clone(),
            read["LogoutURLs"].clone(),
            read["DefaultRedirectURI"].clone()
        ),
        (
            json!([GOOD_CALLBACK, "https://app.example.com/cb%23login"]),
            json!(["https://app.example.com/out?done=1"]),
            json!(GOOD_CALLBACK)
        ),
        "the stored client must carry the URLs the create registered, got {read}"
    );
}
