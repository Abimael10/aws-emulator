Issue-Source: trace-sweep-divergence
Issue-Trace: iot-billing-group-lifecycle
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-billing-group-lifecycle
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|DescribeBillingGroup|2xx->2xx|$.billingGroupProperties.billingGroupDescription
Issue-Key: iot|DescribeBillingGroup|2xx->2xx|$.version
Issue-Key: iot|DescribeBillingGroup|4xx->4xx|$.message
Issue-Commit: d9773982178bffc206ea9531196a8841a020d5e3

# iot billing-group plane diverges: UpdateBillingGroup never applies and the not-found refusal is emulator-authored

> _Note: this issue was identified against the code base at commit `d9773982178bffc206ea9531196a8841a020d5e3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeBillingGroup

## Description
Two net-new causes on the AWS IoT billing-group plane, both observed replaying the iot-billing-group-lifecycle wire fixture. They share one root fault area: the billing-group record is served back without the mutations and the refusal text the real service produces.

Cause 1 of 2: UpdateBillingGroup neither applies billingGroupProperties nor advances the record's version (fixture iot-billing-group-lifecycle, step task#9, operation DescribeBillingGroup after an UpdateBillingGroup).
  AWS: Smithy (com-amazonaws-iot-awsiotservice): UpdateBillingGroup PATCH /billing-groups/{billingGroupName}, input billingGroupName (@required, @httpLabel, @length 1..128, pattern ^[a-zA-Z0-9:_-]+$) and billingGroupProperties (@required, target BillingGroupProperties), optional expectedVersion (OptionalVersion), output member `version` (Version, long); errors InvalidRequestException(400), ResourceNotFoundException(404), VersionConflictException(409), ThrottlingException(400), InternalFailureException(500). DescribeBillingGroup GET /billing-groups/{billingGroupName} returns billingGroupProperties and `version`. A successful UpdateBillingGroup therefore replaces the stored BillingGroupProperties (here billingGroupDescription) and increments the record's optimistic-concurrency `version`, which is the value expectedVersion is matched against on the next write; a subsequent DescribeBillingGroup must report the NEW properties and the INCREMENTED version.
  Emulator: the replayed DescribeBillingGroup returns the pre-update properties and the pre-update version — $.billingGroupProperties.billingGroupDescription is still the value set at CreateBillingGroup instead of the value the preceding UpdateBillingGroup sent, and $.version is still 1 where it must be 2. The write is accepted (no error surfaced) but nothing is persisted and the generation counter never moves.
  Why: a client that updates a billing group's properties and then reads it back sees its own write lost — an idempotent-looking success with no effect. Worse, because `version` is frozen, the optimistic-concurrency protocol collapses: a caller that threads the version it just read into expectedVersion keeps succeeding against a stale generation, and VersionConflictException can never fire, so concurrent writers silently clobber each other instead of being refused with 409.
  Fix: UpdateBillingGroup's handler and the billing-group state: the handler must merge/replace the stored BillingGroupProperties from the request and increment the stored version, returning the new version in its output; DescribeBillingGroup must read those stored fields rather than the creation-time snapshot.
  Traces: iot-billing-group-lifecycle#task#9.

Cause 2 of 2: the DescribeBillingGroup ResourceNotFoundException message is emulator-authored, not AWS's (fixture iot-billing-group-lifecycle, step task#14).
  AWS: Smithy: DescribeBillingGroup models ResourceNotFoundException with httpError 404. Recorded behavior for a billingGroupName that resolves to nothing: HTTP 404, ResourceNotFoundException, message `BillingGroup <accountId>:<billingGroupName> not found.` The FIXED text is the leading `BillingGroup ` token, the `:` separator and the trailing ` not found.` (with the period); the VARIABLE parts are the AWS account id that owns the request and the billingGroupName taken from the request's {billingGroupName} URI label. The account-qualified `<accountId>:<name>` form is the same shape IoT uses for its other named-resource refusals.
  Emulator: it answers the correct 404/ResourceNotFoundException but with its own sentence, `Billing group <billingGroupName> does not exist` — wrong noun casing and spacing (`Billing group` instead of `BillingGroup`), the account-id qualifier dropped entirely, `does not exist` instead of `not found`, and no terminating period.
  Why: the status and error code match, so an SDK's typed error works, but anything matching or displaying the message text diverges: operators and log scrapers that key on AWS's wording miss it, and the dropped `<accountId>:` qualifier removes the account attribution that tells a caller WHICH account the lookup missed in — the main diagnostic value of this refusal.
  Fix: DescribeBillingGroup's not-found refusal construction (the handler's billing-group lookup / its error-message formatting), which must emit AWS's template with the request's account id and billingGroupName substituted in.
  Traces: iot-billing-group-lifecycle#task#14.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator (emulator_up: true, /__admin/health 200). Both clusters carry stub_path "" and owned_by [] — neither is stub-blocked nor owned by an open issue.

Cause 1 — fixture iot-billing-group-lifecycle, step task#9, operation DescribeBillingGroup, verdict mismatch, expected_status 200, actual_status 200:
  $.billingGroupProperties.billingGroupDescription: value_mismatch (expected the description sent by the preceding UpdateBillingGroup, got the description set at CreateBillingGroup)
  $.version: value_mismatch (expected 2, got 1)
  divergence_key: service=iot|DescribeBillingGroup|status=200->200|keys=$.billingGroupProperties.billingGroupDescription,$.version
  affected (1): iot-billing-group-lifecycle#task#9

Cause 2 — fixture iot-billing-group-lifecycle, step task#14, operation DescribeBillingGroup, verdict mismatch, expected_status 404, actual_status 404:
  $.message: value_mismatch (expected 'BillingGroup <accountId>:<billingGroupName> not found.', got 'Billing group <billingGroupName> does not exist')
  divergence_key: service=iot|DescribeBillingGroup|status=404->404|keys=$.message
  affected (1): iot-billing-group-lifecycle#task#14

Smithy evidence (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json): UpdateBillingGroup input billingGroupProperties @required, expectedVersion OptionalVersion, output `version` (Version, long), errors include VersionConflictException(409); DescribeBillingGroup output includes billingGroupProperties and `version`, errors include ResourceNotFoundException(404). No modelled constraint pins the message wording, so the 404 text requirement is recorded behavior only.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Cause 1: replay fixture iot-billing-group-lifecycle up to step task#9 — CreateBillingGroup with a billingGroupDescription, UpdateBillingGroup on the same billingGroupName with a different billingGroupDescription, then DescribeBillingGroup. Observe the response still carries the ORIGINAL billingGroupDescription and version 1; AWS returns the updated description and version 2.
3. Cause 2: replay the same fixture's step task#14 — DescribeBillingGroup for a billingGroupName that was never created. Observe HTTP 404 / ResourceNotFoundException with message 'Billing group <name> does not exist' instead of AWS's 'BillingGroup <accountId>:<name> not found.'
