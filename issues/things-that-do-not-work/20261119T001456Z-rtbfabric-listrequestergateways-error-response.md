Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: b2081c323ecd8661726294a0adb85fa67872f44a

# An expired nextToken is answered with ValidationException where the model documents HTTP 400 InvalidToken

_Filed: 2026-11-19T00:14:55Z_
_Operation: ListRequesterGateways | Class: error-response_

> _Note: this issue was identified against the code base at commit `b2081c323ecd8661726294a0adb85fa67872f44a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The `nextToken` member's own documentation states which error an expired pagination token is answered with: an HTTP 400 `InvalidToken` error. The handler decodes the token through the shared codec, which distinguishes expiry (`TokenRefusal::Expired`) from a token it never issued, and then maps BOTH refusals onto the modelled `ValidationException`, so the wire `__type` a client matches on is `ValidationException` and never `InvalidToken`. The crate already honours the other half of that same documentation sentence — `TOKEN_LIFETIME` is 24 hours precisely because of it — and it already has the mechanism for answering a code outside an operation's modelled error set (`Scaffolded::unmodeled`, used by the tagging surface), so the distinction is reachable without new machinery. ListResponderGateways maps the refusal identically at list_responder_gateways.rs:161-170.

## Evidence

Smithy, `smithy_query(query="doc", shape="ListRequesterGatewaysRequest", member="nextToken")` — verbatim: "... Each pagination token expires after 24 hours. Using an expired pagination token will return an <i>HTTP 400 InvalidToken error</i>."

Handler, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/list_requester_gateways.rs:164-173:
```
        let resume_after = match input.next_token.as_deref() {
            Some(token) => Some(
                crate::state::decode_next_token(Self::NAME, token, now).map_err(|refusal| {
                    ListRequesterGatewaysErrorModeled::ValidationException(ValidationException {
                        message: refusal.message(Self::NAME),
                    })
                })?,
            ),
```
The codec already separates the expired case, src/state/page_tokens.rs:32-52: `Expired,` / "nextToken has expired: a {operation} pagination token expires {} hours after it is issued", and page_tokens.rs:12-15 cites the same sentence: "The `nextToken` member's own documentation states that each pagination token expires after 24 hours, so this span is the service's rather than the emulator's choice."
The crate's existing way to answer an unmodelled code, src/scaffolded.rs:278-282 `pub fn unmodeled(code: &'static str, status: u16, message: impl Into<String>)`, used at src/state/tagging.rs:66-72 (`tagging_unresolved`).

Test coverage: tests/all/requester_gateway_listing.rs:345-377 asserts only `status == 400` for malformed tokens and never asserts the error code, and no test exercises an expired token — so nothing pins `ValidationException` on this path (the status stays 400 either way).

## Steps to Reproduce

1. Create one or more requester gateways so the listing has records.
2. Build a token this operation would have issued at the epoch: hex-encode the string "ListRequesterGateways\u{1f}rtb-gw-0000000000000001\u{1f}0" (operation, cursor and issuing instant, the encoding of `encode_next_token`).
3. GET /requester-gateways?nextToken=<that hex>.
4. Observed: HTTP 400 with `__type` `ValidationException` and message "nextToken has expired: a ListRequesterGateways pagination token expires 24 hours after it is issued". Expected per the documented trigger: HTTP 400 with the `InvalidToken` error code.
