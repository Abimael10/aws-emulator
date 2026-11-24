// SPDX-License-Identifier: CC0-1.0

//! The NAME a user pool's hosted domain may take, over the wire: the two forms a `Domain`
//! value has, and which of them each operation that addresses a domain admits.
//!
//! The rule under test: a pool's domain is named by the kind of domain the request asks for —
//! "for custom domains, this is the fully-qualified domain name, such as auth.example.com.
//! For prefix domains, this is the prefix alone, such as myprefix"
//! (`CreateUserPoolDomain.Domain` in this service's model). A request that carries a
//! `CustomDomainConfig` asks for a CUSTOM domain, whose name is a fully-qualified name (at
//! least two labels, each of lowercase letters, digits and hyphens, beginning and ending
//! alphanumeric); one that carries none asks for a PREFIX domain, whose name is one such
//! label. A name that is not of the form its kind takes is refused
//! `InvalidParameterException` with the one sentence this endpoint words a bad domain name
//! with, and a name of either form addresses an existing domain for the delete, which says
//! nothing about the kind.
//!
//! The model's `DomainType` pattern states the PREFIX form alone, so a custom domain's name
//! satisfies none of it: what each row therefore pins is that the FQDN is served, stored and
//! read back, and that the prefix form is still held to the pattern's own shape.
//!
//! The rows differ along what the rule generalises over: the kind of domain asked for, the
//! number of labels in the name, and which character of a label the name breaks the form with.
//!
//! Each row names the case it asserts and every assertion reports the actual value.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// A request's SigV4 credential scope is what routes it to this service. The signature is
/// never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The certificate a custom domain is served with, as a request states it.
const CERTIFICATE_ARN: &str = "arn:aws:acm:us-east-1:000000000000:certificate/11111111-2222-3333-4444-555555555555";

/// The sentence this endpoint words a domain name no pool may take with. Its only variable
/// part is the format it quotes, which is `DomainType`'s pattern as the model declares it
/// (`smithy_query(query="shape",shape="DomainType")`).
const FORM_PROSE: &str = "The domain name contains an invalid character. Domain names can \
                          only contain lower-case letters, numbers, and hyphens. Please \
                          enter a different name that follows this format: \
                          ^[a-z0-9](?:[a-z0-9\\-]{0,61}[a-z0-9])?$";

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::new(
        engine::InMemoryEngine::new(),
    )));
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

/// Create a pool and return its minted id.
async fn pool(router: &MultiServiceRouter, name: &str) -> String {
    ok(router, "CreateUserPool", &json!({ "PoolName": name })).await["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string()
}

/// One row: the domain name asked for, whether the request carries a `CustomDomainConfig`,
/// and whether that pairing is a name the pool may take.
struct NameCase {
    case: &'static str,
    domain: &'static str,
    custom: bool,
    admitted: bool,
}

/// The rows: the two forms, their boundaries, and the characters each excludes.
const NAMES: &[NameCase] = &[
    NameCase {
        case: "a prefix of one label",
        domain: "auth-prefix-one",
        custom: false,
        admitted: true,
    },
    NameCase {
        case: "a prefix of digits and hyphens",
        domain: "a0-1-2b",
        custom: false,
        admitted: true,
    },
    NameCase {
        case: "a single character, the shortest prefix the form admits",
        domain: "a",
        custom: false,
        admitted: true,
    },
    NameCase {
        case: "a fully-qualified name of three labels, for a custom domain",
        domain: "auth.example.com",
        custom: true,
        admitted: true,
    },
    NameCase {
        case: "a fully-qualified name of two labels, for a custom domain",
        domain: "auth.example",
        custom: true,
        admitted: true,
    },
    NameCase {
        case: "a fully-qualified name whose labels carry digits and hyphens",
        domain: "sign-in.corp-2.example",
        custom: true,
        admitted: true,
    },
    NameCase {
        case: "a fully-qualified name asked for as a PREFIX domain",
        domain: "auth.example.org",
        custom: false,
        admitted: false,
    },
    NameCase {
        case: "a one-label name for a custom domain, the form the model's pattern states",
        domain: "customsinglelabel",
        custom: true,
        admitted: true,
    },
    NameCase {
        case: "an upper-case letter in a prefix",
        domain: "AuthPrefix",
        custom: false,
        admitted: false,
    },
    NameCase {
        case: "an upper-case letter in a label of a custom domain",
        domain: "Auth.example.com",
        custom: true,
        admitted: false,
    },
    NameCase {
        case: "an underscore in a prefix",
        domain: "auth_prefix",
        custom: false,
        admitted: false,
    },
    NameCase {
        case: "a trailing hyphen in a label of a custom domain",
        domain: "auth-.example.com",
        custom: true,
        admitted: false,
    },
    NameCase {
        case: "an empty label in a custom domain",
        domain: "auth..example.com",
        custom: true,
        admitted: false,
    },
];

/// The request a row's create or update is made with.
fn domain_request(user_pool_id: &str, row: &NameCase) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    body.insert("Domain".to_string(), json!(row.domain));
    if row.custom {
        body.insert(
            "CustomDomainConfig".to_string(),
            json!({ "CertificateArn": CERTIFICATE_ARN }),
        );
    }
    Value::Object(body)
}

/// A name of the form its kind takes is created, read back and deleted by that same name; one
/// of another form is refused with the form prose and nothing is stored for it.
#[tokio::test]
async fn a_domain_is_named_by_the_form_of_the_kind_the_request_asks_for() {
    let router = router();

    for (index, row) in NAMES.iter().enumerate() {
        let case = row.case;
        // A pool of its own per row: a pool holds at most one prefix domain and at most one
        // custom domain, so the rows cannot compete for a slot.
        let user_pool_id = pool(&router, &format!("domain-form-pool-{index}")).await;
        let (status, payload) = call(
            &router,
            "CreateUserPoolDomain",
            &domain_request(&user_pool_id, row),
        )
        .await;

        if !row.admitted {
            assert_eq!(
                (status, error_code(&payload), error_message(&payload)),
                (400, "InvalidParameterException", FORM_PROSE),
                "case {case:?}: expected the form prose for {:?}, got {status} with {payload}",
                row.domain
            );
            // Nothing was stored for a refused name: the read of it resolves no domain.
            let described = ok(
                &router,
                "DescribeUserPoolDomain",
                &json!({ "Domain": row.domain }),
            )
            .await;
            assert_eq!(
                described["DomainDescription"],
                json!({}),
                "case {case:?}: expected no stored domain for a refused name, got {described}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: expected the name {:?} to be taken, got {status} with {payload}",
            row.domain
        );

        // The round trip: the domain the create took is the domain a read of that name
        // reports, in the pool that asked for it.
        let described = ok(
            &router,
            "DescribeUserPoolDomain",
            &json!({ "Domain": row.domain }),
        )
        .await;
        let domain = &described["DomainDescription"];
        assert_eq!(
            (
                domain["Domain"].as_str(),
                domain["UserPoolId"].as_str(),
                domain["CustomDomainConfig"]["CertificateArn"].as_str(),
            ),
            (
                Some(row.domain),
                Some(user_pool_id.as_str()),
                row.custom.then_some(CERTIFICATE_ARN),
            ),
            "case {case:?}: expected the stored domain to be the one created, got {described}"
        );

        // The pool reports it under the member its kind is published in.
        let read = ok(
            &router,
            "DescribeUserPool",
            &json!({ "UserPoolId": user_pool_id }),
        )
        .await;
        let member = if row.custom { "CustomDomain" } else { "Domain" };
        assert_eq!(
            read["UserPool"][member].as_str(),
            Some(row.domain),
            "case {case:?}: expected the pool to report {member} as {:?}, got {read}",
            row.domain
        );

        // The delete addresses the domain by the same name, whichever form it is.
        let (status, payload) = call(
            &router,
            "DeleteUserPoolDomain",
            &json!({ "UserPoolId": user_pool_id, "Domain": row.domain }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the delete to address {:?}, got {status} with {payload}",
            row.domain
        );
        let described = ok(
            &router,
            "DescribeUserPoolDomain",
            &json!({ "Domain": row.domain }),
        )
        .await;
        assert_eq!(
            described["DomainDescription"],
            json!({}),
            "case {case:?}: expected the deleted domain to resolve to nothing, got {described}"
        );
    }
}

/// A delete whose name is of NEITHER form names no domain of any pool, which is the form
/// refusal rather than a lookup that found nothing.
#[tokio::test]
async fn a_delete_addressed_by_a_name_of_neither_form_is_refused_with_the_form_prose() {
    let router = router();
    let user_pool_id = pool(&router, "domain-form-delete-pool").await;

    for (case, domain) in [
        ("an underscore", "auth_prefix"),
        ("upper-case letters", "AuthPrefix"),
        ("an empty label", "auth..example.com"),
        ("a trailing hyphen", "auth-"),
    ] {
        let (status, payload) = call(
            &router,
            "DeleteUserPoolDomain",
            &json!({ "UserPoolId": user_pool_id.clone(), "Domain": domain }),
        )
        .await;
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (400, "InvalidParameterException", FORM_PROSE),
            "case {case:?}: expected the form prose for {domain:?}, got {status} with {payload}"
        );
    }
}

/// A custom domain's hosted authorization server is announced and served at the address this
/// emulator reports it on, exactly as a prefix domain's is: the domain string is the path
/// segment, whichever form it has.
#[tokio::test]
async fn a_custom_domain_hosts_the_oauth2_endpoints_this_emulator_serves() {
    let router = router();
    let user_pool_id = pool(&router, "custom-domain-hosted-pool").await;
    let domain = "auth.hosted.example";
    ok(
        &router,
        "CreateUserPoolDomain",
        &json!({
            "UserPoolId": user_pool_id,
            "Domain": domain,
            "CustomDomainConfig": { "CertificateArn": CERTIFICATE_ARN },
        }),
    )
    .await;

    let request = http::Request::builder()
        .method("GET")
        .uri(format!("/{user_pool_id}/.well-known/openid-configuration"))
        .header("host", "127.0.0.1:9297")
        .body(Bytes::new())
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "the discovery document must be served, got {}",
        response.status()
    );
    let document: Value =
        serde_json::from_slice(response.body()).expect("the document carries JSON");
    let issuer = document["issuer"]
        .as_str()
        .expect("the document names an issuer")
        .to_string();
    let origin = issuer
        .rsplit_once('/')
        .map(|(origin, _)| origin.to_string())
        .expect("the issuer carries the pool as its path");
    for (member, expected) in [
        ("token_endpoint", format!("{origin}/{domain}/oauth2/token")),
        (
            "userinfo_endpoint",
            format!("{origin}/{domain}/oauth2/userInfo"),
        ),
        (
            "revocation_endpoint",
            format!("{origin}/{domain}/oauth2/revoke"),
        ),
    ] {
        assert_eq!(
            document[member].as_str(),
            Some(expected.as_str()),
            "expected {member} to be announced as {expected:?}, got {:?}",
            document[member]
        );
    }

    // And the endpoint announced there answers: a grant naming an app client this pool does
    // not hold is the OAuth2 refusal of that endpoint, not a route that resolves nowhere.
    let token_request = http::Request::builder()
        .method("POST")
        .uri(format!("/{domain}/oauth2/token"))
        .header("host", "127.0.0.1:9297")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from("grant_type=refresh_token&client_id=nosuchclient"))
        .expect("the request is well formed");
    let answered = router.dispatch(token_request).await;
    let status = answered.status().as_u16();
    let body: Value = serde_json::from_slice(answered.body()).unwrap_or(Value::Null);
    assert_eq!(
        (status, body["error"].as_str()),
        (401, Some("invalid_client")),
        "expected the announced token endpoint to answer the OAuth2 refusal, got {status} with \
         {body}"
    );
}
