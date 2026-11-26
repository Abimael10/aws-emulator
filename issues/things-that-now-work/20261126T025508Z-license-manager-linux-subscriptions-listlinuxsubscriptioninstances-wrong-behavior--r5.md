Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: dc1b37cb70c2cd36b8cfbe0cb58cd4064d1ca6fc

# Discovery derives DescribeInstances MaxResults from the remaining record budget, so a source Region first reached with 1-4 records left is permanently never discovered

_Filed: 2026-11-26T02:55:08Z_
_Operation: ListLinuxSubscriptionInstances | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `dc1b37cb70c2cd36b8cfbe0cb58cd4064d1ca6fc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::discovery::discovered_in_region` asks EC2 for a page whose `MaxResults` is the number of records the pass may still store (`budget.records`, which starts at `MAX_DISCOVERED_INSTANCES = 1_000` and is decremented per recorded instance). It does so on the strength of a comment asserting that `DescribeInstances` declares no range on `MaxResults`.

The EC2 emulator this crate actually calls over the in-process callback enforces a hand-coded floor of 5 on that member and answers HTTP 400 `InvalidParameterValue` below it. So whenever a pass arrives at a source Region with `budget.records` in 1..=4 — i.e. after earlier Regions of the same pass consumed 996..999 records — the very FIRST hop into that Region is refused.

That refusal is then read as the one outcome the code treats as "evidence of NOTHING": `answered_a_page` is still false, so `discovered_in_region` returns `None`, the handler records `DiscoveryResume::After(region)` for it and moves on, discovering nothing there. Worse, because the pass still walked its whole plan, `plan_unread` stays false and `unread_from` stays `None`, so the pass stores `record_discovery_resume(None)` — the next pass starts the plan at its BEGINNING, re-reads the same first Region, lands on the same `budget.records` value and refuses the same hop. The stall is therefore deterministic and permanent for a given inventory: every Region after the budget-consuming one is never discovered, and neither `ListLinuxSubscriptionInstances` nor `ListLinuxSubscriptions` ever reports its instances, however many times either is called.

(The same derived-below-floor hop inside a single Region is merely a wasted round trip — there `answered_a_page` is already true, so the read is recorded as truncated and the next pass resumes at the cursor. The loss is specific to a Region whose FIRST hop is the refused one.)

## Evidence

The handler sends the remaining record budget as the page size, on a premise about the callee:

`services/service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions/src/state/discovery.rs:384-389`
```
            // A page no larger than what this pass may still RECORD, so a page it asked for
            // can always be read to its end and a resumed read always moves past the page
            // it starts at: `MaxResults` is the `@paginated` page size of
            // `DescribeInstances` — "the maximum number of items to return for this
            // request" — on which that model declares no range.
            .set_max_results(i32::try_from(budget.records).ok())
```

The budget it derives that page size from:
`.../src/state/discovery.rs:127` — `const MAX_DISCOVERED_INSTANCES: usize = 1_000;`

The service it calls refuses that page size below 5:
`services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_instances.rs:288-301`
```
        if input.max_results.is_some_and(|max_results| max_results < MIN_MAX_RESULTS) {
            // EC2 refuses a page size below the smallest the operation pages with rather
            // than serving it: a request that mis-sizes its page must not read as an empty
            // account.
            return Err(max_results_below_published_minimum(
                input.max_results.unwrap_or_default(),
                MIN_MAX_RESULTS,
            ));
        }
```
`services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_instances.rs:461` — `const MIN_MAX_RESULTS: i32 = 5;`
and that constructor answers HTTP 400 `InvalidParameterValue`: `services/service-com-amazonaws-ec2-amazonec2/src/state/parameter_bounds.rs:29-32` — "How EC2 refuses a page size below the smallest one an operation publishes: HTTP 400 `InvalidParameterValue` ..."

The refused first hop is classified as establishing nothing:
`.../src/state/discovery.rs:405-409`
```
                if !answered_a_page {
                    // Not one page was answered, so this read established nothing about the
                    // Region.
                    return None;
                }
```

and the pass then walks past that Region:
`.../src/state/discovery.rs:253-262`
```
        let Some(read) = discovered_in_region(ctx, region, &mut budget, from).await else {
            // Not one page of this Region's inventory was answered: that is evidence of
            // NOTHING ...
            walked_to = Some(crate::state::DiscoveryResume::After(region.clone()));
            continue;
        };
```

and, having walked its whole plan, records no resume point at all, so the next pass repeats identically:
`.../src/state/discovery.rs:301-305`
```
    store.record_discovery_resume(match (unread_from, plan_unread) {
        (Some(inside), _) => Some(inside),
        (None, true) => walked_to,
        (None, false) => None,
    });
```

Both discovery lists report only from the store this pass fills (`src/ops/list_linux_subscription_instances.rs:179` and `src/ops/list_linux_subscriptions.rs:178` both call `crate::state::discovery::materialize_for_report`), so the undiscovered Region is invisible to both.

Test coverage: the crate's discovery suites cover the budget-truncation and resume paths (`tests/all/discovery_partial_inventory_read.rs`, `tests/all/discovery_resumes_where_it_stopped.rs`, `tests/all/discovery_keeps_the_earliest_unread_cursor.rs`, `tests/all/discovery_from_ec2.rs`), but none exercises a hop whose derived `MaxResults` falls below the callee's floor, which is why the stall is unpinned.

## Steps to Reproduce

1. Call UpdateServiceSettings with LinuxSubscriptionsDiscovery=Enabled and LinuxSubscriptionsDiscoverySettings.SourceRegions=["eu-west-1", "us-east-1"] (the pass walks the sorted set, so eu-west-1 is read first).
2. In eu-west-1, launch 997 running instances (each in its own reservation, AZ eu-west-1a) from an AMI whose Image$PlatformDetails bills a Linux subscription, e.g. "Red Hat Enterprise Linux".
3. In us-east-1, launch 1 running instance from such an AMI (AZ us-east-1a).
4. Call ListLinuxSubscriptionInstances. The pass reads eu-west-1 to the end of its inventory, consuming 997 of the 1000-record budget, leaving budget.records = 3.
5. The pass then hops into us-east-1 with MaxResults=3; the in-process EC2 emulator refuses it with HTTP 400 InvalidParameterValue (MIN_MAX_RESULTS = 5), so answered_a_page is false, discovered_in_region returns None, and nothing is discovered in us-east-1.
6. Observe the response: only the eu-west-1 instances are reported; the us-east-1 instance is absent. ListLinuxSubscriptions likewise reports no subscription for it.
7. Call ListLinuxSubscriptionInstances (or ListLinuxSubscriptions) repeatedly. Because the pass walked its whole plan, record_discovery_resume(None) was stored, so every later pass restarts at eu-west-1, reaches us-east-1 with the same 3-record budget and is refused again: the us-east-1 instance is never discovered.
