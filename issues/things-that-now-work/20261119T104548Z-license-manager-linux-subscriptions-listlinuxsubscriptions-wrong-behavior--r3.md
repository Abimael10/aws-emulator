Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 6dd9cfd49efdd782f26be628c2d20d018b89a8a7

# A multi-Region discovery pass discards the inventory cursor of a Region it stopped inside when a later Region finishes

_Filed: 2026-11-19T10:45:48Z_
_Operation: ListLinuxSubscriptions | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `6dd9cfd49efdd782f26be628c2d20d018b89a8a7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`materialize_for_report` tracks where the pass stopped in ONE last-write-wins slot (`let mut stopped`), and decides whether to persist it by looking only at that slot's FINAL value. When a pass stops INSIDE one source Region's inventory while its budget still has room — which happens whenever EC2 answers the first page(s) of that Region and then fails a later hop (`discovered_in_region` breaks with `reached_end == false` and returns `RegionPass::Truncated` + `DiscoveryResume::Inside(region, cursor)`) — the loop goes on to the NEXT Region of the plan. That Region's own outcome then overwrites the slot (`stopped = Some(read.resume)` at discovery.rs:266, or `DiscoveryResume::After(region)` at discovery.rs:247), so the `Inside(region, cursor)` naming the unread remainder is gone. Because `unread` (discovery.rs:270) only inspects the final slot value, a pass whose last Region finished records `record_discovery_resume(None)` (discovery.rs:279): the recorded stopping point for the Region that still holds unread inventory is thrown away, and the next pass starts the plan at its beginning and re-reads that Region's prefix from page one instead of resuming at the cursor the remainder begins at.

This contradicts the rule stated three times in the crate's own text, and the crate's test suite asserts that rule — but only for a single-Region plan, so the multi-Region case is unpinned. The effect is observable through both operations that materialize a pass (`ListLinuxSubscriptions`, whose handler calls it at list_linux_subscriptions.rs:178, and `ListLinuxSubscriptionInstances` at list_linux_subscription_instances.rs:176): with two or more `SourceRegions`, a Region whose inventory was only partly read is re-walked from its beginning on every later report, so it can keep spending the shared budget on inventory it already recorded instead of advancing into the remainder.

## Evidence

The rule the crate states for exactly this case, in the same function:
- `src/state/discovery.rs:244-246`: "A read that WAS answered in part does not come here — it is a truncated read below, whose records are merged and whose unread remainder the next pass resumes at."
- `src/state/discovery.rs:186-191`: "A plan whose Regions hold more inventory than those constants let one pass read is read over SUCCESSIVE passes: the point a pass stopped at is recorded (`crate::state::DiscoveryResume`) and the next pass's plan begins there, so every Region the settings name is asked about rather than only the first few of them."
- `src/state.rs:315-321` (`DiscoveryResume`): "Where a discovery pass STOPPED reading, so the next one picks the plan up there instead of re-reading the prefix the previous one already spent its budget on."

The code that breaks it (single slot, overwritten per Region; final value alone decides):
- `src/state/discovery.rs:216`: `let mut stopped: Option<crate::state::DiscoveryResume> = None;`
- `src/state/discovery.rs:247`: `stopped = Some(crate::state::DiscoveryResume::After(region.clone()));`
- `src/state/discovery.rs:266`: `stopped = Some(read.resume);`
- `src/state/discovery.rs:270`: `let unread = unread || matches!(stopped, Some(crate::state::DiscoveryResume::Inside(_, _)));`
- `src/state/discovery.rs:279`: `store.record_discovery_resume(if unread { stopped } else { None });`

That the partial read really does leave the loop with budget to spare (so a later Region is processed and overwrites the slot):
- `src/state/discovery.rs:379-387`: on a failed hop, `if !answered_a_page { return None; }` … "Earlier pages WERE answered, so what they held stands: the read stops here as a partial one" — `break`, with `reached_end` still false.
- `src/state/discovery.rs:437`: `let complete = reached_end && answered && from.is_none();`
- `src/state/discovery.rs:483-487`: `resume: if reached_end { After(region) } else { Inside(region.to_string(), cursor) }`.

Test coverage states the rule but only for ONE Region (`const REGION`), so the multi-Region clobber is untested:
- `tests/all/discovery_partial_inventory_read.rs:336-341`: "the Region still holds unread inventory, so the pass must stop inside it at the cursor that remainder begins at".
- `tests/all/discovery_partial_inventory_read.rs:344-345`: "And the remainder is read: the next report's pass takes the Region up where this one stopped, so the whole inventory is discovered however the walk was cut short."
- `tests/all/discovery_resumes_where_it_stopped.rs:212-248` drives `discovery_plan` from a pre-seeded `stopped` value; it never drives `materialize_for_report` over a plan in which one Region stops inside and a later one finishes.

## Steps to Reproduce

1. Call UpdateServiceSettings with LinuxSubscriptionsDiscovery=Enabled and SourceRegions=["us-east-1", "us-west-2"] (so discovery_plan walks us-east-1 first, then us-west-2).
2. Through the emulated EC2, put enough subscription-bearing instances in us-east-1 that its inventory spans more than one DescribeInstances page, and a small inventory in us-west-2; arrange for the SECOND us-east-1 DescribeInstances hop to fail while the pass still has pages/records budget left (the mechanism tests/all/discovery_partial_inventory_read.rs already builds with its PagedEc2 / InventoryHop wrapper).
3. Call ListLinuxSubscriptions (or ListLinuxSubscriptionInstances) once, which materializes one pass: us-east-1 is recorded as RegionPass::Truncated with DiscoveryResume::Inside("us-east-1", Some(token)), then us-west-2 is read to its end and sets stopped = After("us-west-2").
4. Inspect the store's discovery_resume: it is None (discovery.rs:279 takes the `else` arm because the final `stopped` is an `After`, so `unread` is false) instead of Inside("us-east-1", Some(token)).
5. Call the list again: the next pass starts the plan at us-east-1 from the beginning of its inventory and re-reads the pages already recorded, rather than resuming at the cursor the unread remainder begins at — the behaviour tests/all/discovery_partial_inventory_read.rs:344-355 asserts must not happen.
