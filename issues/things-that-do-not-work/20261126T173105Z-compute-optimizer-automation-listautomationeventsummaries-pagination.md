Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: a93a38b586f08647bd94440a2511167aed598e08

# A resumed ListAutomationEventSummaries page skips a status group — and under-counts the one it served — because the cursor is a position into a group sequence the handler's own clock settlement re-derives on every call

_Filed: 2026-11-26T17:31:05Z_
_Operation: ListAutomationEventSummaries | Class: pagination_

> _Note: this issue was identified against the code base at commit `a93a38b586f08647bd94440a2511167aed598e08`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListAutomationEventSummaries` pages over a DERIVED sequence: the status groups `group_events_by_event_status` builds, ordered by where the first event carrying each `eventStatus` sits in event-creation order. The `nextToken` it mints is a bare POSITION into that sequence (`PageCursor::offset`), and `walk_page` resumes by counting `offset` items off the front of the sequence as it is re-derived on the NEXT call.

But the handler itself changes which group an event belongs to, on every call, before it groups: `guard.events.settle_due(now)` materialises the status transition the clock owed each event. `EVENT_SETTLEMENT_DELAY` is 500 ms, so between page 1 and page 2 of any real walk an `InProgress` event becomes `Complete`, its old group can vanish and the surviving groups shift position. Unlike the already-open ListAutomationRules cursor issue — which reasons that "ListAutomationRules is the one whose sequence a modelled operation (DeleteAutomationRule) can shorten from the middle" — this needs NO caller mutation at all: only time passing, which is guaranteed.

Two observable failures in one walk, both of which the crate's own page-walk contract forbids:

1. A group is skipped. With E1 `InProgress` (created first) and E2 already `Complete`, the sequence is [InProgress, Complete]. Page 1 at `maxResults:1` serves the `InProgress` row and mints a cursor at offset 1. Once E1 settles, the sequence is the single [Complete] group. Page 2 at offset 1 makes `before == offset` (1 == 1), so `resume_page` answers `Some` with an EMPTY page and no `nextToken` — which a client reads as the authoritative end of the result set. The `Complete` group, which an unpaged call reports, is reported by no page of the walk.

2. The group the walk DID serve under-counts. Served at page 1, the `InProgress` row counted 1 event; by the time the walk ends, the window holds two `Complete` events and zero `InProgress` ones. A caller that accumulates the pages of one walk — which is the only way to read this summary at a page size below the number of status groups — gets a total of 1 for a window holding 2 events, while a single unpaged call reports 2.

The query fingerprint does not save the caller: it digests only `canonical_filters(&filters)` and the two RAW request dates (`instant_key`), none of which changed, so the stale cursor is ACCEPTED rather than refused. A secondary path shifts the same sequence with no mutation either: `window_start` is `effective_window_start(start_date, now)`, re-evaluated per call and never part of the fingerprint, so an event sitting near the one-year retrieval floor drops out of the selection between two pages of one walk and shifts every later group down a position.

## Evidence

The handler mutates every event's grouping dimension, then groups and pages the result, all in one call:
`src/ops/list_automation_event_summaries.rs:310-313`: "// Before summarising, materialise the status transitions the clock has / // already made due, so this reader and `GetAutomationEvent` agree on / // every event's status — which is the dimension the rows are grouped by." followed by `guard.events.settle_due(now);`
`src/ops/list_automation_event_summaries.rs:319-331`: `let grouped = crate::state::group_events_by_event_status(\n    guard\n        .events\n        .iter_in_order()\n        .filter(|event| { crate::state::within_window(event.created_timestamp, Some(window_start), end_date) })\n        .filter(|event| crate::state::event_matches_filters(event, &filters)),\n);`
`src/ops/list_automation_event_summaries.rs:351-355`: `let served: Option<(Vec<crate::state::ActionGroup>, Option<String>)> =\n    crate::state::resume_page(grouped, offset, page_size, query_fingerprint);\nlet Some((groups, next_token)) = served else {\n    return Err(unissued_token());\n};`

The group sequence is ordered by a value `settle_due` rewrites, so the order is not stable across calls:
`src/state.rs:1918-1919` (`group_events_by_event_status`): "The automation events of `events`, grouped by `eventStatus`: one group per status the set holds, in the order the set first holds an event in it."
`src/state.rs:1932`: `|event| Some(event.event_status.as_str()),`

The write that changes that value, due on the clock alone:
`src/state.rs:2489-2492` (`EventStore::settle_due`): `pub fn settle_due(&mut self, now: SystemTime) {\n    for event in self.by_id.values_mut() {\n        event.settle(now);`
`src/state.rs:2289`: `self.event_status = pending.terminal_status;`
`src/state.rs:212`: `pub const EVENT_SETTLEMENT_DELAY: Duration = Duration::from_millis(500);`

The positional resume that indexes the re-derived sequence, and the branch that turns a shifted cursor into a clean "end of results":
`src/state.rs:4357`: `let before = items.by_ref().take(offset).count();`
`src/state.rs:4367`: `(page, next_token, before == offset)`
`src/state.rs:4338-4339` (`resume_page`): `let (page, next_token, reached) = walk_page(selected, offset, page_size, filter_fingerprint);\n    reached.then_some((page, next_token))`

The cursor carries nothing but the position and a fingerprint of the unchanged request members, so the stale token is accepted:
`src/state.rs:4162-4165`: `pub struct PageCursor {\n    pub offset: usize,\n    pub filter_fingerprint: String,\n}`
`src/ops/list_automation_event_summaries.rs:258-265`: `let query_fingerprint = crate::state::page_fingerprint(\n    Self::NAME,\n    &(\n        crate::state::canonical_filters(&filters),\n        start_date.map(crate::state::instant_key),\n        end_date.map(crate::state::instant_key),\n    ),\n);`

The requirement this breaks, stated by the crate itself — once as the contract of the shared walk, once as the reason a sibling listing imposes a stable order:
`tests/all/shared_page_walk.rs:5-10`: "The rule: a page is the slice of the already-selected sequence that starts where the previous page stopped and holds at most `maxResults` items, and a `nextToken` is carried exactly while items remain after it, resuming at the item the page stopped before. So walking a listing at ANY page size yields the same items, in the same order, with no item served twice and none skipped."
`src/state.rs:3546-3549` (`accounts_in_order`): "The store is a map, whose iteration order is its own business, so the ORDER is imposed here: ascending account id is the same sequence on every call, which is what paging over it requires — the pages of one walk must be windows onto ONE sequence rather than samples of a shifting one."

Test coverage: nothing pins or justifies the shifting case. The one pagination test of this operation seeds events whose status the clock can never change, so `settle_due` is a no-op for its whole walk:
`tests/all/automation_event_summaries.rs:966-996`: `async fn paging_walks_every_group_exactly_once() { ... let events: Vec<StoredEvent> = statuses.iter().enumerate().map(|(index, status)| { event(30 + index, "111111111111", "UpgradeEbsVolumeType", status, base - 7200 + ..., None) }).collect();\n    seed(&engine, &events);`
`tests/all/automation_events.rs:184-186` (the fixture those events are built on): "// A seeded event is already in the status it names, so the clock owes it / // no transition." followed by `pending: None,`
So the walk is only ever driven over a static sequence; no test in `tests/all/` takes a second page of this listing across a settlement, and the already-open ListAutomationRules cursor issue explicitly scopes itself to the sequence a modelled operation shortens, not to this one.

## Steps to Reproduce

1. UpdateEnrollmentConfiguration {"status":"Active"} — opens the enrollment gate and records the caller's analysed recommended-action inventory.
2. ListRecommendedActions {} — note two recommendedActionIds, call them R1 and R2.
3. StartAutomationEvent {"recommendedActionId":<R1>} — the event E1 is stored InProgress, created first.
4. Wait more than EVENT_SETTLEMENT_DELAY (500 ms) so E1's Complete status falls due, then StartAutomationEvent {"recommendedActionId":<R2>} — E2 is stored InProgress, created second.
5. ListAutomationEventSummaries {"maxResults":1} — the sequence of groups is [Complete (E1), InProgress (E2)]; the page serves the Complete row with automationEventCount 1 and a nextToken at offset 1.
6. Wait more than 500 ms so E2 settles to Complete as well.
7. ListAutomationEventSummaries {"maxResults":1,"nextToken":<the token from step 5>} — the token is accepted (filters and dates are unchanged, so the fingerprint matches), but the sequence now holds the single Complete group, offset 1 equals its length, and the response is an EMPTY automationEventSummaries list with no nextToken: a client reads that as the end of the walk.
8. ListAutomationEventSummaries {} — the unpaged call reports one Complete row with automationEventCount 2. The two-page walk of steps 5 and 7 reported a total of 1 and never reported the group E2 ended up in, contradicting "no item served twice and none skipped".
9. Reverse the creation order (start R2's event first, let it settle, then start R1's) to see the skip from the other side: page 1 serves the InProgress row, and by page 2 the Complete group has moved from offset 1 to offset 0, so it is never served at all.
