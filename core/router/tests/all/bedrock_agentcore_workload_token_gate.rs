// SPDX-License-Identifier: CC0-1.0

//! The WORKLOAD-IDENTITY-TOKEN GATE of the AgentCore Identity first-party exchange
//! `GetResourceApiKey`, beyond the rows `bedrock_agentcore_resource_api_key.rs` already
//! pins — whose harness this suite drives, so the emulator is mounted in one place only.
//!
//! RULE: the `workloadIdentityToken` a first-party call carries is a credential presented in
//! the request BODY, so a value no live issuance of this service stands behind is the
//! authorization verdict `AccessDeniedException` (HTTP 403) with the fixed sentence
//! `Invalid workload access token` — decided before anything the request named is resolved,
//! recording nothing, and whether or not the credential provider the call names exists. The
//! `UnauthorizedException` (HTTP 401) the operation also models is scoped to the caller's own
//! inbound OAuth bearer credential — "the JWT bearer token is invalid or not found for OAuth
//! bearer token based access" (source: smithy_query query="shape"
//! shape="UnauthorizedException") — which this call presents none of, so the gate never
//! answers it.
//!
//! What these rows add to the sibling suite's: the FORM of the presented value at both
//! published bounds of `WorkloadIdentityTokenType`'s 1..131072 length, a token naming another
//! Region's issuing authority and one minted for the workload itself, each crossed with a
//! provider that EXISTS and a name no provider exists under; that a refused exchange records
//! no issuance; and that the one call refused with a bogus credential answers the key the
//! provider holds when the credential is live, so the gate's place in the order is pinned
//! from both sides.

use crate::bedrock_agentcore_resource_api_key::{
    issuance_count, token_minted_nowhere, Emulator, REGION, USER,
};

/// A second Region, whose issuing authority a presented token can name.
const OTHER_REGION: &str = "eu-west-1";

/// The fixed text AWS refuses this gate with, spelled out here rather than read off the
/// emulator so the assertion states the expectation instead of restating the code.
const REFUSED_MESSAGE: &str = "Invalid workload access token";

/// The sentence AWS refuses a credential provider the account does not hold with — the
/// refusal a call that got PAST the gate earns.
const PROVIDER_NOT_FOUND: &str = "Resource Credential Provider not found";

/// The published bounds of `WorkloadIdentityTokenType` (source: smithy_query
/// query="operation" operation="GetResourceApiKey": input `workloadIdentityToken` @length).
const TOKEN_MIN_LENGTH: usize = 1;
const TOKEN_MAX_LENGTH: usize = 131_072;

/// The provider that EXISTS throughout, the key it holds, and a name the account holds no
/// provider under.
const PROVIDER: &str = "gate-partner-api";
const PROVIDER_KEY: &str = "sk-live-gate-0001";
const NO_SUCH_PROVIDER: &str = "no-provider-of-this-name";

/// Every presented value no live issuance stands behind earns the one authorization verdict
/// of this gate with its fixed sentence — for a provider that exists as much as for one that
/// does not, so the refusal is about the credential and discloses nothing about the account.
#[tokio::test]
async fn the_gate_refuses_every_token_no_live_issuance_backs_with_access_denied() {
    let emulator = Emulator::new();
    emulator
        .create_managed_provider("the provider that exists", PROVIDER, PROVIDER_KEY)
        .await;
    let issuances_before = issuance_count(emulator.engine());

    let presented: Vec<(&str, String)> = vec![
        (
            "a token of this plane's form for the workload itself",
            token_minted_nowhere(REGION, None, 102),
        ),
        (
            "a token naming another Region's issuing authority",
            token_minted_nowhere(OTHER_REGION, Some(USER), 103),
        ),
        (
            "the shortest value the shape admits",
            "z".repeat(TOKEN_MIN_LENGTH),
        ),
        (
            "the longest value the shape admits",
            "z".repeat(TOKEN_MAX_LENGTH),
        ),
    ];

    for (case, token) in presented {
        for (provider_case, provider) in [
            ("a provider that exists", PROVIDER),
            ("a name no provider exists under", NO_SUCH_PROVIDER),
        ] {
            let answer = emulator.resource_api_key(&token, provider).await;
            assert_eq!(
                answer.status, 403,
                "case {case:?} with {provider_case:?}: expected HTTP 403, got {}",
                answer.status
            );
            assert_eq!(
                answer.error_code(),
                "AccessDeniedException",
                "case {case:?} with {provider_case:?}: expected AccessDeniedException, got {:?}",
                answer.error_code()
            );
            assert_eq!(
                answer.message(),
                REFUSED_MESSAGE,
                "case {case:?} with {provider_case:?}: expected {REFUSED_MESSAGE:?}, got {:?}",
                answer.message()
            );
            // Fixed text: the sentence carries no part of the request, and never the key.
            assert!(
                !answer.message().contains(&token)
                    && !answer.message().contains(provider)
                    && !answer.message().contains(PROVIDER_KEY),
                "case {case:?} with {provider_case:?}: the refusal must carry no part of the \
                 request or of the provider's key, got {:?}",
                answer.message()
            );
        }
    }

    // The gate is a READ of the issuances the mints recorded: a refused exchange records
    // none of its own.
    assert_eq!(
        issuance_count(emulator.engine()),
        issuances_before,
        "a refused exchange must record no issuance of its own"
    );
}

/// The gate decides BEFORE the provider the request named is resolved, and a token the
/// identity plane has just minted gets past it: the same call that is refused with a bogus
/// credential is answered with the key the provider holds when the credential is live.
#[tokio::test]
async fn the_gate_decides_before_the_provider_and_a_live_token_gets_past_it() {
    let emulator = Emulator::new();
    emulator
        .create_managed_provider("the provider that exists", PROVIDER, PROVIDER_KEY)
        .await;
    let live = emulator.token_for_user("a live mint").await;

    // A live credential, the provider that exists: the key that provider holds.
    let answer = emulator.resource_api_key(&live, PROVIDER).await;
    assert_eq!(
        answer.status, 200,
        "a live credential must be served, got {answer:?}"
    );
    assert_eq!(
        answer.text("apiKey"),
        PROVIDER_KEY,
        "a live credential must answer the key the provider holds, got {:?}",
        answer.text("apiKey")
    );

    // A live credential, a name no provider exists under: the provider refusal, which is
    // what proves the gate accepted the credential rather than short-circuiting the call.
    let answer = emulator.resource_api_key(&live, NO_SUCH_PROVIDER).await;
    assert_eq!(
        answer.status, 404,
        "a live credential naming no provider must be the provider refusal, got {answer:?}"
    );
    assert_eq!(
        answer.error_code(),
        "ResourceNotFoundException",
        "expected ResourceNotFoundException, got {:?}",
        answer.error_code()
    );
    assert_eq!(
        answer.message(),
        PROVIDER_NOT_FOUND,
        "expected the fixed sentence of the provider refusal, got {:?}",
        answer.message()
    );

    // A bogus credential naming the SAME existing provider: the gate answers first, so the
    // caller learns nothing about the provider and is handed no key.
    let answer = emulator.resource_api_key("not-a-token", PROVIDER).await;
    assert_eq!(
        answer.status, 403,
        "a bogus credential must be refused by the gate, got {answer:?}"
    );
    assert_eq!(
        answer.error_code(),
        "AccessDeniedException",
        "expected AccessDeniedException, got {:?}",
        answer.error_code()
    );
    assert_eq!(
        answer.message(),
        REFUSED_MESSAGE,
        "expected {REFUSED_MESSAGE:?}, got {:?}",
        answer.message()
    );
    assert!(
        !answer.message().contains(PROVIDER_KEY) && answer.text("apiKey").is_empty(),
        "a refused exchange must report no key at all, got {answer:?}"
    );
}
