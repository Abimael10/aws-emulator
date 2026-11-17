Issue-Source: trace-sweep-divergence
Issue-Trace: marketplace-agreement-agreement-payment-request-workflow
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: marketplace-agreement-agreement-payment-request-workflow marketplace-agreement-validation-error-paths
Issue-Shape-Id: com.amazonaws.marketplaceagreement#AWSMPCommerceService_v20200301
Issue-Key: marketplace-agreement|GetAgreementPaymentRequest|4xx->4xx|$.fields[name="agreementId"].message
Issue-Key: marketplace-agreement|SendAgreementPaymentRequest|4xx->4xx|$.fields[*].message
Issue-Key: marketplace-agreement|AcceptAgreementPaymentRequest|4xx->4xx|$.fields[*].message
Issue-Key: marketplace-agreement|RejectAgreementPaymentRequest|4xx->4xx|$.fields[*].message
Issue-Key: marketplace-agreement|CancelAgreementPaymentRequest|4xx->4xx|$.fields[*].message
Issue-Key: marketplace-agreement|ListAgreementCancellationRequests|4xx->4xx|$.fields[name="partyType"].message
Issue-Commit: 068484269d53419aabd6862b01bbe69f7f83ee74

# ValidationException field messages carry a trailing period not in AWS wording

> _Note: this issue was identified against the code base at commit `068484269d53419aabd6862b01bbe69f7f83ee74`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAgreementPaymentRequest

## Description
Every per-member entry the emulator puts in `ValidationException.fields[].message` is rendered with a trailing full stop ("Member is invalid.") where AWSMPCommerceService_v20200301 renders it without one ("Member is invalid"). The HTTP status (400), the error code (`ValidationException`) and the top-level `message` all match on every affected step; the ONLY divergence is the punctuation of the field-level message, so this is one shared defect in how the field message is constructed, reproduced below once per operation the corpus exercises.

The string is entirely FIXED text: `ValidationExceptionField.message` is "Member is invalid" verbatim for a member that fails its modeled constraint, and the member name is NOT interpolated into the message — it is carried separately in the sibling `ValidationExceptionField.name` (Smithy: ValidationExceptionField.name and .message are both @required BoundedString, @length 1..4096, @pattern ^(.)+$). So there is nothing request-derived or run-derived in this value: no id, no limit, no clock, no request id. That is what makes the difference a pure wording defect rather than an environment difference, and it is why one fix to the shared renderer closes all six causes.

Six causes, all `marketplace-agreement`, all "expected 'Member is invalid', got 'Member is invalid.'":

Cause 1 of 6: GetAgreementPaymentRequest, fixture `marketplace-agreement-agreement-payment-request-workflow`, step `task#2`, at JSON path `$.fields[name="agreementId"].message`.
  AWS: Smithy: GetAgreementPaymentRequest input members `paymentRequestId` (@required, @length 1..64, @pattern ^pr-[a-zA-Z0-9]+$) and `agreementId` (@required, @length 1..64, @pattern ^[A-Za-z0-9_/-]+$); modeled error ValidationException (errorKind client, @httpError 400) carrying `message`, `reason`, `requestId` and `fields: ValidationExceptionFieldList`. The recorded response is HTTP 400 `ValidationException` whose `fields` entry for `name: "agreementId"` has `message: "Member is invalid"` — no terminal period. The model does not document the sentence itself, so the exact text is recorded behavior only.
  Emulator: answers HTTP 400 `ValidationException` with the same field `name`, but `message: "Member is invalid."`.
  Why: a client that asserts on, snapshots, logs-matches or surfaces the per-field reason sees text AWS never emits; any equality test against the AWS string fails, and a UI that concatenates the field message into its own sentence renders a doubled stop.
  Fix: the validation layer — the boundary/input validator's construction of `ValidationExceptionField.message` for a constraint violation (the shared constraint-violation message renderer this service crate's generated input validation calls), not the GetAgreementPaymentRequest handler body, since all six operations emit the identical string.
  Traces: marketplace-agreement-agreement-payment-request-workflow#task#2.

Cause 2 of 6: SendAgreementPaymentRequest, fixture `marketplace-agreement-agreement-payment-request-workflow`, step `task#3`, at `$.fields[0].message`.
  AWS: Smithy: SendAgreementPaymentRequest input `agreementId` (@required, @pattern ^[A-Za-z0-9_/-]+$), `termId` (@required, @pattern ^[A-Za-z0-9+=;,.@\-_]+$), `name` (@required, @length 5..64), `chargeAmount` (@required, @pattern ^[0-9]*(\.[0-9]{0,8})?$); modeled error ValidationException (@httpError 400). Recorded: HTTP 400 `ValidationException`, first `fields` entry `message: "Member is invalid"`.
  Emulator: HTTP 400 `ValidationException`, `fields[0].message: "Member is invalid."`.
  Why: same as cause 1 — the per-field reason a client reads to point the user at the offending member does not match AWS's wording.
  Fix: same validation-layer field-message renderer.
  Traces: marketplace-agreement-agreement-payment-request-workflow#task#3.

Cause 3 of 6: AcceptAgreementPaymentRequest, fixture `marketplace-agreement-agreement-payment-request-workflow`, step `task#4`, at `$.fields[0].message`.
  AWS: Smithy: AcceptAgreementPaymentRequest input `paymentRequestId` (@required, @pattern ^pr-[a-zA-Z0-9]+$), `agreementId` (@required, @pattern ^[A-Za-z0-9_/-]+$), `purchaseOrderReference` (@length min 1); modeled error ValidationException (@httpError 400). Recorded: HTTP 400 `ValidationException`, `fields[0].message: "Member is invalid"`.
  Emulator: HTTP 400 `ValidationException`, `fields[0].message: "Member is invalid."`.
  Why: same as cause 1.
  Fix: same validation-layer field-message renderer.
  Traces: marketplace-agreement-agreement-payment-request-workflow#task#4.

Cause 4 of 6: RejectAgreementPaymentRequest, fixture `marketplace-agreement-agreement-payment-request-workflow`, step `task#5`, at `$.fields[0].message`.
  AWS: Smithy: RejectAgreementPaymentRequest input `paymentRequestId` (@required, @pattern ^pr-[a-zA-Z0-9]+$), `agreementId` (@required), `rejectionReason` (@length 1..250); modeled error ValidationException (@httpError 400). Recorded: HTTP 400 `ValidationException`, `fields[0].message: "Member is invalid"`.
  Emulator: HTTP 400 `ValidationException`, `fields[0].message: "Member is invalid."`.
  Why: same as cause 1.
  Fix: same validation-layer field-message renderer.
  Traces: marketplace-agreement-agreement-payment-request-workflow#task#5.

Cause 5 of 6: CancelAgreementPaymentRequest, fixture `marketplace-agreement-agreement-payment-request-workflow`, step `task#6`, at `$.fields[0].message`.
  AWS: Smithy: CancelAgreementPaymentRequest input `paymentRequestId` (@required, @pattern ^pr-[a-zA-Z0-9]+$) and `agreementId` (@required, @pattern ^[A-Za-z0-9_/-]+$); modeled error ValidationException (@httpError 400). Recorded: HTTP 400 `ValidationException`, `fields[0].message: "Member is invalid"`.
  Emulator: HTTP 400 `ValidationException`, `fields[0].message: "Member is invalid."`.
  Why: same as cause 1.
  Fix: same validation-layer field-message renderer.
  Traces: marketplace-agreement-agreement-payment-request-workflow#task#6.

Cause 6 of 6: ListAgreementCancellationRequests, fixture `marketplace-agreement-validation-error-paths`, step `task#2`, at `$.fields[name="partyType"].message`.
  AWS: Smithy: ListAgreementCancellationRequests input `partyType` (@required, @length 1..32, @pattern ^[A-Za-z]+$), plus `agreementId`, `status`, `agreementType`, `catalog`, `maxResults` (@range 1..50), `nextToken`; modeled error ValidationException (@httpError 400). Recorded: HTTP 400 `ValidationException` with a `fields` entry `name: "partyType"`, `message: "Member is invalid"`.
  Emulator: HTTP 400 `ValidationException`, same field `name`, `message: "Member is invalid."`.
  Why: same as cause 1, and this one is on a read/list path, so it shows the defect is not confined to the payment-request mutators — it is every validation path of the service.
  Fix: same validation-layer field-message renderer.
  Traces: marketplace-agreement-validation-error-paths#task#2.

Not covered by any existing issue: the one open marketplace-agreement issue (20261109T032733Z-marketplace-agreement-createagreementrequest-accepts-an-intent-outside-the-intent-enum) is about CreateAgreementRequest accepting an out-of-enum `intent` instead of refusing it, i.e. a missing refusal, not the wording of a refusal that already happens. No divergence here reported `stub_path` or `owned_by`.

## Evidence
Replay of trace set simparity-wire-aws-v1: 35 steps, 29 passed, 6 mismatched, score 82.9%, emulator_up=true, bootstrap_signal=false, no blocked/errored steps, no stub_path and no owned_by on any of the six clusters.

All six clusters are status-matched (expected 400 -> actual 400) and diverge on exactly one JSON key:

1. GetAgreementPaymentRequest  — marketplace-agreement-agreement-payment-request-workflow#task#2
   $.fields[name="agreementId"].message: expected 'Member is invalid', got 'Member is invalid.'
2. SendAgreementPaymentRequest — marketplace-agreement-agreement-payment-request-workflow#task#3
   $.fields[0].message: expected 'Member is invalid', got 'Member is invalid.'
3. AcceptAgreementPaymentRequest — marketplace-agreement-agreement-payment-request-workflow#task#4
   $.fields[0].message: expected 'Member is invalid', got 'Member is invalid.'
4. RejectAgreementPaymentRequest — marketplace-agreement-agreement-payment-request-workflow#task#5
   $.fields[0].message: expected 'Member is invalid', got 'Member is invalid.'
5. CancelAgreementPaymentRequest — marketplace-agreement-agreement-payment-request-workflow#task#6
   $.fields[0].message: expected 'Member is invalid', got 'Member is invalid.'
6. ListAgreementCancellationRequests — marketplace-agreement-validation-error-paths#task#2
   $.fields[name="partyType"].message: expected 'Member is invalid', got 'Member is invalid.'

Expected (AWS, all six): HTTP 400, error code ValidationException, fields[].message == "Member is invalid" (fixed text, no trailing period; the member name lives in fields[].name, not in the message).
Actual (emulator, all six): HTTP 400, error code ValidationException, fields[].message == "Member is invalid." (one extra '.').

No per-run value is involved: the diverging bytes are a constant sentence, not a request id, timestamp, generated id, host or port.

Model references used: smithy_query shape=ValidationException,ValidationExceptionField,ValidationExceptionFieldList and operation=GetAgreementPaymentRequest,SendAgreementPaymentRequest,AcceptAgreementPaymentRequest,RejectAgreementPaymentRequest,CancelAgreementPaymentRequest,ListAgreementCancellationRequests on services/service-com-amazonaws-marketplaceagreement-awsmpcommerceservice-v20200301/smithy-model/com-amazonaws-marketplaceagreement-awsmpcommerceservice-v20200301.json. The text itself is not documented in the model, so it is cited as recorded behavior only.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1 (replay_wire_corpus).
2. Representative cause: fixture marketplace-agreement-agreement-payment-request-workflow, step task#2 — call GetAgreementPaymentRequest with an `agreementId` that violates its modeled constraint (@length 1..64, @pattern ^[A-Za-z0-9_/-]+$).
3. Observe HTTP 400 with error code ValidationException and inspect the `fields` list: the entry whose `name` is "agreementId" has `message` "Member is invalid." where the recording has "Member is invalid".
4. Repeat for the other five causes, each of which refuses a constrained input member the same way:
   - marketplace-agreement-agreement-payment-request-workflow#task#3 (SendAgreementPaymentRequest)
   - marketplace-agreement-agreement-payment-request-workflow#task#4 (AcceptAgreementPaymentRequest)
   - marketplace-agreement-agreement-payment-request-workflow#task#5 (RejectAgreementPaymentRequest)
   - marketplace-agreement-agreement-payment-request-workflow#task#6 (CancelAgreementPaymentRequest)
   - marketplace-agreement-validation-error-paths#task#2 (ListAgreementCancellationRequests, field name "partyType")
   Each returns fields[].message "Member is invalid." instead of "Member is invalid".
5. After fixing the field-message renderer, re-replay ALL SIX fixture#step bindings above (not just task#2): the fix is one renderer but six recorded steps assert it, and the bundle is only closed when every one of them passes.
