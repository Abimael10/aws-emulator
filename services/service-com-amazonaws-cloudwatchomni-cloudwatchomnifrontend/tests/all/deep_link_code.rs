// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for CloudWatchOmni's one-time deep-link codes.
//!
//! Every call goes through the real `CreateOneTimeDeepLinkCode` handler holding the SAME
//! shared engine the router hands every service, so the domain a create persisted is the
//! domain the code is minted for, and [`the_deep_link_lifecycle_over_the_rpcv2cbor_wire`]
//! drives the same store over HTTP so routing, deserialization, the handler and response
//! serialization all run.
//!
//! The suite asserts the RULES rather than one recorded exchange:
//!
//! * a code is minted only for a domain the store HOLDS: a well-formed `domainId` the
//!   account holds nothing under is refused `ResourceNotFoundException` (404) with the
//!   message every operation of this service words a missing domain with;
//! * `expiresAt` is the lifetime the request asked for past the call, and the lifetime a
//!   request that named none leaves is the documented default of 300 seconds;
//! * the `code` is opaque, URL-safe and fresh on every call \u2014 two calls never report one
//!   code twice;
//! * `deepLinkUrl` is a RELATIVE reference at the redemption path carrying exactly the code,
//!   the domain, the Region and the redirect, each read back from the persisted domain
//!   rather than from the request;
//! * a `redirectUrl` is admitted only at one of the domain's OWN endpoints, at the one
//!   callback path, with neither a query string nor a fragment \u2014 and each condition it
//!   fails is refused `ValidationException` (400) with the reason AWS words it with.
//!
//! The rows differ along the lifetime (both published bounds of `ttlSeconds`'
//! `@range(min: 60, max: 600)`, and absent), along the redirect (absent, at the endpoint the
//! domain's id addresses, at the endpoint derived from its name) and along the condition a
//! refused redirect fails, so none of them is one recorded request restated.
//!
//! Each assertion names its case and the actual value, so a failure identifies the row of
//! the table that broke.

use std::collections::HashSet;
use std::sync::Arc;

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::{CborCodec, DeserializableStruct};
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError, Service,
};

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::create_domain::{CreateDomain, CreateDomainError};
use omni::ops::create_one_time_deep_link_code::{
    CreateOneTimeDeepLinkCode, CreateOneTimeDeepLinkCodeError, CreateOneTimeDeepLinkCodeErrorModeled,
};
use omni::ops::get_domain::{GetDomain, GetDomainError};
use omni::ops::types::{
    CreateDomainInput, CreateOneTimeDeepLinkCodeInput, CreateOneTimeDeepLinkCodeOutput, Domain,
    GetDomainInput, GetDomainOutput, CREATE_ONE_TIME_DEEP_LINK_CODE_INPUT_SCHEMA,
};
use omni::scaffolded::Scaffolded;

use crate::validation_message_wording::error_envelope;

/// The region the suite signs for; the handler records it on the domain it creates, so it is
/// the Region the deep-link URL is expected to carry.
const REGION: &str = "us-west-2";

/// The path a one-time code is redeemed at, which `deepLinkUrl` addresses.
const DEEP_LINK_PATH: &str = "/auth/code";

/// The one path a `redirectUrl` may address, per the member's model doc ("with a path of
/// /auth/callback").
const CALLBACK_PATH: &str = "/auth/callback";

/// The lifetime a request that names no `ttlSeconds` leaves, per the member's model doc
/// ("Defaults to 300").
const DEFAULT_TTL_SECONDS: i32 = 300;

/// A well-formed `DomainId` (its modelled `^d-[0-9a-z]{1,25}$`) the store holds nothing
/// under.
const MISSING_DOMAIN_ID: &str = "d-doesnotexist";

/// The message every operation of this service words a missing domain with.
const DOMAIN_NOT_FOUND: &str = "Domain not found";

/// What every refusal of a `redirectUrl` begins with.
const INVALID_REDIRECT_URI: &str = "Invalid redirect URI";

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
        // The router's service and the operations constructed below all hold the SAME
        // engine, so both front doors read and write one store.
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
        self.create_domain(name)
            .await
            .unwrap_or_else(|error| panic!("case {name:?}: the domain create failed: {error:?}"))
    }

    async fn create_domain(&self, name: &str) -> Result<Domain, CreateDomainError> {
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
            .map(|output| output.domain)
    }

    async fn get_domain(&self, domain_id: &str) -> Result<GetDomainOutput, GetDomainError> {
        GetDomain::new(Arc::clone(&self.engine))
            .handle(
                GetDomainInput {
                    domain_id: domain_id.to_string(),
                },
                &self.ctx(),
            )
            .await
    }

    async fn deep_link_code(
        &self,
        input: &CreateOneTimeDeepLinkCodeInput,
    ) -> Result<CreateOneTimeDeepLinkCodeOutput, CreateOneTimeDeepLinkCodeError> {
        CreateOneTimeDeepLinkCode::new(Arc::clone(&self.engine))
            .handle(input.clone(), &self.ctx())
            .await
    }

    /// Dispatch one rpcv2Cbor request over HTTP and return the status and body.
    async fn over_the_wire<I: SerializableStruct>(
        &self,
        operation: &str,
        input_schema: &Schema,
        input: &I,
    ) -> (u16, bytes::Bytes) {
        let codec = CborCodec::default();
        let mut serializer = codec.create_serializer();
        serializer
            .write_struct(input_schema, input)
            .expect("the input shape serializes");
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!(
                "/service/CloudWatchOmniFrontend/operation/{operation}"
            ))
            .header("content-type", "application/cbor")
            .header("smithy-protocol", "rpc-v2-cbor");
        for (name, value) in &self.parts.headers {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(bytes::Bytes::from(serializer.finish()))
            .expect("a well-formed rpcv2Cbor request");
        let response = self.router.dispatch(request).await;
        (response.status().as_u16(), response.body().clone())
    }
}

/// A `CreateOneTimeDeepLinkCodeInput` naming a domain, and only what a row varies.
fn deep_link_input(
    domain_id: &str,
    ttl_seconds: Option<i32>,
    redirect_url: Option<String>,
) -> CreateOneTimeDeepLinkCodeInput {
    CreateOneTimeDeepLinkCodeInput {
        domain_id: domain_id.to_string(),
        ttl_seconds,
        redirect_url,
    }
}

/// Epoch seconds as this service's `now()` reads them, for bounding a reported expiry.
fn epoch_seconds() -> i64 {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the host clock is past the epoch")
        .as_secs();
    i64::try_from(seconds).expect("epoch seconds fit in the modelled type")
}

/// The query parameters of a relative deep-link URL, in the order it carries them, with
/// every value percent-decoded.
fn link_query(case: &str, deep_link_url: &str) -> Vec<(String, String)> {
    let query = deep_link_url.strip_prefix(&format!("{DEEP_LINK_PATH}?")).unwrap_or_else(|| {
        panic!("case {case:?}: expected a relative link at {DEEP_LINK_PATH}, got {deep_link_url:?}")
    });
    url::form_urlencoded::parse(query.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

/// The message the operation's modelled `ValidationException` carries, or a panic naming
/// what came back instead.
fn validation_message(
    case: &str,
    result: Result<CreateOneTimeDeepLinkCodeOutput, CreateOneTimeDeepLinkCodeError>,
) -> String {
    match result {
        Ok(output) => panic!("case {case:?}: expected a ValidationException, got Ok({output:?})"),
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

#[tokio::test]
async fn a_deep_link_code_carries_what_redeeming_it_needs_and_expires_when_it_was_asked_to() {
    let omni = Omni::new();
    let domain = omni.domain("omni-deep-link").await;
    let id_endpoint = domain.domain_endpoint_url.clone();
    let name_endpoint = domain
        .custom_endpoint_urls
        .clone()
        .and_then(|urls| urls.first().cloned())
        .expect("a created domain reports an endpoint derived from its name");

    // Rows differ along the lifetime asked for — both published bounds of `ttlSeconds`'
    // `@range(min: 60, max: 600)`, one in between, and absent — and along the redirect:
    // absent, at the endpoint the domain's id addresses, and at the one derived from its
    // name.
    let cases: Vec<(&str, Option<i32>, Option<String>)> = vec![
        ("no lifetime and no redirect", None, None),
        ("the shortest lifetime", Some(60), None),
        ("the longest lifetime", Some(600), None),
        (
            "a redirect at the endpoint the id addresses",
            Some(300),
            Some(format!("{id_endpoint}{CALLBACK_PATH}")),
        ),
        (
            "a redirect at the endpoint derived from the name",
            Some(120),
            Some(format!("{name_endpoint}{CALLBACK_PATH}")),
        ),
    ];

    let mut seen: HashSet<String> = HashSet::new();
    for (case, ttl_seconds, redirect_url) in cases {
        // The expiry is bounded between the clock read before the call and the one after it,
        // each plus the lifetime: both bounds move with the host, so neither a slow nor a
        // fast machine can decide this.
        let before = epoch_seconds();
        let minted = omni
            .deep_link_code(&deep_link_input(
                &domain.domain_id,
                ttl_seconds,
                redirect_url.clone(),
            ))
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the code was refused: {error:?}"));
        let after = epoch_seconds();

        let lifetime = i64::from(ttl_seconds.unwrap_or(DEFAULT_TTL_SECONDS));
        assert!(
            (before + lifetime..=after + lifetime).contains(&minted.expires_at.secs()),
            "case {case:?}: expected an expiry in {:?}, got {:?}",
            before + lifetime..=after + lifetime,
            minted.expires_at
        );
        assert_eq!(
            minted.expires_at.subsec_nanos(),
            0,
            "case {case:?}: an instant this service reports carries whole seconds, got {:?}",
            minted.expires_at
        );

        // The code is opaque, long enough not to be guessable, carries only characters that
        // stand for themselves in a URL, and is fresh on every call.
        assert!(
            minted.code.len() >= 32
                && minted.code.chars().all(|c| c.is_ascii_alphanumeric()),
            "case {case:?}: expected an opaque URL-safe code of at least 32 characters, got {:?}",
            minted.code
        );
        assert!(
            seen.insert(minted.code.clone()),
            "case {case:?}: the code {:?} was already handed out by an earlier call",
            minted.code
        );

        // The link is a relative reference carrying exactly the four parameters redeeming
        // the code needs, with the redirect EMPTY when the request asked for none.
        let query = link_query(case, &minted.deep_link_url);
        let expected = vec![
            ("code".to_string(), minted.code.clone()),
            ("domainId".to_string(), domain.domain_id.clone()),
            ("region".to_string(), REGION.to_string()),
            (
                "redirect".to_string(),
                redirect_url.clone().unwrap_or_default(),
            ),
        ];
        assert_eq!(
            query, expected,
            "case {case:?}: expected the link to carry {expected:?}, got {query:?} \
             ({:?})",
            minted.deep_link_url
        );
    }

    // ROUND TRIP: the domain the links name is the persisted one, read back through the
    // implemented reader rather than from the create's own answer.
    let stored = omni
        .get_domain(&domain.domain_id)
        .await
        .expect("the created domain is in the store")
        .domain;
    let minted = omni
        .deep_link_code(&deep_link_input(&stored.domain_id, None, None))
        .await
        .expect("a code is minted for the persisted domain");
    let query = link_query("the persisted domain", &minted.deep_link_url);
    assert_eq!(
        (
            query
                .iter()
                .find(|(key, _)| key == "domainId")
                .map(|(_, value)| value.as_str()),
            query
                .iter()
                .find(|(key, _)| key == "region")
                .map(|(_, value)| value.as_str()),
        ),
        (Some(stored.domain_id.as_str()), Some(stored.region.as_str())),
        "case \"the persisted domain\": the link must name the stored domain and its Region, \
         got {query:?}"
    );
}

#[tokio::test]
async fn a_redirect_is_admitted_only_at_the_domains_own_callback() {
    let omni = Omni::new();
    let domain = omni.domain("omni-redirects").await;
    let endpoint = domain.domain_endpoint_url.clone();
    let host = endpoint
        .strip_prefix("https://")
        .expect("a domain endpoint is an https URL")
        .to_string();
    let name_endpoint = domain
        .custom_endpoint_urls
        .clone()
        .and_then(|urls| urls.first().cloned())
        .expect("a created domain reports an endpoint derived from its name");

    // Every endpoint the domain reports is admitted at the callback path.
    for admitted in [
        format!("{endpoint}{CALLBACK_PATH}"),
        format!("{name_endpoint}{CALLBACK_PATH}"),
    ] {
        omni.deep_link_code(&deep_link_input(
            &domain.domain_id,
            None,
            Some(admitted.clone()),
        ))
        .await
        .unwrap_or_else(|error| {
            panic!("case {admitted:?}: a redirect at the domain's own endpoint was refused: {error:?}")
        });
    }

    // Rows differ along the condition each one fails: the text is not a URL, the scheme, the
    // host, the port (a different origin of the same host), the path, the query string and
    // the fragment.
    let refused: Vec<(&str, String, &str)> = vec![
        (
            "not a URL at all",
            CALLBACK_PATH.to_string(),
            "malformed URL",
        ),
        (
            "an http URL at the domain",
            format!("http://{host}{CALLBACK_PATH}"),
            "scheme must be https",
        ),
        (
            "another host entirely",
            format!("https://attacker.example{CALLBACK_PATH}"),
            "unrecognized domain",
        ),
        (
            "the domain's host as a prefix of another",
            format!("https://{host}.attacker.example{CALLBACK_PATH}"),
            "unrecognized domain",
        ),
        (
            "the domain's host on another port",
            format!("https://{host}:8443{CALLBACK_PATH}"),
            "unrecognized domain",
        ),
        (
            "another path at the domain",
            format!("{endpoint}/auth/elsewhere"),
            "path must be /auth/callback",
        ),
        (
            "the callback path with a query string",
            format!("{endpoint}{CALLBACK_PATH}?next=/home"),
            "query string and fragment are not allowed",
        ),
        (
            "the callback path with a fragment",
            format!("{endpoint}{CALLBACK_PATH}#section"),
            "query string and fragment are not allowed",
        ),
    ];
    for (case, redirect_url, reason) in refused {
        let message = validation_message(
            case,
            omni.deep_link_code(&deep_link_input(
                &domain.domain_id,
                None,
                Some(redirect_url.clone()),
            ))
            .await,
        );
        let expected = format!("{INVALID_REDIRECT_URI}: {reason}");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains(&redirect_url),
            "case {case:?}: the refusal renders the URL the caller sent, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_code_is_minted_only_for_a_domain_the_store_holds() {
    // Rows differ along what the store holds when a domain it does NOT hold is named:
    // nothing at all, and a domain other than the one the request names.
    for (case, other_domain) in [
        ("an empty store", None),
        ("a store holding another domain", Some("omni-other-domain")),
    ] {
        let omni = Omni::new();
        if let Some(name) = other_domain {
            omni.domain(name).await;
        }
        let error = match omni
            .deep_link_code(&deep_link_input(MISSING_DOMAIN_ID, None, None))
            .await
        {
            Ok(output) => panic!(
                "case {case:?}: a code was minted for a domain the store does not hold: \
                 {output:?}"
            ),
            Err(error) => error,
        };
        assert_eq!(
            (error.shape_name(), error.http_status()),
            ("ResourceNotFoundException", 404),
            "case {case:?}: expected a 404 ResourceNotFoundException, got {error:?}"
        );
        let message = match error {
            Scaffolded::Modeled(
                CreateOneTimeDeepLinkCodeErrorModeled::ResourceNotFoundException(refusal),
            ) => refusal.message,
            other => panic!(
                "case {case:?}: expected the modelled ResourceNotFoundException, got {other:?}"
            ),
        };
        assert_eq!(
            message, DOMAIN_NOT_FOUND,
            "case {case:?}: expected {DOMAIN_NOT_FOUND:?}, got {message:?}"
        );
    }

    // And the domain the account does hold is served, so the refusals above are about the
    // record the request named rather than about the operation.
    let omni = Omni::new();
    let domain = omni.domain("omni-held-domain").await;
    omni.deep_link_code(&deep_link_input(&domain.domain_id, None, None))
        .await
        .expect("the domain the account holds is served");
}

#[tokio::test]
async fn the_deep_link_lifecycle_over_the_rpcv2cbor_wire() {
    let omni = Omni::new();
    // The domain is created through the handler holding the same engine the router's service
    // holds, which is what makes the wire calls below see it.
    let domain = omni.domain("omni-wire-deep-link").await;

    let (status, body) = omni
        .over_the_wire(
            "CreateOneTimeDeepLinkCode",
            CREATE_ONE_TIME_DEEP_LINK_CODE_INPUT_SCHEMA,
            &deep_link_input(
                &domain.domain_id,
                Some(60),
                Some(format!("{}{CALLBACK_PATH}", domain.domain_endpoint_url)),
            ),
        )
        .await;
    assert_eq!(
        status,
        200,
        "CreateOneTimeDeepLinkCode over the wire answered {status}: {:?}",
        String::from_utf8_lossy(&body)
    );
    let codec = CborCodec::default();
    let mut deserializer = codec.create_deserializer(&body);
    let minted = CreateOneTimeDeepLinkCodeOutput::deserialize(&mut deserializer)
        .expect("the output deserializes");
    let query = link_query("over the wire", &minted.deep_link_url);
    assert_eq!(
        query
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<&str>>(),
        vec!["code", "domainId", "region", "redirect"],
        "case \"over the wire\": expected the four link parameters, got {query:?}"
    );
    assert!(
        query.contains(&("code".to_string(), minted.code.clone())),
        "case \"over the wire\": the link must carry the code it was minted with, got {query:?}"
    );

    // A redirect outside the domain is refused over the same wire, with the status, the
    // error code and the message that are requirements of the refusal.
    let (status, body) = omni
        .over_the_wire(
            "CreateOneTimeDeepLinkCode",
            CREATE_ONE_TIME_DEEP_LINK_CODE_INPUT_SCHEMA,
            &deep_link_input(
                &domain.domain_id,
                None,
                Some(format!("https://attacker.example{CALLBACK_PATH}")),
            ),
        )
        .await;
    let (code, message) = error_envelope("a foreign redirect over the wire", &body);
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (
            400,
            "ValidationException",
            "Invalid redirect URI: unrecognized domain"
        ),
        "case \"a foreign redirect over the wire\": expected the modelled refusal, got {status} \
         {code:?}: {message:?}"
    );
}
