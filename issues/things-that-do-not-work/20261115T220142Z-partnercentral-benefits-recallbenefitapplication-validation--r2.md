Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: f6d87ce57ca9b25a0fb0bb3a40dc7f114673a905

# An empty Reason is accepted and persisted as StatusReason, breaking the rule the handler states and the bound its own cited source gives

_Filed: 2026-11-15T22:01:42Z_
_Operation: RecallBenefitApplication | Class: validation_

> _Note: this issue was identified against the code base at commit `f6d87ce57ca9b25a0fb0bb3a40dc7f114673a905`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

RecallBenefitApplication bounds its (model-unconstrained) `Reason` with this crate's single length check, but passes a MINIMUM of 0:

    crate::state::length_clause(&input.reason, 0, crate::state::MAX_STATUS_REASON_LEN)

so `"Reason": ""` is accepted and stored verbatim as the application's `StatusReason`. Two things in this crate contradict that:

1. The comment directly above the write states the rule the empty value breaks: "`Reason` is required on this operation, so a recalled application always reports why it was recalled". With an empty reason the recalled application reports `StatusReason: ""` — it does NOT report why it was recalled, while still having moved the record to PENDING_SUBMISSION and bumped Revision/UpdatedAt.

2. The handler's own comment names where the bounds come from: "the bounds are the ones the model gives these values where it states them — `@length(max: 1000)` on the member the reason is reported through (`GetBenefitApplicationOutput$StatusReason`) and **on the sibling input that writes it**". The sibling input that writes the same stored member is `CancelBenefitApplicationInput$Reason`, which the model bounds to 1..1000 characters (`@pattern ^[\s\S]{1,1000}$` beside `@length(max: 1000)`), so the centralized walk refuses `""` on CancelBenefitApplication with a 400 ValidationException. One stored member therefore has two acceptance rules between siblings: empty through the recall, refused through the cancellation.

The same handler takes BOTH ends from its cited source for the other member it bounds (`MIN_CLIENT_TOKEN_LEN`, `MAX_CLIENT_TOKEN_LEN` for `clientToken`), which is why the `0` on the reason reads as an omission rather than a decision: nothing in the crate — comment, doc or test — states a reason for admitting the empty reason.

## Evidence

Rule the code under it breaks — services/service-com-amazonaws-partnercentralbenefits-partnercentralbenefitsservice/src/ops/recall_benefit_application.rs:310-316:
    // `Reason` is required on this operation, so a recalled application always
    // reports why it was recalled — and that reason replaces whatever explained the
    // status the recall takes it out of.
    application.set_status(
        crate::state::STATUS_PENDING_SUBMISSION,
        Some(input.reason),
    );

The check that admits the empty value — same file:226-230:
    if let Some(clause) =
        crate::state::length_clause(&input.reason, 0, crate::state::MAX_STATUS_REASON_LEN)
    {
        return Err(refused("reason", &clause));
    }

The cited source of the bound — same file:217-225:
    // ... the bounds are the
    // ones the model gives these values where it states them — `@length(max: 1000)`
    // on the member the reason is reported through
    // (`GetBenefitApplicationOutput$StatusReason`) and on the sibling input that
    // writes it, and `@length(min: 1, max: 64)` on every `ClientToken` of this
    // service the model constrains.

The same handler DOES take both ends from that source for the token — same file:231-239:
    if let Some(clause) = crate::state::length_clause(
        client_token,
        crate::state::MIN_CLIENT_TOKEN_LEN,
        crate::state::MAX_CLIENT_TOKEN_LEN,
    ) {

The sibling that writes the same stored member, from the model (smithy_query operation=CancelBenefitApplication):
    "Reason": {"target": "String", "length": {"max": 1000}, "pattern": "^[\\s\\S]{1,1000}$"}
so the centralized walk refuses `""` there, while `RecallBenefitApplicationInput$Reason` is `{"target": "String", "required": true}` and is refused by nothing.

Test coverage: tests/all/persisted_string_bounds.rs:83-85 states the rule it pins — "A reason a write records as `StatusReason` is accepted up to the bound of the member it is reported through, and refused one past it — on the operation whose model bounds it and on the operation whose model does not" — and its rows are only the maximum (lines 95-189: at the bound, one past it, on both operations). The empty reason is pinned by no test, unlike the token, whose table includes ("an empty token", String::new(), false) at line 249.

## Steps to Reproduce

1. Grant the account the partner entitlement (state::grant_partner_entitlement), then CreateBenefitApplication in catalog `AWS` and SubmitBenefitApplication so the application is IN_REVIEW.
2. Call RecallBenefitApplication with {"Catalog":"AWS","Identifier":"benappl-…","Reason":""} — answered 200.
3. Call GetBenefitApplication for the same identifier: Status is PENDING_SUBMISSION, Revision has advanced, and StatusReason is "" — the recalled application reports no reason, contradicting the comment at recall_benefit_application.rs:310-312.
4. Call CancelBenefitApplication on another submitted application with {"Catalog":"AWS","ClientToken":"tok","Identifier":"benappl-…","Reason":""}: answered 400 ValidationException by the centralized walk, because the sibling member admits only 1..1000 characters — the same stored StatusReason cannot be set empty through that path.
