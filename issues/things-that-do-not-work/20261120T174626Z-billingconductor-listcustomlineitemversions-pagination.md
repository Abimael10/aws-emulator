Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 0fd0c8a38859f6782ef39e69b19293525b727c9f

# The version listing pages on a POSITIONAL index, so a write that drops a generation makes a resumed page skip the live version

_Filed: 2026-11-20T17:46:26Z_
_Operation: ListCustomLineItemVersions | Class: pagination_

> _Note: this issue was identified against the code base at commit `0fd0c8a38859f6782ef39e69b19293525b727c9f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every other paginated read of this crate cuts its page over a STABLE identity of the element (an ARN, an account id, the row's own attribute values) and says so, because the resume filter in `paginate_bounded` keeps the elements whose key is `>= cursor`. `ListCustomLineItemVersions` is the one listing that keys the page on the element's POSITION in the history (`format!("{index:010}")`), while the history it indexes is not append-only: a write that leaves a generation applying in no billing period DROPS that generation out of the middle/front of the list (`superseded.retain(CustomLineItemVersion::applies_anywhere)` in `CustomLineItemRecord::remove_periods`, and the same retain plus `kept.remove(0)` at the version cap in `put_custom_line_item`). Every index after the dropped generation then shifts down by one, so the `NextToken` a first page handed back no longer names the generation it was minted for: the token still decodes (its scope is only the operation and the item ARN), and the listing serves a page from the WRONG cut point — in the repro below the live, current generation is silently omitted from the resumed page and a caller paging the history never sees it. The handler's own comment claims the opposite property for this key.

## Evidence

The comment and the key it is written over, contradicting each other — `src/ops/list_custom_line_item_versions.rs:320-331`: "// Oldest generation first, the order the history was written in, so a `NextToken` names the same cut point on the next call." … "// The pagination key is the generation's position in that order, written fixed-width so it orders as a string the way it orders as a number." / `selected.push((format!("{index:010}"), element(item, version, end)));` — and the page is cut with that key at `src/ops/list_custom_line_item_versions.rs:334-340`: `crate::state::paginate(&crate::state::page_scope_within(Self::NAME, &item_arn), selected, input.next_token.as_deref(), input.max_results)`.

The resume is a cursor comparison over those keys — `src/state/pagination.rs:263-269`: `Some(start) => items.into_iter().filter(|(key, _)| key.as_str() >= start).collect()`, documented at `src/state/pagination.rs:224-228`: "The `NextToken` carries the key of the first element left out, so it is a cursor rather than an offset: resuming skips every key below it."

The history is NOT index-stable — `src/state.rs:2468-2476` (`CustomLineItemRecord::remove_periods`, the path a bounded `DeleteCustomLineItem` takes): "// A generation this removal left no billing period to is not a version of the item in any period, so there is nothing for a read to report it for" / `superseded.retain(CustomLineItemVersion::applies_anywhere); self.spans = spans; self.superseded_versions = superseded;`, and the same drop plus the oldest-first eviction in `put_custom_line_item` at `src/state.rs:3815-3821`: `kept.retain(CustomLineItemVersion::applies_anywhere);` … `while kept.len() > MAX_CUSTOM_LINE_ITEM_VERSIONS { kept.remove(0);`.

The sibling rule this listing breaks — `src/ops/list_resources_associated_to_custom_line_item.rs:236-238`: "// The page is cut over the ARN order, so the `NextToken` a call hands back names the same cut point on the next one however the associations grow." and `src/ops/list_custom_line_items.rs:293-295`: "// `custom_line_items` is ordered by ARN, and an ARN carries the zero-padded monotonic id this service mints, so the sequence is creation order and a `NextToken` names the same cut point on the next call."

A stable key is already on the element the handler builds: `start_time_millis` / `start_billing_period` (`src/ops/list_custom_line_item_versions.rs:195,209,212`), both strictly ordered oldest-first.

Test coverage: `tests/all/list_custom_line_item_versions.rs` and `tests/all/pagination_token_opacity.rs` page a history that no call mutates between pages, so nothing pins the behaviour of a token across an intervening write; `tests/all/bounded_delete_narrows_resource_associations.rs` and `tests/all/custom_line_item_versions_by_billing_period.rs` exercise the generation-dropping write without paging it.

## Steps to Reproduce

1. Create a billing group, then CreateCustomLineItem on it with {"BillingPeriodRange":{"InclusiveStartBillingPeriod":"<previous billing period>"}} so the item applies from the previous period on.
2. UpdateCustomLineItem with {"Arn":"<item>","Name":"renamed","BillingPeriodRange":{"InclusiveStartBillingPeriod":"<current billing period>"}} — the first generation is frozen holding the previous period and a second generation takes the current one, so the history has two versions.
3. POST /list-custom-line-item-versions with {"Arn":"<item>","MaxResults":1} and keep the NextToken: the first (oldest) version is reported and the token carries the key "0000000001".
4. DeleteCustomLineItem with {"Arn":"<item>","BillingPeriodRange":{"InclusiveStartBillingPeriod":"<previous billing period>","ExclusiveEndBillingPeriod":"<current billing period>"}} — the frozen generation now applies in no billing period and is dropped, so the live generation moves from index 1 to index 0.
5. POST /list-custom-line-item-versions again with {"Arn":"<item>","MaxResults":1,"NextToken":"<the token from step 3>"} and observe an EMPTY CustomLineItemVersions page with no NextToken: the only remaining version has the key "0000000000", which the `key >= "0000000001"` resume filter drops, so the version the item currently is is never reported to a caller that paged the history.
