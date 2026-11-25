Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: f646100c30785845e42ab79c89912e1b5426a340

# GetBillingPreferences is the one paginated read that does not hold its page size inside the modelled maxResults bounds, so a page size of 0 answers an empty page with no nextToken

_Filed: 2026-11-23T00:51:14Z_
_Operation: GetBillingPreferences | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `f646100c30785845e42ab79c89912e1b5426a340`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every other paginated read of this crate computes its page size as `max_results -> usize -> unwrap_or(DEFAULT) -> clamp(MIN_MAX_RESULTS, MAX_MAX_RESULTS)`, and each one says in a comment why the clamp is there even though the boundary validator already enforces the modelled `@range`: it is "used to clamp a size that reached this handler anyway" (ListBillingViews, GetCreditAllocationHistory, ListSourceViewsForBillingView, ListBusinessSupportAccountCharges, ListBusinessSupportSubscriptionHistory, ListEnterpriseSupportLinkedAccountCharges), and ListBillingViewSegments states the consequence: "the page size is held inside those bounds here as well, so no answer can carry more entries than the surface states whatever reaches the member".

`GetBillingPreferences` is the only paginated read that skips that normalisation: it has no `MIN_MAX_RESULTS`/`MAX_MAX_RESULTS` constants at all and uses the raw member value (`.unwrap_or(DEFAULT_MAX_RESULTS)` with no `.clamp(..)`), so the modelled `maxResults` range (`min: 1, max: 50`) is the ONLY thing standing between the request and the walk.

The failure mode the siblings' clamp prevents is concrete here, because this walk's termination is written as an equality against the limit rather than a `len() > limit` truncation: with `limit == 0`, the first selected entry hits `if page.len() == limit` (0 == 0), sets `more_to_come = true` and breaks, leaving `page` empty; `next_token` is then computed from `(more_to_come, page.last())`, whose `(true, None)` arm falls to `_ => None`. The response is `billingPreferences: []` with `nextToken: null` — i.e. the exact answer the handler's own access comment says a client must be able to tell apart from a refusal ("so a client can tell 'no access to this feature' from 'this feature has no preferences'") — and the walk cannot be resumed, so every stored preference of that feature is unreachable through that request. A size above the modelled maximum is likewise served in full, where the sibling reads cap the page.

The current wire behaviour is not wrong today (the boundary layer refuses maxResults 0 and 51 with 400, pinned by a test), which is exactly the position the sibling reads are in when they clamp anyway — the inconsistency is that this one handler relies on the boundary alone while seven others deliberately do not.

## Evidence

The unclamped page size — services/service-com-amazonaws-billing-awsbilling/src/ops/get_billing_preferences.rs:446-449:
```
        let limit = input
            .max_results
            .and_then(|max| usize::try_from(max).ok())
            .unwrap_or(DEFAULT_MAX_RESULTS);
```
(the only page-size constant this handler declares is the default — get_billing_preferences.rs:144-146: `/// The page size a request that names none asks for: "The maximum number of records to return per page. Range: 1 to 50. Default: 50." (`maxResults` doc).` / `const DEFAULT_MAX_RESULTS: usize = 50;`)

The sibling rule, stated and applied — services/service-com-amazonaws-billing-awsbilling/src/ops/list_billing_views.rs:158-162:
```
/// The smallest and largest page size the model's `maxResults` range allows
/// (`BillingViewsMaxResults`, 1..100), used to clamp a size that reached this
/// handler anyway.
const MIN_MAX_RESULTS: usize = 1;
const MAX_MAX_RESULTS: usize = 100;
```
list_billing_views.rs:437-441:
```
        let limit = input
            .max_results
            .and_then(|max| usize::try_from(max).ok())
            .unwrap_or(DEFAULT_MAX_RESULTS)
            .clamp(MIN_MAX_RESULTS, MAX_MAX_RESULTS);
```
and the same clamp with the same justification in src/ops/get_credit_allocation_history.rs:158-161 + 369-373, src/ops/list_source_views_for_billing_view.rs:152-156 + 420-424, src/ops/list_business_support_account_charges.rs:166-170 + 526-530, src/ops/list_business_support_subscription_history.rs:160-163 + 307-311, src/ops/list_enterprise_support_linked_account_charges.rs:175-179 + 417-421, and most explicitly src/ops/list_billing_view_segments.rs:456-464:
```
        // `maxResults` is modelled 1..100 and refused outside it before this
        // handler runs; the page size is held inside those bounds here as well,
        // so no answer can carry more entries than the surface states whatever
        // reaches the member.
        let limit = input
            .max_results
            .and_then(|max| usize::try_from(max).ok())
            .unwrap_or(DEFAULT_MAX_RESULTS)
            .clamp(MIN_MAX_RESULTS, DEFAULT_MAX_RESULTS);
```

Why `limit == 0` loses the walk instead of just serving nothing — get_billing_preferences.rs:465-482:
```
            if page.len() == limit {
                // One further selected entry is what says another page exists:
                // "Present when more pages are available; null when there are
                // no more results." (`nextToken` doc.)
                more_to_come = true;
                break;
            }
            page.push(entry);
        }
        let next_token = match (more_to_come, page.last()) {
            (true, Some(last)) => Some(encode_page_token(
```
...with the `(true, None)` case falling through to `_ => None`, so `more_to_come` is computed and then discarded.

The signal that is thereby made ambiguous, stated by this same handler — get_billing_preferences.rs:416-421: "a feature this account has no access to is refused with the modeled `AccessDeniedException` (403) whether or not a preference is stored under it, so a client can tell \"no access to this feature\" from \"this feature has no preferences\"."

The modelled bound the siblings mirror — smithy_query(query="operation", operation="GetBillingPreferences"): `"maxResults": {"target": "Integer", "range": {"min": 1, "max": 50}}`.

Test coverage: the boundary refusal is pinned (so today's wire answers are unchanged by a fix) — tests/all/billing_preferences.rs:642-659: "// `maxResults` is modelled `@range(min: 1, max: 50)`: both bounds are accepted and one past each is refused." with rows `("below the lower bound", 0, 400)` and `("above the upper bound", 51, 400)`. The omitted-page-size behaviour of this read is covered by tests/all/omitted_page_size.rs and tests/all/billing_preference_pagination.rs; no test exercises the handler's own page-size normalisation.

## Steps to Reproduce

1. Read services/service-com-amazonaws-billing-awsbilling/src/ops/list_billing_views.rs:158-162 and :437-441 — the crate states that a page size is clamped into the modelled range inside the handler because a size outside it may have "reached this handler anyway"; the same constants and the same `.clamp(MIN_MAX_RESULTS, MAX_MAX_RESULTS)` appear in six more paginated reads (get_credit_allocation_history.rs:369-373, list_source_views_for_billing_view.rs:420-424, list_business_support_account_charges.rs:526-530, list_business_support_subscription_history.rs:307-311, list_enterprise_support_linked_account_charges.rs:417-421, list_billing_view_segments.rs:460-464).
2. Read src/ops/get_billing_preferences.rs:446-449: the page size is the raw member value with no clamp, and the file declares no MIN/MAX page-size constant.
3. Follow the walk with `limit == 0` through get_billing_preferences.rs:451-482: the first selected entry satisfies `page.len() == limit`, so `more_to_come = true` and the loop breaks with `page` empty; `match (more_to_come, page.last())` then takes the `_ => None` arm.
4. Observe the answer such a page size produces: HTTP 200 with `billingPreferences: []` and no `nextToken` for a feature that has stored preferences — the "this feature has no preferences" answer get_billing_preferences.rs:416-421 says must stay distinguishable, and with no token the caller cannot continue the walk.
5. Compare with any sibling read handed the same size (e.g. ListBillingViews): the clamp turns it into a page of one and a `nextToken`, so no entry is unreachable.
