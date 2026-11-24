Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 22b97d79269fccd521ce633534b046fbdbf99fe9

# ListBusinessSupportSubscriptionHistory answers a billingMonth that has not begun with an empty 200, although the shared BusinessSupportBillingMonth shape supports only the current and past months and the sibling read refuses one

_Filed: 2026-11-16T18:17:30Z_
_Operation: ListBusinessSupportSubscriptionHistory | Class: validation_

> _Note: this issue was identified against the code base at commit `22b97d79269fccd521ce633534b046fbdbf99fe9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListBusinessSupportSubscriptionHistory$billingMonth` targets the SAME Smithy shape (`BusinessSupportBillingMonth`) as `ListBusinessSupportAccountCharges$billingMonth`, and that shape's own documentation states which months it supports: the current month and past months. The sibling read in this crate enforces exactly that — it compares the requested month index against the clock's and refuses a later one with the modelled `ValidationException` (400), with a comment quoting the doc — and a test pins the refusal for 1, 2 and 13 months ahead.

This handler applies no such gate. It resolves the month (defaulting to the current one), parses it through `crate::state::month_period` and refuses ONLY a value that names no calendar month at all; a well-formed future month such as `2999-12` therefore passes, the window becomes that future month's UTC period, no stored contract overlaps it, and the read answers HTTP 200 with an empty `subscriptionContracts` list. A caller that mistyped a year cannot tell that answer from "this month genuinely has no subscription contracts", which is the exact outcome the sibling's comment says the refusal exists to prevent ("a month that has not begun is neither, so it is refused rather than answered with an empty one").

## Evidence

The Smithy sentence, on the shape BOTH operations' `billingMonth` targets — smithy_query(query="doc", shape="com.amazonaws.billing#BusinessSupportBillingMonth"):
```
"A billing month in YYYY-MM format. Supports the current month (estimated charges) and past months (finalized charges)."
```
(`smithy_query(query="operation", operation="ListBusinessSupportSubscriptionHistory")` shows `"billingMonth": {"target": "BusinessSupportBillingMonth", "pattern": "^\\d{4}-(0[1-9]|1[0-2])$"}`, the same target the sibling's required `billingMonth` carries.)

The sibling in this crate that enforces it — services/service-com-amazonaws-billing-awsbilling/src/ops/list_business_support_account_charges.rs:469-480:
```
        // The `billingMonth` doc: "You can request the current month (charges
        // will be estimated) or a past month (charges will be finalized)" — a
        // month that has not begun is neither, so it is refused rather than
        // answered with an empty one, and the current month is exactly the one
        // whose charges are still estimated.
        if month_index(requested) > month_index(current) {
            return Err(invalid_field(
                "billingMonth",
                "must be the current billing month or an earlier one",
            ));
        }
        let is_estimated = month_index(requested) == month_index(current);
```

The handler that skips it — services/service-com-amazonaws-billing-awsbilling/src/ops/list_business_support_subscription_history.rs:266-274 (the ONLY check made on the month):
```
        let billing_month = requested_month(input.billing_month.as_deref()).ok_or_else(|| {
            ListBusinessSupportSubscriptionHistoryErrorModeled::InternalServerException(
                InternalServerException {
                    message: "the current billing month is unavailable".to_string(),
                },
            )
        })?;
        let (month_start, month_end) = crate::state::month_period(&billing_month)
            .ok_or_else(|| invalid_field("billingMonth", "must name a calendar month"))?;
```
Nothing between that line and the answer compares the month with the clock; `crate::state::month_index`/`parse_month` (src/state/billing_month.rs:36-65), which the sibling uses for the comparison, are never called here.

Test coverage today: the sibling's rule IS pinned — services/service-com-amazonaws-billing-awsbilling/tests/all/business_support_account_charges.rs:969-988:
```
    // ...and a month that has not begun is neither estimated nor finalized, so
    // it is refused rather than answered with an empty month.
    for offset in [1_i32, 2, 13] {
        let future = month_from_now(offset);
        ...
        assert_eq!(status, 400, ...);
        assert_eq!(error_code(&body), "ValidationException", ...);
```
The subscription-history suite has no such case: its refusal table (tests/all/business_support_subscription_history.rs:741-762) covers an unparseable `nextToken`, `"billingMonth": "2024-13"` (outside the modelled pattern), `maxResults: 101` and a malformed `accountId` only, so nothing in the crate justifies accepting a month that has not begun.

## Steps to Reproduce

1. Boot the emulator and point an awsJson1_0 `billing` client at it (the harness in tests/all/business_support_subscription_history.rs::emulator_with_engine).
2. Call ListBusinessSupportAccountCharges with {"billingMonth": "<next month, YYYY-MM>"} and observe 400 ValidationException "InvalidArgument: billingMonth (DetailedCode=billingMonth must be the current billing month or an earlier one)" (pinned by tests/all/business_support_account_charges.rs:969-988).
3. Call ListBusinessSupportSubscriptionHistory with the same {"billingMonth": "<next month>"} (or "2999-12").
4. Observe HTTP 200 with {"subscriptionContracts": []} instead of the ValidationException the shared BusinessSupportBillingMonth shape's doc ("Supports the current month ... and past months") and the sibling read both call for.
