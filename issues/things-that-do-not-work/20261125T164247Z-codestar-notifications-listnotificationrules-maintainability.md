Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 6efdcc2c184ddba11bf2d42c3ae06c16d4d6253e

# The page-slicing and next-token computation is copied verbatim into all three listing handlers

_Filed: 2026-11-25T16:42:47Z_
_Operation: ListNotificationRules | Class: maintainability_

> _Note: this issue was identified against the code base at commit `6efdcc2c184ddba11bf2d42c3ae06c16d4d6253e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The three paginated handlers of this crate each carry their own copy of the same two blocks of pagination LOGIC: (a) decode `NextToken` or start at 0, answering `InvalidNextTokenException` with `st::invalid_next_token(token)` when it does not decode, and (b) `has_more = matching.len() > page_size`, take `page_size` items, and mint the next token as `encode_page_token(<last item's position> + 1)`. `crate::state` already owns the primitives the copies share (`decode_page_token`, `encode_page_token`, `page_size`, `invalid_next_token`), but the rule that combines them — resume one position past the item the page ended on, and emit a token only when something was left over — lives three times in the handlers, so the copies can drift (they already differ in their default page size, which `list_event_types.rs:94-97` documents as deliberate). Collapsing the shared computation into one helper in `state.rs` (the module that already hosts the token codec, and whose doc at `src/state.rs:5-9` says it keeps "the small set of helpers (ARNs, ids, page tokens, request fingerprints) the handlers share") leaves every response and every existing test unchanged.

## Evidence

Copy 1 — `src/ops/list_notification_rules.rs:204-212`:
```
let has_more = matching.len() > page_size;
let page: Vec<(u64, NotificationRuleSummary)> =
    matching.into_iter().take(page_size).collect();
// Resume one position past the rule this page ended on, so the next page starts
// exactly where this one stopped.
let next_token = match (has_more, page.last()) {
    (true, Some((sequence, _))) => Some(st::encode_page_token(sequence + 1)),
    _ => None,
};
```
Copy 2 — `src/ops/list_targets.rs:208-215`:
```
let has_more = matching.len() > page_size;
let page: Vec<(u64, TargetSummary)> = matching.into_iter().take(page_size).collect();
// Resume one position past the target this page ended on, so the next page starts
// exactly where this one stopped.
let next_token = match (has_more, page.last()) {
    (true, Some((sequence, _))) => Some(st::encode_page_token(sequence + 1)),
    _ => None,
};
```
Copy 3 — `src/ops/list_event_types.rs:218-225`:
```
let has_more = matching.len() > page_size;
let page: Vec<(usize, EventTypeSummary)> = matching.into_iter().take(page_size).collect();
// Resume one position past the entry this page ended on, so the next page starts
// exactly where this one stopped.
let next_token = match (has_more, page.last()) {
    (true, Some((position, _))) => Some(st::encode_page_token(*position as u64 + 1)),
    _ => None,
};
```
The token-decode branch is likewise triplicated: `src/ops/list_notification_rules.rs:156-169`, `src/ops/list_targets.rs:159-172`, `src/ops/list_event_types.rs:174-187` — all three `match st::decode_page_token(token)` and answer `InvalidNextTokenException { message: Some(st::invalid_next_token(token)) }`.
Existing tests that pin the behaviour the refactor must preserve: `tests/all/event_type_catalog_and_next_token.rs` (catalog paging and next-token rules), `tests/all/list_targets.rs:423-455` (target paging with filters), `tests/all/list_event_types.rs`, and the listing suites in `tests/all/notification_rule_lifecycle.rs`; plus the unit test `a_page_token_round_trips_and_a_foreign_one_does_not_decode` at `src/state.rs:951`.

## Steps to Reproduce

1. Open src/ops/list_notification_rules.rs:204, src/ops/list_targets.rs:208 and src/ops/list_event_types.rs:218 side by side: the same has_more / take / encode_page_token(last+1) computation appears three times.
2. Open src/ops/list_notification_rules.rs:156, src/ops/list_targets.rs:159 and src/ops/list_event_types.rs:174: the same decode-or-InvalidNextToken branch appears three times.
3. Change the resume rule in one copy only (e.g. drop the `+ 1`) and observe that the other two listings keep the old rule — the drift the duplication allows.
