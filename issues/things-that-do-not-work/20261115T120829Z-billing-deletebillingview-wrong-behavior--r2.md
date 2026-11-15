Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: e3cc94be0b4813053fd6eddedae190e54e24dc0f

# A deleted billing view's segment timeline is left in the store, contradicting forget_view's stated "no stored record names a view this store no longer holds"

_Filed: 2026-11-15T12:08:29Z_
_Operation: DeleteBillingView | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `e3cc94be0b4813053fd6eddedae190e54e24dc0f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DeleteBillingView` reclaims the deleted view through `ComAmazonawsBillingAwsbillingState::forget_view`, whose own doc comment asserts a complete-reclamation invariant: the view's record, every idempotency token that named it, and its place in every other view's source-view set are "all a billing view is referenced by, so afterwards no stored record names a view this store no longer holds".

That enumeration is incomplete inside this same crate. `ComAmazonawsBillingAwsbillingState::billing_view_segments` is a fourth store keyed BY a billing view ARN ("Every billing view's segment timeline, keyed and ordered by [`BillingViewSegment::key`] - the view's ARN, then the instant the segment began"), and each record carries `billing_view_arn`, documented as "The billing view whose timeline this segment belongs to, as `billing_views` keys it". `forget_view` never touches that map, and neither does the handler (its only mutation is the `forget_view` call). So after a `DeleteBillingView` of a view that has a recorded timeline, the store still holds segment records naming an ARN `billing_views` no longer has - exactly the state the quoted sentence says cannot exist afterwards.

The leaked records are unreachable and unfreeable through the API: `mint_view_arn` only ever advances `next_view_id`, so the ARN is never minted again, no operation deletes a segment, and `ListBillingViewSegments` resolves the ARN against `billing_views` first (404 for a deleted view), so nothing can observe or reclaim them. They still occupy keys against `BillingViewSegment::MAX_RESIDENT`, which `record_billing_view_segment` refuses writes past ("Answers `false` without writing when the timeline is already at [`BillingViewSegment::MAX_RESIDENT`]"), so a create/record/delete cycle permanently consumes that budget - the opposite of the "a create/delete loop reclaims every token it allocated instead of leaving entries behind that no API can observe or free" property the same comment claims for the idempotency map.

The fix is the one the comment already describes: drop the deleted view's contiguous segment range (`BillingViewSegment::arn_key_bounds`, which `billing_view_segments_of` already uses) in `forget_view`, so the fourth reference is reclaimed with the other three.

## Evidence

The invariant the function states - services/service-com-amazonaws-billing-awsbilling/src/state.rs:934-943:
```
    /// Drop the view at `arn` and every reference the store holds to it: its own
    /// record, every idempotency token that named it, and its place in every
    /// other view's source-view set.
    ///
    /// Those three are all a billing view is referenced by, so afterwards no
    /// stored record names a view this store no longer holds: a create/delete
    /// loop reclaims every token it allocated instead of leaving entries behind
    /// that no API can observe or free, and no read can report an association
    /// that points at nothing - the state `AssociateSourceViews` refuses to
    /// write.
```
The body, which touches three maps and not the fourth - state.rs:952-964:
```
    pub fn forget_view(&mut self, arn: &str, at: SystemTime) -> Option<BillingView> {
        let removed = self.billing_views.remove(arn);
        self.idempotency_tokens.retain(|_, stored| stored != arn);
        for view in self.billing_views.values_mut() {
            if !view.source_views.iter().any(|source| source == arn) {
                continue;
            }
            view.source_views.retain(|source| source != arn);
            view.updated_at = Some(at);
            view.view_definition_last_updated_at = Some(at);
        }
        removed
    }
```
The fourth store, keyed by the very ARN that was just removed - state.rs:688-699:
```
    /// Every billing view's segment timeline, keyed and ordered by
    /// [`BillingViewSegment::key`] - the view's ARN, then the instant the
    /// segment began - so one view's segments are a contiguous range walked
    /// oldest first and `ListBillingViewSegments`' page boundaries are stable.
    /// Bounded by [`BillingViewSegment::MAX_RESIDENT`].
...
    pub billing_view_segments: BTreeMap<String, BillingViewSegment>,
```
and the record's own reference to the view - services/service-com-amazonaws-billing-awsbilling/src/state/billing_view_segments.rs:38-41:
```
pub struct BillingViewSegment {
    /// The billing view whose timeline this segment belongs to, as
    /// `billing_views` keys it.
    pub billing_view_arn: String,
```
The handler's only mutation is that one call, and its comment repeats the claim - services/service-com-amazonaws-billing-awsbilling/src/ops/delete_billing_view.rs:208-217:
```
        // Removing the view reclaims every reference the store holds to it: the
        // idempotency tokens that named it, so a create/delete loop frees
        // everything it allocated, and its place in the source-view set of each
        // view that drew from it - the `derived` views the gate above counted.
...
        state.forget_view(&input.arn, ::std::time::SystemTime::now());
```
The budget the leaked records consume - state.rs:991-1002:
```
    /// Answers `false` without writing when the timeline is already at
    /// [`BillingViewSegment::MAX_RESIDENT`], so the caller refuses rather than
    /// growing the store without a bound.
    pub fn record_billing_view_segment(&mut self, segment: BillingViewSegment) -> bool {
        let key = segment.key();
        if !self.billing_view_segments.contains_key(&key)
            && self.billing_view_segments.len() >= BillingViewSegment::MAX_RESIDENT
        {
            return false;
```
The range helper a reclamation would use already exists - state.rs:1011-1016 (`billing_view_segments_of`) over `BillingViewSegment::arn_key_bounds` (billing_view_segments.rs:125-127).

Test coverage today: tests/all/billing_view_delete_cascade.rs:6-11 states the rule it checks as only the two references it enumerates - "a deleted billing view is referenced nowhere in the store afterwards - not by its own record, and not by the source-view set of any view that drew from it" - so no test pins the segment timeline either way, and nothing in the crate justifies leaving it behind.

## Steps to Reproduce

1. Build the emulator as tests/all/billing_views.rs:57-62 does (`emulator_with_engine`, which records the caller as the organization management account so a create is reachable).
2. Create a CUSTOM billing view over PRIMARY (`CreateBillingView` with `sourceViews: [PRIMARY_ARN]`) and keep the minted ARN A.
3. Record a segment for A, as tests/all/token_scope.rs does for the PRIMARY view: `engine.state::<ComAmazonawsBillingAwsbillingState>().lock().record_billing_view_segment(BillingViewSegment::in_effect(&A, BILLING_DOMAIN_BILLABLE))`.
4. Call `DeleteBillingView` with `{"arn": A}` -> 200.
5. Read the store: `billing_views` no longer holds A and `idempotency_tokens` holds no entry naming it, but `billing_view_segments` still holds the record whose key is `A\u{1f}...` and whose `billing_view_arn` is A - a stored record naming a view the store no longer holds, which state.rs:938-939 says cannot exist after `forget_view`.
6. Note that nothing can observe or free it afterwards: `ListBillingViewSegments` with `{"arn": A}` answers 404 (the ARN resolves in `billing_views` first), `mint_view_arn` never re-mints A, and the record keeps counting against `BillingViewSegment::MAX_RESIDENT` for every later `record_billing_view_segment`.
