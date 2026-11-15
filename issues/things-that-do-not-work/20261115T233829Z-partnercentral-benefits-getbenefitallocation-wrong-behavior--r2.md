Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 9e7a0155591d86d36e2bc8d364179a5a1cfd8174

# A materialised allocation expiry stamps UpdatedAt with the read instant, not the instant the expiry fell due

_Filed: 2026-11-15T23:38:29Z_
_Operation: GetBenefitAllocation | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `9e7a0155591d86d36e2bc8d364179a5a1cfd8174`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`BenefitAllocation::expire_due` is the one place that materialises an allocation expiry, and the reporting operations (`GetBenefitAllocation`, `ListBenefitAllocations`) call it through `expire_allocations_due` before they read. Its own doc states WHEN the change happened: the allocation is `INACTIVE` "whether or not anybody has looked at it: the change was owed at that instant" — i.e. at the record's `ExpiresAt`. The code then dates that change at the moment of the FIRST READ instead: it writes `self.updated_at_millis = bump_millis(self.updated_at_millis, now_millis)` with `now_millis` = `crate::state::now_millis()` taken in the handler for this request.

So the member the model documents as "the timestamp when the benefit allocation was last updated" reports a moment at which nothing happened to the allocation — the moment somebody first looked at it. For the seeded allocation whose term closed before state init (`SEEDED_ALLOCATIONS`: `created_days_ago: 400, term_days: 365`, src/state.rs:689-690) `GetBenefitAllocation` answers `CreatedAt` = 400 days ago, `ExpiresAt` = 35 days ago, and `UpdatedAt` = now, so the record claims an update 35 days after it expired and the value is a function of when the emulator was read rather than of the state it holds (two runs over identical state answer different `UpdatedAt`). The instant the change was owed is already on the record (`expires_at_millis`), which is what the doc above names, so nothing new has to be computed to date the write by it.

## Evidence

The rule the crate states about this change — src/state.rs:1020-1029:
    /// Materialise the expiry the clock has already made due, and report whether this call
    /// is what materialised it.
    ///
    /// An `ACTIVE` allocation past its `ExpiresAt` is no longer usable, so it is
    /// `INACTIVE` whether or not anybody has looked at it: the change was owed at that
    /// instant, which is why a reporting operation may apply it.

The code that breaks it — src/state.rs:1032-1046 (`now_millis` is this REQUEST's clock, not `self.expires_at_millis`):
        if self.status == ALLOCATION_STATUS_ACTIVE && self.has_expired(now_millis) {
            self.status = ALLOCATION_STATUS_INACTIVE.to_string();
...
        self.updated_at_millis = bump_millis(self.updated_at_millis, now_millis);

where src/state.rs:3009-3011 is
    pub fn bump_millis(previous: i64, now: i64) -> i64 {
        now.max(previous.saturating_add(1))
    }

The read that supplies that instant and then reports the member — src/ops/get_benefit_allocation.rs:261 `let now_millis = crate::state::now_millis();`, :270-274
        // An allocation past its `ExpiresAt` is no longer usable whether or not anybody has
        // looked at it, so this report first materialises the expiries the clock has ALREADY
        // made due; it writes nothing else, and a later caller sees exactly what this one
        // did. See `crate::state::BenefitAllocation::expire_due`.
        store.expire_allocations_due(&input.catalog, now_millis);
and :319 `updated_at: Some(crate::state::timestamp(allocation.updated_at_millis)),`. The sibling report does the same — src/ops/list_benefit_allocations.rs:197, :201-207.

The record whose expiry is already past at state init — src/state.rs:686-690:
        issued_amount_minor: 0,
        // Granted more than a term ago, so its `ExpiresAt` is already past: the expiry is
        // what moves it to `INACTIVE`, rather than a status seeded as already moved.
        created_days_ago: 400,
        term_days: 365,

Test coverage: tests/all/benefit_allocations.rs:874-877 asserts only `moved.updated_at_millis > moved.created_at_millis` ("materialising the expiry must advance UpdatedAt"), which holds for either instant — `expires_at_millis` is also past `created_at_millis` — so no test pins the read instant, and tests/all/get_benefit_allocation.rs:387 only echoes the stored `updated_at_millis` back. No comment, test or doc in the crate states a reason for dating the write at the read.

## Steps to Reproduce

1. Boot the emulator and grant the caller's account the partner entitlement (`state::grant_partner_entitlement`, as `tests/all/benefit_application_lifecycle.rs`'s `Harness::new` does).
2. Call `ListBenefitAllocations` with `Catalog: "AWS"` and find the allocation based on "Partner Training Access" — the seeded row with `created_days_ago: 400, term_days: 365` (src/state.rs:678-691), whose term closed ~35 days before state init.
3. Call `GetBenefitAllocation` for that `Id` and compare the three timestamps: `CreatedAt` ~400 days ago, `ExpiresAt` ~35 days ago, `UpdatedAt` = the instant of this (first) call, although the only change ever applied to the record is the expiry that fell due at `ExpiresAt` (src/state.rs:1032-1035).
4. Repeat the whole run at a different wall-clock time: the same unchanged seeded state answers a different `UpdatedAt`, because src/state.rs:1045 takes the value from `now_millis` rather than from `self.expires_at_millis`.
5. Re-read src/state.rs:1023-1025: "the change was owed at that instant" — the instant the code declines to date the write by.
