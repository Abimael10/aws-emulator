Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: billing.update-billing-view-nonexistent-arn-rejected
Issue-Shape-Id: com.amazonaws.billing#AWSBilling
Issue-Key: billing.update-billing-view-nonexistent-arn-rejected|run#9|AssertionError|assert code == "ValidationException", code
Issue-Commit: c3aab41312baa47e690c4afe8f7c7d51a974747b

# UpdateBillingView answers 404 ResourceNotFoundException for a well-formed own-account ARN no view is stored under, where AWS answers 400 ValidationException "Only Custom Billing Views may be edited."

> _Note: this issue was identified against the code base at commit `c3aab41312baa47e690c4afe8f7c7d51a974747b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateBillingView

## Description
REOPENS 20261124T234529Z-billing-updatebillingview-error-response.md: its fix left the call refused.

Cause 1 of 1: UpdateBillingView decides EXISTENCE before ELIGIBILITY, so a well-formed BillingViewArn in the caller's own account that no view is stored under is refused as a not-found instead of as an ineligible (non-CUSTOM) view.

AWS: recorded behavior only — the Smithy model declares both ValidationException (HTTP 400) and ResourceNotFoundException (HTTP 404) on UpdateBillingView (Smithy: UpdateBillingView errors ValidationException httpError 400, ResourceNotFoundException httpError 404; input member arn target BillingViewArn @required, pattern ^arn:aws[a-z-]*:(billing)::[0-9]{12}:billingview/[a-zA-Z0-9/:_\+=\.\-@]{0,75}[a-zA-Z0-9]$), so the model alone does not choose between them. The recording does: UpdateBillingView against a well-formed, pattern-valid own-account billingview ARN that no view is stored under is refused HTTP 400, error code `ValidationException`, message `The Billing View ARN does not correspond to a Custom Billing View. Only Custom Billing Views may be edited.` — and UpdateBillingView against the account's own PRIMARY view (a view that DOES exist, but is not CUSTOM) is refused with the IDENTICAL code and the IDENTICAL message. In other words AWS decides eligibility first and does not disclose not-found-vs-ineligible on this operation: both inputs collapse into the one ValidationException. That whole message is FIXED text for this error class — it carries no value from the request or from state; the request's ARN appears only in the per-member fieldList entry, not in the top-level message. The effect is also absent in both cases: a later GetBillingView on the PRIMARY view reports the same name and description as before the refused calls.

Emulator: for the nonexistent own-account ARN the emulator answers HTTP 404 with error code `ResourceNotFoundException` instead of 400 `ValidationException`. The handler tests `state.billing_views.contains_key(&input.arn)` and returns `UpdateBillingViewErrorModeled::ResourceNotFoundException(crate::state::unresolved_billing_view_arn(..))` BEFORE it tests `state.custom_view(&input.arn).is_none()` and returns the shared `crate::state::not_a_custom_view(arn, "edited")` ValidationException. The PRIMARY-view branch is correct — `not_a_custom_view` already renders exactly AWS's two-sentence text — so only the absent-ARN branch diverges, and it diverges in both status and error code.

This is the direct result of the fix for 20261124T234529Z-billing-updatebillingview-error-response.md, which argued from the declared-but-unconstructed 404 variant that an absent own-account ARN must be answered 404 and added the existence check ahead of the eligibility check. The recorded service does the opposite: on UpdateBillingView the 404 variant is simply not reachable for this input class, because eligibility is decided first. The earlier handler comment ("eligibility is decided before existence ... the modeled ResourceNotFoundException would tell a caller which of the two it was") was describing real AWS behavior; the fix inverted it.

Why: a client relying on AWS's behavior branches on the refusal of an edit to a view it did not create — a retry/create-or-update path catching ValidationException to mean "this ARN is not an editable Custom view, stop" now sees an uncaught ResourceNotFoundException and takes the wrong branch (or surfaces a 404 to its caller). It also leaks a distinction AWS withholds: against the emulator a caller can probe which own-account billingview ids exist purely from UpdateBillingView's status code, while against AWS absent and non-CUSTOM are indistinguishable on this operation.

Fix: the operation's refusal ordering in its handler — resolve the ARN through the CUSTOM-view lookup and refuse absent and stored-but-not-CUSTOM alike with the shared ValidationException, so the 404 branch is not taken for an own-account ARN on this operation; the sibling readers and the source-view writers keep their 404 (their recordings are not in question here). The pinning test row that asserts 404 for the absent ARN on UpdateBillingView has to move back with it.

Traces: behavioral check billing.update-billing-view-nonexistent-arn-rejected.

## Evidence
Failing check: billing.update-billing-view-nonexistent-arn-rejected (mismatch)

    File "check_billing_update_billing_view_nonexistent_arn_rejected.py", line 36, in run
      assert code == "ValidationException", code
    AssertionError: ResourceNotFoundException

Expected (recorded AWS), for arn=arn:aws:billing::<account-id>:billingview/<id-no-view-is-stored-under>:
  HTTP 400
  Error.Code    = ValidationException
  Error.Message = "The Billing View ARN does not correspond to a Custom Billing View. Only Custom Billing Views may be edited."
  (identical code and message as UpdateBillingView against the account's PRIMARY view ARN)

Actual (emulator), same input:
  HTTP 404
  Error.Code    = ResourceNotFoundException
  Error.Message = "[<arn>] is an invalid billing view arn"  (crate::state::unresolved_billing_view_arn)

Message anatomy of the expected error: the entire sentence pair "The Billing View ARN does not correspond to a Custom Billing View. Only Custom Billing Views may be edited." is FIXED text for this error class; the only variable part AWS carries for this refusal is the ARN inside the per-member fieldList entry, not the top-level message. The emulator already renders that fixed text correctly for the stored-but-non-CUSTOM case.

Code that produces the divergence — services/service-com-amazonaws-billing-awsbilling/src/ops/update_billing_view.rs:227-250:
    // Existence comes first, because the two inputs are different refusals: ...
    if !state.billing_views.contains_key(&input.arn) {
        return Err(UpdateBillingViewErrorModeled::ResourceNotFoundException(
            crate::state::unresolved_billing_view_arn(&input.arn),
        )
        .into());
    }
    if state.custom_view(&input.arn).is_none() {
        return Err(UpdateBillingViewErrorModeled::ValidationException(
            crate::state::not_a_custom_view(&input.arn, "edited"),
        )
        .into());
    }

The correct wording already exists in the crate — services/service-com-amazonaws-billing-awsbilling/src/state.rs:1810-1822:
    pub fn not_a_custom_view(arn: &str, action: &str) -> ValidationException {
        ValidationException {
            message: format!(
                "The Billing View ARN does not correspond to a Custom Billing View. \
                 Only Custom Billing Views may be {action}."
            ),
            ...

Model surface (smithy_query query="operation" operation="UpdateBillingView"): errors include ValidationException (client, httpError 400) and ResourceNotFoundException (client, httpError 404); the model states no precedence between them, so the recording is the authority.

Prior fixed issue this reopens: 20261124T234529Z-billing-updatebillingview-error-response.md ("An own-account ARN that no view is stored under is refused 400 ValidationException, so the modelled ResourceNotFoundException UpdateBillingView declares is never reachable"). Its fix introduced the existence-before-eligibility ordering above; the recording shows AWS orders them the other way and never reaches the 404 for this input class.

## Steps to Reproduce
1. Boot the emulator and point an awsJson1_0 `billing` client at it.
2. Get the account id (sts.get_caller_identity()["Account"]) and the account's PRIMARY billing view ARN (billing.list_billing_views(billingViewTypes=["PRIMARY"]) -> billingViews[0].arn). Confirm billing.get_billing_view(arn=<primary arn>) answers 200 with billingViewType == PRIMARY (positive control: billing reads work).
3. Call UpdateBillingView(arn="arn:aws:billing::<account-id>:billingview/sim2real-behav-does-not-exist", name="<any valid name>") — a well-formed, pattern-valid own-account ARN that no view is stored under.
   Observed: HTTP 404, Error.Code=ResourceNotFoundException.
   Expected: HTTP 400, Error.Code=ValidationException, Error.Message="The Billing View ARN does not correspond to a Custom Billing View. Only Custom Billing Views may be edited."
4. Call UpdateBillingView(arn=<the PRIMARY view ARN>, name="<any valid name>") — expected to return the IDENTICAL code and message as step 3. The emulator gets this one right today, which is what makes the pair in step 3/4 diverge instead of agree.
5. Call GetBillingView(arn=<primary arn>) again and confirm name and description are unchanged (both runs agree here; no write happened).
