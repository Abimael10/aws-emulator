Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 44bbf79358888366d10de6b934e7daf7fcf849e8

# The page-token entry branch (decode, then compare the query fingerprint) is open-coded in all nine paginated handlers

_Filed: 2026-11-15T21:36:17Z_
_Operation: ListRecommendedActions | Class: maintainability_

> _Note: this issue was identified against the code base at commit `44bbf79358888366d10de6b934e7daf7fcf849e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every paginated handler of this crate writes out the same token-entry decision by hand: no `nextToken` means offset 0; a token that does not decode is refused with this operation's "returned by a previous <Op> call" report; a token that decodes but carries a fingerprint other than the one this query would mint is refused with this operation's own mismatch requirement; otherwise the cursor's offset is taken. Three handlers (ListRecommendedActions, ListRecommendedActionSummaries, ListAutomationRules) carry that block character for character, mismatch message included; the other six repeat the same branch with only the requirement sentence changed — and ListAccounts has already DRIFTED structurally, folding the two refusals into one `match` guard instead of the two-step check its siblings use.

The crate's own convention is to single-home exactly this kind of shared rule: `crate::state` already owns `page_size` ("One rule for every paginated operation of this service"), `page_fingerprint`, `decode_page_token`, `encode_page_token` and `resume_page` ("so every paginated operation of this service performs ONE walk rather than a copy of it"). An earlier fix folded the page WALK out of these same handlers and explicitly treated `decode_page_token` as already single-sourced; the caller-side branch around it stayed duplicated nine times, so the copies can drift (one handler comparing the fingerprint before decoding, one forgetting the comparison, or one answering an empty page instead of a refusal would be a per-operation bug rather than a single-helper bug — and the ListAccounts copy shows drift has already begun).

A single helper in `crate::state` taking the optional token, the fingerprint this query would issue, and the two refusal wordings (or answering a two-variant outcome the handler maps to its own error type) reproduces each copy exactly: every response body and every existing test stays byte-for-byte unchanged, because the per-operation requirement sentences remain the handler's to supply.

## Evidence

Copy 1 — `services/service-com-amazonaws-computeoptimizerautomation-computeoptimizerautomationservice/src/ops/list_recommended_actions.rs:203-217`:
```
        let offset = match input.next_token.as_deref() {
            None => 0,
            Some(token) => {
                let Some(cursor) = crate::state::decode_page_token(token) else {
                    return Err(unissued_token());
                };
                if cursor.filter_fingerprint != filter_fingerprint {
                    return Err(invalid_parameter(crate::state::constraint_report(
                        "nextToken",
                        "Member must be a token returned for the same filters",
                    )));
                }
                cursor.offset
            }
        };
```
Copy 2 — `src/ops/list_recommended_action_summaries.rs:217-231`, the same block over the same filter list, identical down to the message:
```
        let offset = match input.next_token.as_deref() {
            None => 0,
            Some(token) => {
                let Some(cursor) = crate::state::decode_page_token(token) else {
                    return Err(unissued_token());
                };
                if cursor.filter_fingerprint != filter_fingerprint {
                    return Err(invalid_parameter(crate::state::constraint_report(
                        "nextToken",
                        "Member must be a token returned for the same filters",
                    )));
                }
                cursor.offset
            }
        };
```
Copy 3 — `src/ops/list_automation_rules.rs:191-205`: the same block again, same wording.

The same branch appears six more times with only the requirement sentence differing: `src/ops/list_automation_events.rs:212-226` ("...for the same filters and time range"), `src/ops/list_automation_event_summaries.rs:266-280` ("...for the same filters and dates"), `src/ops/list_automation_event_steps.rs:182-196` ("...for the same eventId"), `src/ops/list_automation_rule_preview.rs:232-247` and `src/ops/list_automation_rule_preview_summaries.rs:237-252` ("...for the same rule type, action types and criteria").

The ninth copy has already drifted into a different shape — `src/ops/list_accounts.rs:181-193`:
```
        let offset = match input.next_token.as_deref() {
            None => 0,
            Some(token) => match crate::state::decode_page_token(token) {
                Some(cursor) if cursor.filter_fingerprint == query_fingerprint => cursor.offset,
                _ => return Err(unissued_token()),
            },
        };
```

The crate states the single-home convention these copies violate — `src/state.rs:4158-4172`:
```
/// How many items one page of a paginated listing serves, from the caller's
/// `maxResults`.
///
/// One rule for every paginated operation of this service: ...
pub fn page_size(max_results: Option<i32>) -> usize {
```
and `src/state.rs:4177-4180`:
```
/// The walk itself, beside the rules [`page_size`], [`PageCursor`] and
/// [`encode_page_token`] already state, so every paginated operation of this
/// service performs ONE walk rather than a copy of it: ...
```

Test coverage that pins every response the fix must not change: `tests/all/page_token_refusals.rs:133-185` tables EVERY paginated operation with the exact per-operation mismatch requirement, and `tests/all/page_token_refusals.rs:216` (`an_unissued_page_token_is_refused_without_being_echoed`), `:249` (`a_page_token_taken_for_another_query_is_refused`) and `:276` (`a_minted_token_resumes_its_own_query_and_is_refused_by_any_other`) drive all nine over the wire; `tests/all/page_cursor_spellings.rs`, `tests/all/cross_operation_page_tokens.rs`, `tests/all/shared_page_walk.rs` and `tests/all/page_size_rule.rs` cover the decode spellings, the cross-operation refusal and the walk itself.

## Steps to Reproduce

1. Read src/ops/list_recommended_actions.rs:203-217 and src/ops/list_recommended_action_summaries.rs:217-231: the same decode/fingerprint-compare/offset branch, character for character including the "Member must be a token returned for the same filters" wording.
2. Find the same branch in src/ops/list_automation_rules.rs:191-205, src/ops/list_automation_events.rs:212-226, src/ops/list_automation_event_summaries.rs:266-280, src/ops/list_automation_event_steps.rs:182-196, src/ops/list_automation_rule_preview.rs:232-247 and src/ops/list_automation_rule_preview_summaries.rs:237-252 — only the requirement sentence differs.
3. Compare src/ops/list_accounts.rs:181-193, which spells the same decision as a single `match` guard rather than the two-step check: the copies have already diverged in structure.
4. Note that crate::state already single-homes every neighbouring rule of this walk (page_size at src/state.rs:4168, page_fingerprint, decode_page_token at src/state.rs:4133, resume_page at src/state.rs:4220), so the entry branch is the one part still duplicated.
5. Confirm tests/all/page_token_refusals.rs:133-185 asserts the exact body for each of the nine operations, so folding the branch into one helper that still takes each operation's requirement sentence leaves every response and test unchanged.
