Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: b2081c323ecd8661726294a0adb85fa67872f44a

# The page walk of the two gateway listings is implemented twice, so the two copies can drift

_Filed: 2026-11-19T00:14:55Z_
_Operation: ListResponderGateways | Class: maintainability_

> _Note: this issue was identified against the code base at commit `b2081c323ecd8661726294a0adb85fa67872f44a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

ListRequesterGateways and ListResponderGateways carry the same pagination LOGIC twice, line for line: the `maxResults` clamp with its two local constants, the sort into a total order by id, the `partition_point` resume from the decoded cursor, the end-of-page slice and the condition under which a `nextToken` is issued. The only differences are the store field read and the operation's error type. The crate already states the reason for not doing this once per listing — the token codec was centralised in `state/page_tokens.rs` precisely "so the token codec is stated once here rather than once per listing: two listings then cannot drift apart about what a token means" — and the same argument covers the walk the codec is used by: a fix to one copy (for example the expired-token error code, or the clamp) silently leaves the other behind. A shared helper taking the sorted id vector, the page size and the cursor keeps every response byte-identical.

## Evidence

Copy A, services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/list_requester_gateways.rs:181-220 plus the two constants at 224-230:
```
        let page_size = match input.max_results {
            requested if requested > 0 => {
                usize::try_from(requested.min(MAX_PAGE_SIZE)).unwrap_or(DEFAULT_PAGE_SIZE)
            }
            _ => DEFAULT_PAGE_SIZE,
        };
...
        let start = match resume_after.as_deref() {
            Some(cursor) => gateway_ids.partition_point(|id| *id <= cursor),
            None => 0,
        };
        let end = start.saturating_add(page_size).min(gateway_ids.len());
...
        let next_token = (end < gateway_ids.len())
            .then(|| {
                page.last()
                    .map(|cursor| crate::state::encode_next_token(Self::NAME, cursor, now))
            })
            .flatten();
```
Copy B, src/ops/list_responder_gateways.rs:178-212 plus the same two constants at 221-227 — character-for-character the same, with `store.responder_gateways.keys()` in place of `store.requester_gateways.keys()`.

The crate's own statement of the rule, src/state/page_tokens.rs:6-10: "Every listing of this service walks its records in a total order and names the page boundary with a cursor, so the token codec is stated once here rather than once per listing: two listings then cannot drift apart about what a token means".

Tests that must stay green and unchanged: tests/all/requester_gateway_listing.rs (page-size and token walk, `a_page_size_past_a_published_bound_is_refused` at 381, the walk at 311-345) and tests/all/responder_gateway_listing.rs (the same walk at 339-345, plus the cross-listing token refusal at 377).

## Steps to Reproduce

1. Read services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/list_requester_gateways.rs:181-230.
2. Read services/service-com-amazonaws-rtbfabric-rtbfabric/src/ops/list_responder_gateways.rs:178-227 and diff the two bodies: only the store field and the error type differ.
3. Note that a change to one page-walk rule (clamp, cursor comparison, token issuance condition) has to be made twice to keep the two listings in agreement.
