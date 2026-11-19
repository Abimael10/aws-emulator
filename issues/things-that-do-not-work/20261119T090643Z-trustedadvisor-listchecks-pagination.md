Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 1a2367ed6050fcb8453dc573f19b34d5cca12151

# A nextToken this service never issued is honoured as a cursor, although the crate documents that value as a MalformedToken refusal

_Filed: 2026-11-19T09:06:43Z_
_Operation: ListChecks | Class: pagination_

> _Note: this issue was identified against the code base at commit `1a2367ed6050fcb8453dc573f19b34d5cca12151`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every paginated read of this service resumes through `crate::state::decode_token`, which the crate documents as the test of whether the service ISSUED the token: "The key a cursor points at, or [`MalformedToken`] when the token is not one this service issued for `scope`" (src/state.rs:1561-1562), and the type it answers with is documented "A `nextToken` this service did not issue, or issued for a different list" (src/state.rs:1477).

The code under those two sentences tests something weaker: that the value is base64, that its bytes are UTF-8, and that the text before the first `\u{1f}` equals the scope (src/state.rs:1563-1574). Every one of those properties is reproducible by a caller:

* `encode_token` is plain base64 of `{scope}\u{1f}{last_key}` with no authenticator over it (src/state.rs:1557-1559), so base64-decoding ANY token the service hands back reveals the scope verbatim;
* the scope itself is built from the request the caller sends — `list_scope` over that read's own filters (src/state.rs:1529-1548) — so the caller already knows every input to it;
* the key half is unconstrained: `paginate` only uses it as `key > resume` (src/state.rs:1601-1604).

So a caller can mint a value for which `decode_token` answers `Ok(key)` with a key the service never offered, and the read answers 200 with a page starting after that key instead of the 400 `ValidationException` the crate says that value earns. That turns a documented refusal into a success, and it silently breaks the ONE guarantee `paginate` states — "a caller that follows the token until it is absent sees every entry exactly once" (src/state.rs:1579-1581) — because a forged cursor skips entries while the response looks like a normal (possibly final) page.

The condition is in the shared helper, so all seven paginated reads inherit it: ListChecks (src/ops/list_checks.rs:236-253), ListRecommendations, ListRecommendationResources, ListRecommendationsForResource, ListOrganizationRecommendations, ListOrganizationRecommendationAccounts, ListOrganizationRecommendationResources. Filed against ListChecks, whose scope is the simplest to state.

This repo already owns the distinction the doc claims, in a crate this one ALREADY depends on: `services/common/src/page_tokens.rs` exists to tell "a continuation token it ISSUED from one a caller built", and its header states this exact failure mode. `service-common` is a direct dependency and this crate already reaches into two of its modules (`service_common::arn::is_account_id` at src/state.rs:2299, `service_common::validation::with_submitted_values` at src/service/validation.rs:510), so nothing about the dependency graph is new.

Distinct from the two neighbouring issues: the FIXED `20261028T200344Z-trustedadvisor-listorganizationrecommendationresources-pagination` closed CROSS-scope forgery by making the scope an injective JSON document (which this does not touch or reverse — the JSON scope stays), and the OPEN `20261112T213623Z-trustedadvisor-listrecommendations-error-response` is about WHERE the token refusal is evaluated relative to the support-level gate, not about which values reach it.

## Evidence

What the crate says the function decides — services/service-com-amazonaws-trustedadvisor-trustedadvisor/src/state.rs:1561-1562:
```
/// The key a cursor points at, or [`MalformedToken`] when the token is not one
/// this service issued for `scope`.
```
and the type it answers with — src/state.rs:1477:
```
/// A `nextToken` this service did not issue, or issued for a different list.
```

What the code actually checks — src/state.rs:1563-1574 (shape only: base64, UTF-8, scope prefix):
```
pub fn decode_token(scope: &str, token: &str) -> Result<String, MalformedToken> {
    let bytes =
        ::aws_smithy_types::base64::decode(token).map_err(|_| MalformedToken::new(token))?;
    let decoded = String::from_utf8(bytes).map_err(|_| MalformedToken::new(token))?;
    let (token_scope, key) = decoded
        .split_once(TOKEN_SEPARATOR)
        .ok_or_else(|| MalformedToken::new(token))?;
    if token_scope != scope {
        return Err(MalformedToken::new(token));
    }
    Ok(key.to_string())
}
```

Why every one of those properties is caller-reproducible — src/state.rs:1557-1559, the minting side, which carries no authenticator:
```
pub fn encode_token(scope: &str, last_key: &str) -> String {
    ::aws_smithy_types::base64::encode(format!("{scope}{TOKEN_SEPARATOR}{last_key}"))
}
```

The guarantee that then holds only for issued tokens — src/state.rs:1576-1583:
```
/// `next_token` resumes strictly after the key it names, and the returned token
/// is present only when entries remain — so a caller that follows the token
/// until it is absent sees every entry exactly once. Both the token read and the
/// token written carry `scope`, so a cursor is only accepted by the list that
/// issued it.
```

The handler that turns the (never raised) rejection into the 400 — src/ops/list_checks.rs:236-253:
```
        let page = crate::state::paginate(
            &crate::state::list_scope(
                "checks",
                &[ ... ],
            ),
            matching,
            input.next_token.as_deref(),
            input.max_results,
        )
        .map_err(|malformed| {
            ListChecksErrorModeled::ValidationException(ValidationException {
                message: validation::page_token_refusal(malformed.token()),
            })
        })?;
```

The facility this repo already has for the distinction the doc claims, in a crate this one already depends on — services/common/src/page_tokens.rs:3-17:
```
//! The ONE way a service tells a continuation token it ISSUED from one a caller built.
//!
//! A paginated read hands back an opaque `NextToken` ... so a read that only checks the
//! token's SHAPE honours any value of that shape, and a caller can name a position the
//! service never offered: the first page of a report it was paging past, a position far
//! in the future, a position under another filter. What the services' APIs document for
//! such a value is a refusal (an invalid-token error), not a page.
//!
//! So a token carries an AUTHENTICATOR over its own cursor ...
```
and the dependency that makes it reachable — services/service-com-amazonaws-trustedadvisor-trustedadvisor/Cargo.toml:13:
```
service-common = { path = "../common" }
```

Test coverage, which leaves this case open: tests/all/page_token_refusal_sentence.rs:175-197 enumerates the refused values — "four characters outside the base64 alphabet", "base64 of bytes that are not text", "base64 of text carrying no cursor", a 1000-character value, and a cursor ANOTHER list issued — but no row sends base64 of THIS read's own scope plus a key the service never offered. tests/all/cursor_scope_encoding.rs:25-35 pins only that two different (list, filter set) pairs cannot encode to one scope, i.e. cross-scope replay; tests/all/recommendation_loop.rs:373-469 drives only tokens the reads issued.

## Steps to Reproduce

1. Read src/state.rs:1561-1562 and src/state.rs:1477: `decode_token` is documented to answer `MalformedToken` "when the token is not one this service issued for `scope`", and that type is "A `nextToken` this service did not issue, or issued for a different list".
2. Read src/state.rs:1557-1559: a token is plain base64 of `{scope}\u{1f}{last_key}`, with nothing over it that only the service can produce.
3. Start the emulator with EMULATOR_TRUSTEDADVISOR_SUPPORT_PLAN=business and call GET /v1/checks?maxResults=1. Base64-decode the returned nextToken: the plaintext is the read's whole scope, `["checks",[]]`, then \u{1f}, then the last check id.
4. Mint a value the service never issued: base64 of `["checks",[]]` + "\u{1f}" + "zzz" (any key will do — the service offered none of them). It satisfies the modelled `@length(4, 10000)`, so it reaches the handler.
5. Send GET /v1/checks?nextToken=<that value>. Observe 200 with `checkSummaries: []` and no `nextToken` — `decode_token` returned Ok("zzz") and `paginate` resumed after a key nobody issued (src/state.rs:1601-1604), where the quoted doc says the value is a `MalformedToken` and the handler (src/ops/list_checks.rs:249-253) would answer 400 `ValidationException`.
6. Repeat with a key in the middle of the catalog (e.g. base64 of `["checks",[]]\u{1f}iam-access-key-rotation`): the read answers 200 with a page that silently omits every earlier check, so following tokens no longer walks every entry exactly once (src/state.rs:1579-1581).
7. Repeat against /v1/recommendations, /v1/recommendations/{arn}/resources, /v1/recommendations-for-resource/{arn}, /v1/organization-recommendations, /v1/organization-recommendations/{arn}/accounts and /v1/organization-recommendations/{arn}/resources with each read's own decoded scope: the same helper serves all seven.
8. Confirm the repo already owns this distinction and the dependency is in place: services/common/src/page_tokens.rs:3-17 and Cargo.toml:13 (service-common is already used at src/state.rs:2299 and src/service/validation.rs:510).
