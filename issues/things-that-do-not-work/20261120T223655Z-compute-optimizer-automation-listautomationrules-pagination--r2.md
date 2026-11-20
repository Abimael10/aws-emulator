Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 769cbcc3fb9c7f64b5cd67434307cf1fcb979398

# A resumed ListAutomationRules page skips a live rule when an earlier rule is deleted between pages

_Filed: 2026-11-20T22:36:55Z_
_Operation: ListAutomationRules | Class: pagination_

> _Note: this issue was identified against the code base at commit `769cbcc3fb9c7f64b5cd67434307cf1fcb979398`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every paginated listing of this crate resumes a walk by a POSITIONAL offset into the current sequence: `walk_page` counts `offset` items off the front of the freshly-recomputed sequence and serves the next `page_size` from there. `RuleStore::remove` drops the deleted ARN from the `order` vector, so every rule after it shifts one position down. A `nextToken` minted before a DeleteAutomationRule therefore names a position that now denotes a DIFFERENT rule, and resuming the walk silently skips the rule that moved into the consumed prefix.

Concretely, with rules A, B, C in creation order and `maxResults: 1`: page 1 serves [A] and mints a cursor at offset 1; DeleteAutomationRule(A) leaves order [B, C]; the resumed page counts 1 item (B) as "already served" and serves [C]. B is never reported by any page of that walk, and the walk ends with no `nextToken`, which a client reads as the authoritative end of the result set. The cursor's fingerprint matches (the filters did not change), so the token is accepted rather than refused.

The crate's own page-walk contract is the opposite: the suite that pins the shared walk states that walking a listing yields "the same items, in the same order, with no item served twice and none skipped", and `resume_page`'s doc only contemplates the TAIL of a sequence being removed after a token was minted — not an item BEFORE the cursor. Nothing in the crate justifies the mid-sequence case; no test exercises a delete between two pages of one walk. The same positional cursor is used by every other listing here, but ListAutomationRules is the one whose sequence a modelled operation (DeleteAutomationRule) can shorten from the middle.

## Evidence

The positional resume, which counts the prefix off the CURRENT sequence:
`src/state.rs:4248`: `let before = items.by_ref().take(offset).count();`
`src/state.rs:4258`: `(page, next_token, before == offset)`

The mutation that shifts every later position:
`src/state.rs:1279` (in `RuleStore::remove`): `self.order.retain(|arn| arn != rule_arn);`
reached from `src/ops/delete_automation_rule.rs:243`: `guard.rules.remove(&input.rule_arn);`

The listing that pages that very sequence with the caller's cursor:
`src/ops/list_automation_rules.rs:203`: `cursor.offset`
`src/ops/list_automation_rules.rs:227-236`: `let served: Option<(Vec<AutomationRule>, Option<String>)> = crate::state::resume_page(guard.rules.iter_in_order().filter(...).map(crate::state::StoredRule::to_automation_rule), offset, page_size, filter_fingerprint);`

The rule the crate states for that walk, which the above breaks:
`tests/all/shared_page_walk.rs:5-10`: "The rule: a page is the slice of the already-selected sequence that starts where the previous page stopped and holds at most `maxResults` items, and a `nextToken` is carried exactly while items remain after it, resuming at the item the page stopped before. So walking a listing at ANY page size yields the same items, in the same order, with no item served twice and none skipped."

And the only removal `resume_page` accounts for is the tail:
`src/state.rs:4216-4219`: "The end itself is reachable — `offset` equal to the sequence's length serves its empty remainder — because that is where a walk whose tail was removed after the token was minted resumes."

Test coverage: `tests/all/shared_page_walk.rs` drives the walk over static sequences only (lines 294-360 table of lengths/offsets/page sizes); no test in `tests/all/` deletes an item between two pages of one walk (grep for `offset|removed after|mid-walk` across `tests/all` finds no such case).

## Steps to Reproduce

1. Opt the caller in: UpdateEnrollmentConfiguration {"status":"Active"}.
2. CreateAutomationRule three times (rules A, B, C), noting each `ruleArn` and `ruleRevision`.
3. ListAutomationRules {"maxResults":1} — observe page [A] and a `nextToken`.
4. DeleteAutomationRule {"ruleArn":<A>,"ruleRevision":<A's revision>}.
5. ListAutomationRules {"maxResults":1,"nextToken":<token from step 3>} — the page reports C and carries no `nextToken`; rule B, which is still stored and is returned by an unpaged ListAutomationRules, is reported by no page of the walk.
