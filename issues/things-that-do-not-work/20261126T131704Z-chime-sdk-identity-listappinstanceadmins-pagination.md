Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: c3aab41312baa47e690c4afe8f7c7d51a974747b

# ListAppInstanceAdmins can issue a NextToken longer than the modelled 2048, so the second page can never be fetched

_Filed: 2026-11-26T13:17:04Z_
_Operation: ListAppInstanceAdmins | Class: pagination_

> _Note: this issue was identified against the code base at commit `c3aab41312baa47e690c4afe8f7c7d51a974747b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

ListAppInstanceAdmins pages a map keyed by the caller-supplied `AppInstanceAdminArn`, and `crate::state::encode_page_token` builds the token by HEX-encoding the authenticated bytes of `"v1\n<scope>\n<cursor>"`, where both the scope (the operation name plus the `AppInstanceArn`) and the cursor (the admin ARN) travel VERBATIM. Hex doubles the byte count, so the issued token is roughly `2 * (32 + 3 + len(scope) + 1 + len(admin ARN))` characters.

`AppInstanceAdminArn` is a caller-chosen `ChimeArn` whose resource field the model bounds at 1024 characters (`[^/].{0,1023}$`), and `CreateAppInstanceAdmin` promotes a `<AppInstanceArn>/bot/<id>` ARN without requiring that any bot exist, so an admin ARN of ~1060 characters is accepted and becomes a page cursor. Paging such an instance with `max-results=1` (two promoted admins, the long one first in ARN order) issues a `NextToken` of ~2400 characters.

That token exceeds `NextToken`'s modelled `@length(min: 0, max: 2048)` on BOTH the output member and the `@httpQuery("next-token")` input member, so when the client hands it back the framework's boundary validation — which this crate states runs before the handler (state.rs:1125) — refuses the request with a validation error. The remaining administrators are then unreachable: the only cursor the listing offers is one the service itself will not accept.

The shared page-token module this crate already uses names this exact failure mode and carries the fixed-width scope stand-in that avoids it (`scope_digest`), which `encode_page_token` does not call. The authenticator must stay (a fixed issue requires it); only the size of the authenticated string is at fault.

## Evidence

Model (smithy_query / OPERATIONS.md:135-136, ListAppInstanceAdmins): input `NextToken` (string, len 0..2048, pattern .*, @httpQuery(next-token)), output `NextToken` (string, len 0..2048, pattern .*); and `AppInstanceAdminArn` (string, len 5..1600, pattern `^arn:[a-z0-9-\.]{1,63}:[a-z0-9-\.]{0,63}:[a-z0-9-\.]{0,63}:[a-z0-9-\.]{0,63}:[^/].{0,1023}$`, required) — a resource field of up to 1024 characters.

The cursor is the caller's ARN: services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/list_app_instance_admins.rs:212 `.map(|record| (record.admin_arn.clone(), record.clone()))` and :219 `let scope = format!("{}:{}", Self::NAME, input.app_instance_arn);`, passed to `crate::state::paginate(&administrators, input.max_results, after.as_deref(), &token_key, &scope, ...)` at :234-239.

The token carries both verbatim, in hex: src/state.rs:1085-1089
```
pub fn encode_page_token(token_key: &PageTokenKey, scope: &str, key: &str) -> String {
    // `\n` separates the fields: a scope is an operation name or an ARN and a key is
    // an ARN, so neither can contain one.
    ::hex::encode(token_key.mint(&format!("{TOKEN_VERSION}\n{scope}\n{key}")))
}
```
and src/state.rs:1154 `next_token: truncated.then(|| last_key.map(|key| encode_page_token(token_key, scope, key)))`. The only stated reason for the rendering is escaping, not size — src/state.rs:1082-1084: "Hex-coded rather than base64: the token travels back in a query string (`?next-token=...`), and hex needs no escaping there". Nothing in the crate justifies a token past the modelled maximum.

The crate states the boundary validator enforces the modelled constraints before a handler runs — src/state.rs:1125-1128: "Defence in depth, not validation: the boundary validator has already refused a `MaxResults` outside the model's `range(min: 1, max: 50)` by the time a handler runs" — so the over-long token is refused on the follow-up call.

The shared module already carries the remedy and documents this failure — services/common/src/page_tokens.rs:126-146 (`scope_digest`): "a scope travelling VERBATIM inside the token makes the token a service hands out grow with the request that asked for it, past the length the model declares for the member it travels on — and a page named by a token the service's own request walk refuses cannot be fetched at all. Digesting the scope is what keeps the SIZE of an issued token a property of the service rather than of the request."

Test coverage: tests/all/app_instance_admin_listing.rs and tests/all/app_instance_lifecycle.rs:1522-1527 exercise ListAppInstanceAdmins pagination only with short, emulator-minted admin ARNs, so no test pins the long-ARN cursor.

## Steps to Reproduce

1. CreateAppInstance to get <instance> (e.g. arn:aws:chime:us-east-1:000000000000:app-instance/<uuid>).
2. CreateAppInstanceAdmin on <instance> with AppInstanceAdminArn = "<instance>/bot/" + "a".repeat(960) — accepted 201 even though no such bot exists.
3. CreateAppInstanceAdmin on <instance> with a second, short admin ARN so the listing has two entries.
4. GET /app-instances/{<instance> url-encoded}/admins?max-results=1 — 200, and the returned NextToken is ~2400 characters, past NextToken's modelled 0..2048.
5. GET /app-instances/{<instance> url-encoded}/admins?max-results=1&next-token=<that token> — the boundary validator refuses the over-long NextToken with a 400 validation error, so the second page of administrators cannot be fetched.
