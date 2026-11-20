Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__pricing-plan-manager__route53__wafv2.free-subscription-refuses-a-key-value-store-and-keeps-its-resources cloudfront__pricing-plan-manager__route53__wafv2.hosted-zone-joins-and-leaves-a-free-subscription
Issue-Key: cloudfront__pricing-plan-manager__route53__wafv2.free-subscription-refuses-a-key-value-store-and-keeps-its-resources|run#12|AssertionError|assert (code, message) == ("ValidationException", (
Issue-Key: pricing-plan-manager|AssociateResourcesToSubscription|NotImplemented
Issue-Commit: 14df30a096a54aecb31fd12ed18f88f92038b3f7

# Implement pricing-plan-manager AssociateResourcesToSubscription: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `14df30a096a54aecb31fd12ed18f88f92038b3f7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateResourcesToSubscription

## Description
`AssociateResourcesToSubscription` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-pricingplanmanager-awspricingplanmanager/src/ops/associate_resources_to_subscription.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

2 recorded unit(s) fail on their `AssociateResourcesToSubscription` call, so each one unblocks only when `AssociateResourcesToSubscription` works: `cloudfront__pricing-plan-manager__route53__wafv2.free-subscription-refuses-a-key-value-store-and-keeps-its-resources`, `cloudfront__pricing-plan-manager__route53__wafv2.hosted-zone-joins-and-leaves-a-free-subscription`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-pricingplanmanager-awspricingplanmanager/src/ops/associate_resources_to_subscription.rs`. Replaying the corpus, the emulator refused the `AssociateResourcesToSubscription` call: File "run.py", line 365, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_cloudfront__pricing_plan_manager__route53__wafv2_free_subscription_refuses_a_key_value_store_and_keeps_its_resources.py", line 320, in run
    assert (code, message) == ("ValidationException", (
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: unexpected answer to a KeyValueStore on the FREE tier: NotImplemented: 'operation AssociateResourcesToSubscription is scaffolded but not implemented in this emulator'

## Steps to Reproduce
Boot the pricing-plan-manager emulator
Call AssociateResourcesToSubscription and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal

## Resolution and residual

`AssociateResourcesToSubscription` is implemented (`src/ops/associate_resources_to_subscription.rs`,
scaffold marker removed): it resolves the subscription ARN and `If-Match`, weighs the request through
the shared resource-set rule `state::subscription_resources::plan_resource_addition` (status, pending
change, a resource already associated, the modelled 10-resource maximum) and the plan-family/plan-tier
eligibility gate, stores the resulting set and reports it `SYNC_IN_PROGRESS` until the clock settles it
to `ACTIVE`, exactly as `DisassociateResourcesFromSubscription` does. The key
`pricing-plan-manager|AssociateResourcesToSubscription|NotImplemented` replays as fixed, and the unit
`hosted-zone-joins-and-leaves-a-free-subscription` now joins and leaves the subscription.

RESIDUAL, still open and tracked here rather than re-filed: the check assertion key of unit
`free-subscription-refuses-a-key-value-store-and-keeps-its-resources` is not closed. The call is now
refused as that assertion expects in class and effect — HTTP 400 `ValidationException`, nothing stored,
the subscription keeps its resources — but the assertion also compares the MESSAGE, and the recorded
key does not disclose the sentence AWS words this tier gate with. The emulator answers with the one
wording this service is recorded issuing for a resource a tier does not cover, "The following resources
are not eligible for this subscription tier: <arns>."
(`state::resource_eligibility::ineligible_resources_message`, shared with `CreateSubscription`). When a
recording shows AWS's own sentence for the tier gate, that function and
`state::resource_eligibility::covered_by_plan_tier` are where it changes; the latter also places only
one resource kind today (a CloudFront KeyValueStore is covered from a paid tier up) and would become a
tier-to-kind table if a recording places another kind or another tier.

The same two units also fail on causes filed elsewhere: the post-cancel poll of
`20261111T105841Z-pricing-plan-manager-cancelling-a-free-subscription-never-completes-and-publishes.md`
(behavioural grounding `20261114T092337Z-...-the-createsubscription-eligibility.md`).
