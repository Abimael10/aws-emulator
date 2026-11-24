Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 86a81857af943b6387678f8429ed876a9a758ecc

# The paginated page-size rule (apply the documented default, then hold it inside the modelled maxResults range) is written out once per paginated read

_Filed: 2026-11-24T03:32:23Z_
_Operation: ListBillingViews | Class: maintainability_

> _Note: this issue was identified against the code base at commit `86a81857af943b6387678f8429ed876a9a758ecc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Seven of this crate's eight paginated reads each carry their own copy of the same three-step computation: take `max_results`, convert it to a `usize`, fall back to the read's documented default, then clamp into the read's modelled `maxResults` range. The constants differ per read (that is data), but the rule is identical, and the copies have already drifted: the eighth read, `GetBillingPreferences`, is the one copy that applies the default but NOT the clamp, which is exactly the defect the open issue `20261123T005114Z-billing-getbillingpreferences-wrong-behavior` reports ("GetBillingPreferences is the one paginated read that does not hold its page size inside the modelled maxResults bounds").

The crate already keeps its other pagination rule in one place — `state::encode_page_token` / `decode_page_token` / `encode_scoped_page_token`, whose doc says "this crate keeps one page-token codec and one acceptance rule" — so the page-size resolution belongs beside it as a single helper taking (requested, default, min, max). Folding the seven copies onto one helper leaves every response and every existing test unchanged: each call site keeps its own documented default and modelled bounds.

## Evidence

The same computation, seven times:

src/ops/list_billing_views.rs:437-441
        let limit = input
            .max_results
            .and_then(|max| usize::try_from(max).ok())
            .unwrap_or(DEFAULT_MAX_RESULTS)
            .clamp(MIN_MAX_RESULTS, MAX_MAX_RESULTS);

src/ops/list_source_views_for_billing_view.rs:420-424 — same five lines.
src/ops/list_billing_view_segments.rs:483-487 — same, clamped to `(MIN_MAX_RESULTS, DEFAULT_MAX_RESULTS)`.
src/ops/get_credit_allocation_history.rs:369-373 — same.
src/ops/list_business_support_account_charges.rs:521-525 — same.
src/ops/list_business_support_subscription_history.rs:339-343 — same.
src/ops/list_enterprise_support_linked_account_charges.rs:417-421 — same.

The copy that drifted — src/ops/get_billing_preferences.rs:446-449:
        let limit = input
            .max_results
            .and_then(|max| usize::try_from(max).ok())
            .unwrap_or(DEFAULT_MAX_RESULTS);
(no `.clamp(..)`), which is the open issue "GetBillingPreferences is the one paginated read that does not hold its page size inside the modelled maxResults bounds, so a page size of 0 answers an [empty page]" (id 20261123T005114Z-billing-getbillingpreferences-wrong-behavior).

The precedent for keeping one pagination rule in one place — src/state.rs:1809-1811, on `encode_scoped_page_token`: "The transcoding, the binding to the read that minted the token and the authentication under the store's key are [`encode_page_token`]'s, so this crate keeps one page-token codec and one acceptance rule."

Tests that cover the behaviour and must stay green unchanged: tests/all/omitted_page_size.rs ("a paginated read answers a PAGE — the size its `maxResults` member documents a default for, and, where the member documents no default, the largest size that member's modelled range admits", with `LIST_BILLING_VIEWS_DEFAULT_PAGE = 100` and `SOURCE_VIEWS_MAX_PAGE = 100`), tests/all/billing_preference_pagination.rs, tests/all/billing_view_segments.rs, tests/all/business_support_account_charges.rs, tests/all/credit_allocation_history.rs.

## Steps to Reproduce

1. grep -n "max_results" services/service-com-amazonaws-billing-awsbilling/src/ops/*.rs and note the identical five-line default-then-clamp block in list_billing_views.rs:437, list_source_views_for_billing_view.rs:420, list_billing_view_segments.rs:483, get_credit_allocation_history.rs:369, list_business_support_account_charges.rs:521, list_business_support_subscription_history.rs:339 and list_enterprise_support_linked_account_charges.rs:417.
2. Compare get_billing_preferences.rs:446-449, the one copy missing `.clamp(..)` — the drift already filed as a behavioural issue.
3. Observe that src/state.rs keeps the crate's other pagination rule (the page-token codec) in exactly one place, per its own doc at src/state.rs:1809-1811, while the page-size rule has seven copies.
