Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: a4424715f3a5bc425be43bc4e35aa4a3867c8573

# A discovery pass that fails on a LATER inventory page discards every instance it already read, and is recorded as "EC2 could not be asked at all"

_Filed: 2026-11-15T13:02:49Z_
_Operation: ListLinuxSubscriptions | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `a4424715f3a5bc425be43bc4e35aa4a3867c8573`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`discovered_in_region` walks a source Region's EC2 inventory page by page, accumulating the instances it reads into a local `inventory` and spending the pass's shared `Budget` (`budget.pages` per page, `budget.records` per instance) as it goes. If any page AFTER the first fails, the whole read is abandoned with `return None` — the instances already read from the earlier pages are dropped on the floor, and the caller treats the Region as one EC2 was never asked about.

That contradicts the module's own model of a partial read. The crate HAS a representation for exactly this situation — `RegionPass::Truncated`, whose doc says the records are "only part of it" and are therefore MERGED into the store rather than replacing what is there — and `discovered_in_region` already returns it for the other two ways a read can cover only part of a Region (budget spent, or a resumed read). An error on page two is the same kind of outcome: part of the inventory WAS established. Instead it takes the `None` branch, whose own doc and whose caller's comment both assert the opposite of what happened ("`None` when EC2 could not be asked about it at all" / "EC2 could not be asked about this Region at all: that is evidence of NOTHING") — EC2 WAS asked, and it answered at least one page.

Two observable consequences, both on the two operations that materialize the pass (`ListLinuxSubscriptions` at src/ops/list_linux_subscriptions.rs:178 and `ListLinuxSubscriptionInstances` at src/ops/list_linux_subscription_instances.rs:176):

1. The instances read from the successful pages are not recorded, so neither list reports them, although the pass established they exist. A merge (`merge_discovery_pass`) would have recorded them without claiming anything about the pages it never reached — which is precisely what `RegionPass::Truncated` exists to express.
2. The budget those pages and records consumed is gone, and the pass carries the reduced budget into the remaining Regions of its plan: `if budget.spent() { unread = true; break; }` (src/state/discovery.rs:221-224) can therefore cut the rest of the plan short on account of work that recorded nothing at all, and the failed Region is recorded as `DiscoveryResume::After(region)` — i.e. finished — rather than as a Region still holding unread inventory.

## Evidence

The branch that discards the partial read — `src/state/discovery.rs:355-367`:
```
        let page = match answer {
            Ok(page) => page,
            Err(error) => {
                // At `debug`: an account with no instances in a Region, and a process that
                // mounts this service with no EC2 beside it, are both ordinary — but a
                // failed hop IS why a pass discovered nothing, so it is traceable.
                ::tracing::debug!(
                    %region,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the EC2 inventory of a source Region could not be read"
                );
                return None;
            }
        };
```
`inventory` (declared at `src/state/discovery.rs:334`) is a local `Vec`, so `return None` drops every record read from the earlier pages of this same Region; `budget` is `&mut` and keeps its decrements (`budget.pages -= 1` at `src/state/discovery.rs:369`, `budget.records -= 1` at `src/state/discovery.rs:383`).

The claim that branch is documented with — `src/state/discovery.rs:317-318`:
```
/// The instances this pass discovered in `region`, or `None` when EC2 could not be asked
/// about it at all.
```

The caller's comment resting on the same claim — `src/state/discovery.rs:240-246`:
```
        let Some(read) = discovered_in_region(ctx, region, &mut budget, from).await else {
            // EC2 could not be asked about this Region at all: that is evidence of
            // NOTHING, so what an earlier pass discovered there stays as it was rather
            // than being dropped. The Region was still asked about, so the next pass goes
            // on past it instead of stopping on the same unanswerable hop.
            stopped = Some(crate::state::DiscoveryResume::After(region.clone()));
            continue;
        };
```

The representation the crate already has for a read that covered only part of a Region — `src/state/discovery.rs:302-306`:
```
    /// The pass read only part of that inventory — it spent its budget before the inventory
    /// ran out, or it resumed one an earlier pass had stopped inside — so these records are
    /// only part of it: what an earlier pass found there is still evidence, and is merged
    /// with rather than replaced by them.
    Truncated(Vec<DiscoveredInstanceRecord>),
```
and the merge it routes to — `src/state.rs:568-570`:
```
    /// Record what a pass found WITHOUT deciding what a Region holds: `found` is merged in,
    /// and a record this pass did not reach is left alone.
```

The budget cut-off the lost budget feeds — `src/state/discovery.rs:220-224`:
```
    for (position, region) in plan.regions.iter().enumerate() {
        if budget.spent() {
            unread = true;
            break;
        }
```

Test coverage: none. A grep of `tests/` for a failed or partially failed inventory hop finds nothing — `tests/all/discovery_from_ec2.rs` only drives successful EC2 reads (its `unwrap_or_else(|error| panic!(...))` setup helpers at lines 324, 347, 358, 385, 403 assert EC2 succeeds), and `tests/all/discovery_resumes_where_it_stopped.rs` covers only the budget-exhaustion form of a partial read. So no suite pins the behaviour of the branch either way.

## Steps to Reproduce

1. Enable discovery for the account: POST /subscription/UpdateServiceSettings with {"LinuxSubscriptionsDiscovery":"Enabled","LinuxSubscriptionsDiscoverySettings":{"SourceRegions":["us-east-1"],"OrganizationIntegration":"Disabled"}}.
2. Populate the source Region's EC2 inventory with enough instances launched from a subscription-bearing AMI that `DescribeInstances` answers more than one page for the page size the pass asks for (`set_max_results(budget.records)`, src/state/discovery.rs:351).
3. Arrange for the SECOND `DescribeInstances` page of that Region to fail (any `Err` from the in-process hop — a throttle, a fault, a rejected continuation token) while the first page succeeds.
4. POST /subscription/ListLinuxSubscriptionInstances (or /subscription/ListLinuxSubscriptions) and observe that NONE of the instances from the successfully-read first page are reported: the whole read was discarded by `return None` at src/state/discovery.rs:366, instead of being recorded as the `RegionPass::Truncated` merge the module defines for a partial read.
5. Observe in the trace that the pass nevertheless spent `budget.pages`/`budget.records` on those pages, and recorded the Region as `DiscoveryResume::After(region)` — finished — so a plan with further source Regions can be cut short at src/state/discovery.rs:221 on account of work that recorded nothing.
