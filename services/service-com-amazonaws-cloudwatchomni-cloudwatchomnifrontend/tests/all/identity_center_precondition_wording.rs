// SPDX-License-Identifier: CC0-1.0

//! How CloudWatchOmni words the refusal of a domain that is not provisioned with Identity
//! Center, read off the rpcv2Cbor wire the way a client reads it.
//!
//! `SearchPrincipals`'s own model doc states the precondition — "Searches Identity Center
//! for users and groups in a domain. The domain must be configured with Identity Center" —
//! and declares `ValidationException` (`@httpError 400`) for a request that fails it
//! (`smithy_query(query="operation", operation="SearchPrincipals")`). The model fixes no
//! message text, so the wording is recorded behaviour, and the RULE it follows is the one
//! this suite asserts:
//!
//! * the precondition is a property of the DOMAIN rather than of the request, so its
//!   refusal is ONE CONSTANT sentence — identical for every request in that state, and
//!   carrying no value taken from the request (no `domainId`, no domain name, no search
//!   term, no page size or token) and no operation name.
//!
//! The rows differ along everything of the request the refusal could have interpolated:
//! the domain the search addressed (each row provisions its own, so each carries a
//! different minted `domainId` and a different name), the search term's form (the
//! wildcard, a name, a single character, a term at `searchQuery`'s modelled 128-character
//! maximum) and the paging the request asked for (absent, both published bounds of
//! `maxResults`' `@range(min: 1, max: 100)`, and a resumed page). The recorded case is one
//! row of that table.
//!
//! [`a_domain_provisioned_with_identity_center_is_searched`] is the control: the same
//! request against a domain that IS configured with Identity Center is served, so the
//! refusal above is the precondition rather than a blanket failure of the operation.

use std::collections::BTreeSet;
use std::sync::Arc;

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::ShapeSerializer;
use aws_smithy_schema_server::CborCodec;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, Service,
};

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::create_domain::CreateDomain;
use omni::ops::types::{
    CreateDomainInput, SearchPrincipalsInput, SEARCH_PRINCIPALS_INPUT_SCHEMA,
};

use crate::validation_message_wording::error_envelope;

/// The error code this refusal names, as the model declares it with `@httpError 400`.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The status that error carries.
const VALIDATION_STATUS: u16 = 400;

/// The whole sentence a search of a domain that is not provisioned with Identity Center
/// earns, written out here so a change to the wording fails this suite rather than
/// following the implementation.
///
/// Wholly fixed text: it states what the domain is missing and the step that clears it,
/// and nothing of the request that ran into it.
const PRECONDITION_MESSAGE: &str = "Domain does not have an IdC application ARN configured. \
                                    The domain must be provisioned with Identity Center \
                                    before this operation can proceed.";

/// The region the suite signs for.
const REGION: &str = "us-west-2";

/// `IdentityProviderList` is `@length(1..2)`, `@uniqueItems`, over `IdentityProvider`
/// [IAM, IDC] (`smithy_query(query="shape", shape="IdentityProvider")`) — so `[IAM]` is
/// the only provider set a domain NOT configured with Identity Center can carry.
const WITHOUT_IDENTITY_CENTER: &[&str] = &["IAM"];

/// The provider set a domain configured with Identity Center carries.
const WITH_IDENTITY_CENTER: &[&str] = &["IDC"];

/// A SigV4 `Authorization` header whose credential scope names the region the handlers
/// record.
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

/// One emulator: the service mounted on a router over one shared engine, so the domain a
/// create persisted is the domain a search resolves.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    service: omni::EmulatedService<engine::InMemoryEngine>,
    router: MultiServiceRouter,
    parts: http::request::Parts,
}

impl Emulator {
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

    /// The request metadata a handler of this service reads.
    fn ctx(&self) -> OperationContext<'_> {
        OperationContext::new(
            &self.parts,
            self.service.definition().identity(),
            EmbeddedEmulator::from_router(self.router.clone()),
        )
    }

    /// A domain configured with `identity_providers`, created through the handler that owns
    /// it, so the `domainId` a row searches is one this store really minted.
    async fn domain(&self, case: &str, name: &str, identity_providers: &[&str]) -> String {
        CreateDomain::new(Arc::clone(&self.engine))
            .handle(
                CreateDomainInput {
                    name: name.to_string(),
                    identity_providers: identity_providers
                        .iter()
                        .map(|provider| (*provider).to_string())
                        .collect(),
                    client_token: Some(format!("token-{name}")),
                    ..Default::default()
                },
                &self.ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateDomain failed: {error:?}"))
            .domain
            .domain_id
    }

    /// Dispatch one `SearchPrincipals` request over the rpcv2Cbor wire and return the
    /// status and body, so a refusal is read the way a client reads it.
    async fn search_over_the_wire(&self, input: &SearchPrincipalsInput) -> (u16, bytes::Bytes) {
        let codec = CborCodec::default();
        let mut serializer = codec.create_serializer();
        serializer
            .write_struct(SEARCH_PRINCIPALS_INPUT_SCHEMA, input)
            .expect("the input shape serializes");
        let mut builder = http::Request::builder()
            .method("POST")
            .uri("/service/CloudWatchOmniFrontend/operation/SearchPrincipals")
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

/// One row of the table: the domain it provisions and the request it searches with.
struct Case {
    /// What the row is about, named in every assertion it makes.
    label: &'static str,
    /// The domain's name, which decides nothing about the refusal and so must not reach it.
    domain_name: &'static str,
    search_query: String,
    max_results: Option<i32>,
    /// Whether the term is long enough that finding it inside the message would really be
    /// an interpolation rather than a coincidence of one or two letters.
    term_is_distinctive: bool,
    /// Whether the request resumes a page, through a token this service itself minted.
    resumed: bool,
}

impl Case {
    fn new(label: &'static str, domain_name: &'static str, search_query: &str) -> Self {
        Self {
            label,
            domain_name,
            term_is_distinctive: search_query.len() >= 3,
            search_query: search_query.to_string(),
            max_results: None,
            resumed: false,
        }
    }

    fn paged(mut self, max_results: i32) -> Self {
        self.max_results = Some(max_results);
        self
    }

    fn resumed(mut self) -> Self {
        self.resumed = true;
        self
    }
}

#[tokio::test]
async fn the_identity_center_precondition_states_one_constant_message() {
    // `searchQuery` is `@length(1..128)` and `maxResults` `@range(min: 1, max: 100)`
    // (`smithy_query(query="operation", operation="SearchPrincipals")`), so the terms and
    // page sizes below are inside what the front door admits: every row reaches the
    // handler's precondition rather than the boundary's constraints.
    let cases = vec![
        Case::new("the wildcard term, no paging", "omni-wildcard", "*"),
        Case::new("an exact user name", "omni-named-user", "aisha").paged(1),
        Case::new("a single-character term", "omni-single", "z").paged(100),
        Case::new(
            "a term at searchQuery's length maximum",
            "omni-longest",
            &"q".repeat(128),
        )
        .paged(7),
        Case::new("a resumed page", "omni-resumed", "*").resumed(),
        Case::new("a substring of a display name", "omni-substring", "kumar").paged(2),
    ];

    let mut messages: BTreeSet<String> = BTreeSet::new();
    for case in &cases {
        // An account holds at most one domain, so each row runs against an emulator of its
        // own and therefore against a freshly minted, different `domainId`.
        let emulator = Emulator::new();
        let domain_id = emulator
            .domain(case.label, case.domain_name, WITHOUT_IDENTITY_CENTER)
            .await;
        let next_token = case
            .resumed
            .then(|| omni::state::encode_search_principals_page_token(3));
        let input = SearchPrincipalsInput {
            domain_id: domain_id.clone(),
            search_query: case.search_query.clone(),
            max_results: case.max_results,
            next_token,
        };

        let (status, body) = emulator.search_over_the_wire(&input).await;
        let (code, message) = error_envelope(case.label, &body);
        assert_eq!(
            (status, code.as_str()),
            (VALIDATION_STATUS, VALIDATION_EXCEPTION),
            "case {:?}: expected {VALIDATION_STATUS} {VALIDATION_EXCEPTION}, got {status} \
             {code:?}: {message:?}",
            case.label
        );
        assert_eq!(
            message, PRECONDITION_MESSAGE,
            "case {:?}: expected {PRECONDITION_MESSAGE:?}, got {message:?}",
            case.label
        );
        // Nothing of the request, and nothing of the operation that refused.
        for (what, value) in [
            ("the domain id", domain_id.as_str()),
            ("the domain name", case.domain_name),
            ("the operation name", "SearchPrincipals"),
        ] {
            assert!(
                !message.contains(value),
                "case {:?}: the refusal renders {what} ({value:?}): {message:?}",
                case.label
            );
        }
        if case.term_is_distinctive {
            assert!(
                !message.contains(&case.search_query),
                "case {:?}: the refusal renders the search term ({:?}): {message:?}",
                case.label,
                case.search_query
            );
        }
        if let Some(max_results) = case.max_results {
            assert!(
                !message.contains(&max_results.to_string()),
                "case {:?}: the refusal renders the page size ({max_results}): {message:?}",
                case.label
            );
        }
        // An id of this service's own form, whatever the row's domain is called.
        assert!(
            !message.contains("d-"),
            "case {:?}: the refusal renders a domain identifier: {message:?}",
            case.label
        );
        messages.insert(message);
    }

    assert_eq!(
        messages.len(),
        1,
        "every request in this state must read the SAME refusal, got {messages:?}"
    );
}

#[tokio::test]
async fn a_domain_provisioned_with_identity_center_is_searched() {
    // The control for the table above: the same request, against a domain that satisfies
    // the precondition, is served rather than refused.
    let emulator = Emulator::new();
    let domain_id = emulator
        .domain("provisioned", "omni-provisioned", WITH_IDENTITY_CENTER)
        .await;
    let (status, body) = emulator
        .search_over_the_wire(&SearchPrincipalsInput {
            domain_id,
            search_query: "*".to_string(),
            max_results: None,
            next_token: None,
        })
        .await;
    assert_eq!(
        status,
        200,
        "case \"provisioned\": a domain configured with Identity Center must be searched, \
         got {status}: {:?}",
        String::from_utf8_lossy(&body)
    );
}
