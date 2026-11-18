// SPDX-License-Identifier: CC0-1.0

//! The refusal a `nextToken` a list operation cannot resume from takes, end to end over
//! the restJson1 wire.
//!
//! The rule under test, one level above the one request that filed the issue:
//!
//! > A pagination token is OPAQUE: it carries a wrapped payload the service wrote, so a
//! > value the service did not write is refused with the operation's own modelled client
//! > error — AppSync's `BadRequestException` at HTTP 400 — and the two generations of this
//! > service's API state that refusal differently. A `/v1/…` list operation states ONE
//! > fixed sentence and nothing of the token or the call. The `/v2/apis…` surface surfaces
//! > the token decoder's own failure instead: the OPERATION the token was submitted to,
//! > and the decoder's diagnosis of the payload — the wrapping schema its leading 16 bits
//! > name. A token a listing itself handed back still resumes, on either surface.
//!
//! The rows differ along what the rule generalises over: the SURFACE the operation belongs
//! to, the OPERATION itself (and so the collection being paged), and the token's own
//! CONTENT — which is what the v2 diagnosis is computed from. The expected schema id of
//! each token is transcribed here, read off the payload its base64 characters spell, so
//! this suite states the diagnosis independently of the code that computes it.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, create_api, router};
use crate::event_api::event_config;

/// AppSync's own modelled error for a request that is not well formed, and its status
/// (`smithy_query(query="shape", shape="BadRequestException")`, `@httpError` 400).
const BAD_REQUEST: &str = "BadRequestException";
const BAD_REQUEST_STATUS: u16 = 400;

/// The v1 control plane's one fixed sentence for a token it cannot resume from.
const FIXED_SENTENCE: &str = "Invalid pagination token given.";

/// The tokens this suite submits, each with the wrapping schema its payload states.
///
/// A token's payload is the bytes its base64 characters spell (RFC 4648; characters
/// outside that alphabet carry no payload bits), and the schema id is the leading 16 of
/// those bits read as a signed value — so `"not…"` spells `0x9E8B`, i.e. `-24949`. Each
/// value below is that reading of the token beside it.
const TOKENS: &[(&str, i16)] = &[
    ("not-a-valid-token", -24949),
    ("notavalidtoken", -24949),
    ("bogus", 28296),
    ("garbage", -32342),
    ("zzz", -12484),
    ("AAAA", 0),
    ("0", -12288),
    // Shorter than the schema id spans, and characters the alphabet does not hold at
    // all: both still carry exactly one diagnosis.
    ("A", 0),
    ("...", 0),
];

/// The sentence the `/v2/apis…` surface refuses `token` submitted to `operation` with.
fn decoder_sentence(operation: &str, token: &str) -> String {
    let schema = TOKENS
        .iter()
        .find(|(value, _)| *value == token)
        .map_or_else(
            || panic!("case {token:?}: this suite carries no schema id for it"),
            |(_, schema)| *schema,
        );
    format!(
        "Pagination token exception in operation '{operation}': invalid payload wrapping \
         schema {schema}"
    )
}

/// One GraphQL API with two data sources, two functions and two API keys, and two Event
/// APIs each holding two channel namespaces — so every listing below has something to
/// page on either surface.
async fn fixture(router: &MultiServiceRouter, name: &str) -> (String, Vec<String>) {
    let api_id = create_api(router, name).await;
    for index in 0..2 {
        let created = call(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/datasources"),
            Some(json!({"name": format!("ds_{index}"), "type": "NONE"})),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case seed the data source {index}: expected 200, got {created:?}"
        );
        let function = call(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/functions"),
            Some(json!({
                "name": format!("fn_{index}"),
                "dataSourceName": format!("ds_{index}"),
                "functionVersion": "2018-05-29",
            })),
        )
        .await;
        assert_eq!(
            function.status, 200,
            "case seed the function {index}: expected 200, got {function:?}"
        );
        let key = call(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/apikeys"),
            Some(json!({})),
        )
        .await;
        assert_eq!(
            key.status, 200,
            "case seed the API key {index}: expected 200, got {key:?}"
        );
    }

    let mut event_apis = Vec::new();
    for index in 0..2 {
        let created = call(
            router,
            "POST",
            "/v2/apis",
            Some(json!({
                "name": format!("Events_{index}"),
                "eventConfig": event_config(
                    &["API_KEY"], &["API_KEY"], &["API_KEY"], &["API_KEY"], None,
                ),
            })),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case seed the Event API {index}: expected 200, got {created:?}"
        );
        let event_api_id = created.body["api"]["apiId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        for namespace in 0..2 {
            let made = call(
                router,
                "POST",
                &format!("/v2/apis/{event_api_id}/channelNamespaces"),
                Some(json!({"name": format!("ns-{namespace}")})),
            )
            .await;
            assert_eq!(
                made.status, 200,
                "case seed the channel namespace {namespace}: expected 200, got {made:?}"
            );
        }
        event_apis.push(event_api_id);
    }
    (api_id, event_apis)
}

/// RULE, on the v2 surface: the refusal names the operation the token was submitted to
/// and the decoder's diagnosis of the payload, for every token.
#[tokio::test]
async fn the_v2_surface_states_the_operation_and_the_decoders_diagnosis() {
    let router = router();
    let (_api_id, event_apis) = fixture(&router, "Api_token_diagnosis").await;

    let listings = [
        ("ListApis", "/v2/apis".to_string()),
        (
            "ListChannelNamespaces",
            format!("/v2/apis/{}/channelNamespaces", event_apis[0]),
        ),
    ];

    for (operation, base) in &listings {
        for (token, _) in TOKENS {
            let case = format!("{operation} resuming from {token:?}");
            let answer = call(&router, "GET", &format!("{base}?nextToken={token}"), None).await;
            assert_eq!(
                (answer.status, answer.error_type.as_str()),
                (BAD_REQUEST_STATUS, BAD_REQUEST),
                "case {case:?}: expected {BAD_REQUEST_STATUS} {BAD_REQUEST}, got {answer:?}"
            );
            let stated = decoder_sentence(operation, token);
            assert_eq!(
                answer.message, stated,
                "case {case:?}: expected {stated:?}, got {answer:?}"
            );
        }
    }
}

/// RULE, on the v1 surface: the same tokens take the one fixed sentence, which names
/// neither the operation nor the payload — so the diagnosis above belongs to the v2
/// surface rather than to this service's every listing.
#[tokio::test]
async fn the_v1_surface_states_its_one_fixed_sentence() {
    let router = router();
    let (api_id, _event_apis) = fixture(&router, "Api_token_fixed_sentence").await;

    let listings = [
        ("ListGraphqlApis", "/v1/apis".to_string()),
        ("ListDomainNames", "/v1/domainnames".to_string()),
        (
            "ListDataSources",
            format!("/v1/apis/{api_id}/datasources"),
        ),
        ("ListFunctions", format!("/v1/apis/{api_id}/functions")),
        ("ListApiKeys", format!("/v1/apis/{api_id}/apikeys")),
        ("ListTypes", format!("/v1/apis/{api_id}/types?format=SDL")),
    ];

    for (operation, base) in &listings {
        for (token, _) in TOKENS {
            let case = format!("{operation} resuming from {token:?}");
            let joiner = if base.contains('?') { '&' } else { '?' };
            let answer = call(
                &router,
                "GET",
                &format!("{base}{joiner}nextToken={token}"),
                None,
            )
            .await;
            assert_eq!(
                (answer.status, answer.error_type.as_str()),
                (BAD_REQUEST_STATUS, BAD_REQUEST),
                "case {case:?}: expected {BAD_REQUEST_STATUS} {BAD_REQUEST}, got {answer:?}"
            );
            assert_eq!(
                answer.message, FIXED_SENTENCE,
                "case {case:?}: expected the fixed sentence, got {answer:?}"
            );
            assert!(
                !answer.message.contains(operation) && !answer.message.contains("schema"),
                "case {case:?}: expected neither the operation nor the payload stated, \
                 got {answer:?}"
            );
        }
    }
}

/// RULE's other half: a token a listing itself handed back still resumes, on either
/// surface — so the refusals above are about the payload rather than about paging at all.
#[tokio::test]
async fn a_token_the_listing_handed_back_still_resumes() {
    let router = router();
    let (api_id, event_apis) = fixture(&router, "Api_token_resumes").await;

    for (operation, base, member) in [
        (
            "ListDataSources",
            format!("/v1/apis/{api_id}/datasources"),
            "dataSources",
        ),
        (
            "ListFunctions",
            format!("/v1/apis/{api_id}/functions"),
            "functions",
        ),
        (
            "ListApiKeys",
            format!("/v1/apis/{api_id}/apikeys"),
            "apiKeys",
        ),
        ("ListApis", "/v2/apis".to_string(), "apis"),
        (
            "ListChannelNamespaces",
            format!("/v2/apis/{}/channelNamespaces", event_apis[0]),
            "channelNamespaces",
        ),
    ] {
        let first = call(&router, "GET", &format!("{base}?maxResults=1"), None).await;
        assert_eq!(
            first.status, 200,
            "case {operation} first page: expected 200, got {first:?}"
        );
        let cursor = first.body["nextToken"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !cursor.is_empty(),
            "case {operation} first page: expected a nextToken while entries remain, got \
             {first:?}"
        );
        let resumed = call(
            &router,
            "GET",
            &format!("{base}?maxResults=1&nextToken={cursor}"),
            None,
        )
        .await;
        assert_eq!(
            resumed.status, 200,
            "case {operation} resuming from its own token: expected 200, got {resumed:?}"
        );
        assert_eq!(
            resumed.body[member]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            1,
            "case {operation} resuming from its own token: expected the next entry, got \
             {resumed:?}"
        );
    }
}
