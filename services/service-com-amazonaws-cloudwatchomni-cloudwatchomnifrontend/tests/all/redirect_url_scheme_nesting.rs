// SPDX-License-Identifier: CC0-1.0

//! A `redirectUrl` is decided by its SCHEME, ORIGIN, PATH, QUERY and FRAGMENT, and by
//! nothing else — not by how deeply its scheme is nested and not by how long it is.
//!
//! `CreateOneTimeDeepLinkCode`'s `redirectUrl` reads "Must be an HTTPS URL in the domain
//! with a path of /auth/callback, and cannot include a query string or fragment", so a URL
//! whose scheme is anything else is refused whatever it wraps: a scheme that merely CARRIES
//! an https URL (one or many levels deep) is not an https URL, even though the URL Standard
//! gives a singly-wrapped one the inner URL's tuple origin — which is exactly the origin of
//! the domain's own endpoint. Deciding such a URL also costs the service no stack
//! proportional to the nesting depth: the answer for a URL with one wrapper and the answer
//! for a URL with thousands are the same refusal, served by a process that goes on serving.
//!
//! The rows differ along the nesting depth (one wrapper, two, sixteen and thousands), along
//! the wrapping scheme, and along the case the scheme is written in, so none of them is one
//! recorded request restated. The two admitted rows show the refusals are about the scheme
//! rather than about the operation.

use std::sync::Arc;

use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError, Service,
};

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::create_domain::CreateDomain;
use omni::ops::create_one_time_deep_link_code::{
    CreateOneTimeDeepLinkCode, CreateOneTimeDeepLinkCodeErrorModeled,
};
use omni::ops::types::{CreateDomainInput, CreateOneTimeDeepLinkCodeInput, Domain};
use omni::scaffolded::Scaffolded;

/// The Region the suite signs for, the one the handlers record on what they create.
const REGION: &str = "us-west-2";

/// The one path a `redirectUrl` may address, per the member's model doc ("with a path of
/// /auth/callback").
const CALLBACK_PATH: &str = "/auth/callback";

/// The refusal a `redirectUrl` that is not an HTTPS URL earns, as the service words it.
const SCHEME_MUST_BE_HTTPS: &str = "Invalid redirect URI: scheme must be https";

/// How deeply a scheme is nested in the deepest row.
///
/// Far past the depth at which resolving a nested URL by recursion exhausts a thread's
/// stack, so a row this deep is served only by a decision whose cost does not grow with the
/// depth.
const DEEP_NESTING: usize = 10_000;

/// A SigV4 `Authorization` header whose credential scope names this service's signing name
/// and the Region the handlers record.
fn signed_request_parts() -> http::request::Parts {
    let (parts, ()) = http::Request::builder()
        .method("POST")
        .uri("/service/CloudWatchOmniFrontend/operation/Unused")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/cloudwatch/aws4_request, \
                 SignedHeaders=host, Signature=emulator"
            ),
        )
        .body(())
        .expect("a well-formed request")
        .into_parts();
    parts
}

/// One emulator: one shared engine, the service mounted on a router, and the request
/// metadata a handler reads.
struct Omni {
    engine: Arc<engine::InMemoryEngine>,
    service: omni::EmulatedService<engine::InMemoryEngine>,
    router: MultiServiceRouter,
    parts: http::request::Parts,
}

impl Omni {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(omni::EmulatedService::new(Arc::clone(&engine)));
        Self {
            service: omni::EmulatedService::new(Arc::clone(&engine)),
            engine,
            router,
            parts: signed_request_parts(),
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(
            &self.parts,
            self.service.definition().identity(),
            EmbeddedEmulator::from_router(self.router.clone()),
        )
    }

    /// The account's domain, created through the handler that persists it.
    async fn domain(&self, name: &str) -> Domain {
        CreateDomain::new(Arc::clone(&self.engine))
            .handle(
                CreateDomainInput {
                    name: name.to_string(),
                    identity_providers: vec!["IAM".to_string()],
                    client_token: Some(format!("token-{name}")),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {name:?}: the domain create failed: {error:?}"))
            .domain
    }

    /// The message `CreateOneTimeDeepLinkCode` refuses `redirect_url` with, or a panic
    /// naming what came back instead.
    async fn redirect_refusal(&self, case: &str, domain_id: &str, redirect_url: &str) -> String {
        let result = CreateOneTimeDeepLinkCode::new(Arc::clone(&self.engine))
            .handle(
                CreateOneTimeDeepLinkCodeInput {
                    domain_id: domain_id.to_string(),
                    ttl_seconds: None,
                    redirect_url: Some(redirect_url.to_string()),
                },
                &self.ctx(),
            )
            .await;
        match result {
            Ok(output) => {
                panic!("case {case:?}: expected a ValidationException, got Ok({output:?})")
            }
            Err(error) => {
                assert_eq!(
                    (error.shape_name(), error.http_status()),
                    ("ValidationException", 400),
                    "case {case:?}: expected a 400 ValidationException, got {error:?}"
                );
                match error {
                    Scaffolded::Modeled(
                        CreateOneTimeDeepLinkCodeErrorModeled::ValidationException(refusal),
                    ) => refusal.message,
                    other => panic!(
                        "case {case:?}: expected the modelled ValidationException, got {other:?}"
                    ),
                }
            }
        }
    }

    /// The code `CreateOneTimeDeepLinkCode` mints for `redirect_url`, or a panic naming the
    /// refusal it answered instead.
    async fn admitted_redirect(&self, case: &str, domain_id: &str, redirect_url: &str) -> String {
        CreateOneTimeDeepLinkCode::new(Arc::clone(&self.engine))
            .handle(
                CreateOneTimeDeepLinkCodeInput {
                    domain_id: domain_id.to_string(),
                    ttl_seconds: None,
                    redirect_url: Some(redirect_url.to_string()),
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: an admissible redirect was refused: {error:?}")
            })
            .code
    }
}

/// `depth` wrappers of `scheme` around `inner`, e.g. `blob:blob:https://host/path`.
fn nested(scheme: &str, depth: usize, inner: &str) -> String {
    let prefix = [scheme, ":"].concat().repeat(depth);
    prefix + inner
}

#[tokio::test]
async fn a_redirect_is_refused_whatever_scheme_wraps_it_and_however_deeply() {
    let omni = Omni::new();
    let domain = omni.domain("omni-nested-redirects").await;
    let endpoint = domain.domain_endpoint_url.clone();
    let callback = format!("{endpoint}{CALLBACK_PATH}");

    // Rows differ along the wrapping scheme and along the nesting depth: one wrapper around
    // the domain's OWN callback (the URL Standard gives that one the endpoint's tuple
    // origin, so an origin check alone would admit it), two, sixteen, and thousands —
    // besides the schemes that carry no URL at all.
    let mut refused: Vec<(String, String)> = vec![
        (
            "one blob wrapper around the domain's own callback".to_string(),
            nested("blob", 1, &callback),
        ),
        (
            "two blob wrappers around the domain's own callback".to_string(),
            nested("blob", 2, &callback),
        ),
        (
            "sixteen blob wrappers around the domain's own callback".to_string(),
            nested("blob", 16, &callback),
        ),
        (
            "a blob wrapper written in upper case".to_string(),
            nested("BLOB", 1, &callback),
        ),
        (
            "a filesystem wrapper around the domain's own callback".to_string(),
            nested("filesystem", 1, &callback),
        ),
        (
            "a blob wrapper around a websocket URL".to_string(),
            nested("blob", 1, "ws://omni.example/auth/callback"),
        ),
        (
            "a javascript URL".to_string(),
            "javascript:location='/auth/callback'".to_string(),
        ),
        (
            "a data URL".to_string(),
            "data:text/plain,/auth/callback".to_string(),
        ),
    ];
    for depth in [DEEP_NESTING / 2, DEEP_NESTING] {
        refused.push((
            format!("{depth} blob wrappers around the domain's own callback"),
            nested("blob", depth, &callback),
        ));
    }

    for (case, redirect_url) in &refused {
        let message = omni
            .redirect_refusal(case, &domain.domain_id, redirect_url)
            .await;
        assert_eq!(
            message, SCHEME_MUST_BE_HTTPS,
            "case {case:?}: expected {SCHEME_MUST_BE_HTTPS:?}, got {message:?}"
        );
    }

    // The refusals above are about the scheme, not about the operation: the same callback
    // unwrapped is admitted, and so is one whose scheme is written in upper case, which the
    // URL Standard normalises.
    for (case, admitted) in [
        ("the domain's own callback", callback.clone()),
        (
            "the domain's own callback with an upper-case scheme",
            callback.replacen("https://", "HTTPS://", 1),
        ),
    ] {
        let code = omni
            .admitted_redirect(case, &domain.domain_id, &admitted)
            .await;
        assert!(
            code.chars().all(|c| c.is_ascii_alphanumeric()) && code.len() >= 32,
            "case {case:?}: expected an opaque URL-safe code, got {code:?}"
        );
    }
}

/// The origin the URL crate reports for a wrapped URL, which is what "in the domain" is
/// decided by once the scheme has been admitted: the inner URL's origin only when that
/// inner URL is itself one a browser has an origin for, and an opaque origin otherwise —
/// answered for any nesting depth without the stack growing with it.
#[test]
fn a_wrapped_url_has_a_tuple_origin_only_one_level_deep() {
    let endpoint = "https://d-omni.cloudwatch-omni.global.app.aws";
    let callback = format!("{endpoint}{CALLBACK_PATH}");
    let endpoint_origin = ::url::Url::parse(endpoint)
        .expect("a domain endpoint parses")
        .origin();

    // Rows differ along the nesting depth and along the inner scheme: one wrapper around an
    // https URL is the only shape with a tuple origin.
    let mut cases: Vec<(String, String, bool)> = vec![
        (
            "one wrapper around an https URL".to_string(),
            nested("blob", 1, &callback),
            true,
        ),
        (
            "two wrappers around an https URL".to_string(),
            nested("blob", 2, &callback),
            false,
        ),
        (
            "sixteen wrappers around an https URL".to_string(),
            nested("blob", 16, &callback),
            false,
        ),
        (
            "one wrapper around a websocket URL".to_string(),
            nested("blob", 1, "ws://omni.example/auth/callback"),
            false,
        ),
    ];
    for depth in [DEEP_NESTING / 2, DEEP_NESTING] {
        cases.push((
            format!("{depth} wrappers around an https URL"),
            nested("blob", depth, &callback),
            false,
        ));
    }

    for (case, candidate, tuple) in &cases {
        let parsed = ::url::Url::parse(candidate)
            .unwrap_or_else(|error| panic!("case {case:?}: the URL must parse, got {error:?}"));
        let origin = parsed.origin();
        assert_eq!(
            origin.is_tuple(),
            *tuple,
            "case {case:?}: expected is_tuple() == {tuple}, got {origin:?}"
        );
        assert_eq!(
            origin == endpoint_origin,
            *tuple,
            "case {case:?}: expected the endpoint's origin to match only one level deep, \
             got {origin:?}"
        );
    }
}
