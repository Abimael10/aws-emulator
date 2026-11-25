Issue-Source: trace-sweep-divergence
Issue-Trace: marketplace-agreement-validation-error-paths
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: marketplace-agreement-validation-error-paths
Issue-Shape-Id: com.amazonaws.marketplaceagreement#AWSMPCommerceService_v20200301
Issue-Key: marketplace-agreement|ListAgreementCancellationRequests|4xx->4xx|$.fields[name="partyType"].message
Issue-Commit: 27fe05928240d46dd0e5c39fef588ae5ad5ce26b

# ValidationException field message carries a trailing period AWS does not emit

> _Note: this issue was identified against the code base at commit `27fe05928240d46dd0e5c39fef588ae5ad5ce26b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListAgreementCancellationRequests

## Description
Cause 1 of 1: the `ValidationExceptionField$message` the emulator emits for the unacceptable-value refusal class ends in a period, where AWS's wording for this service has none.

AWS: `ListAgreementCancellationRequests` models `partyType` as a required `PartyType` (`@length(1,32)`, pattern `^[A-Za-z]+$`) and declares `ValidationException` (HTTP 400) for it (Smithy: ListAgreementCancellationRequests input member `partyType` required; error ValidationException httpError 400). `ValidationException` carries `requestId`, `message`, `reason` (`ValidationExceptionReason`, which includes `INVALID_PARTY_TYPE`) and `fields` (`ValidationExceptionFieldList` of `ValidationExceptionField{name, message}` — `name` is "The name of the field associated with the error", `message` is the per-field text). The model does not state the per-field sentence, so for the exact text this is recorded behavior only: the recording answers HTTP 400 with the `ValidationException` body and, for the entry whose `name` is `partyType`, the field message `Member is invalid` — no trailing period. In that sentence the whole string is FIXED text for the invalid-value class; nothing in it is taken from the request or from state (the request-derived part of the refusal is carried by `fields[].name`, which is the member path `partyType`, and by `reason` = `INVALID_PARTY_TYPE`). The HTTP status (400), the error code (`ValidationException`) and `fields[].name` all match; only the fixed sentence diverges.

Emulator: for the same step the emulator answers HTTP 400 `ValidationException` with `$.fields[name="partyType"].message` = `Member is invalid.` — the same sentence plus a trailing `.`. This is not specific to `partyType` or to this operation: the emulator keeps two separate fixed sentences for its refusal classes, `Member is invalid.` for the unacceptable-value class and `Member is invalid` for the unresolved-reference class, and routes every reason it classifies as an invalid value (an out-of-set party type, catalog, agreement type, filter name, sort attribute, next token, and every boundary constraint violation the input walk raises) to the period-terminated one. So every invalid-value refusal of this service, in every operation, carries the extra character, while the unresolved-reference refusals already use AWS's wording.

Why: the field message is part of the error contract. A client that branches on, asserts on, logs-matches or snapshot-compares the per-field message — including SDK integration tests and any "expected error text" fixture — sees a string that is one character longer than AWS's and treats an otherwise correct refusal as an unknown one. It also makes the service internally inconsistent: two refusals the same client sees from the same API disagree on the punctuation of the same sentence, so no single comparison works for both.

Fix: the service's shared `ValidationException` field-message construction — the fixed sentence the unacceptable-value refusal class states, i.e. the constant `RefusalKind::Invalid` maps to in `field_message()`, which both the boundary-validator path (`validation_exception_for`) and the handler-raised path (`validation_exception` / `validation_exception_naming` / `validation_exception_in_kind`) resolve through. Changing the one constant fixes every operation at once; no handler needs editing. Do not special-case `partyType`: the sentence is class-wide, not per-member.

Traces: marketplace-agreement-validation-error-paths#task#2 (`ListAgreementCancellationRequests` with a `partyType` that is neither `Proposer` nor `Acceptor`).

## Evidence
Replay step marketplace-agreement-validation-error-paths#task#2, operation ListAgreementCancellationRequests, verdict `mismatch`.

Status: expected 400, actual 400 (matches).
Error shape/code: ValidationException (matches). reason `INVALID_PARTY_TYPE` and `fields[0].name` = `partyType` (match).

Diverging value — $.fields[name="partyType"].message:
  expected (AWS, recorded): "Member is invalid"
  actual   (emulator):      "Member is invalid."

Replay detail verbatim: `$.fields[name="partyType"].message: value_mismatch (expected 'Member is invalid', got 'Member is invalid.')`

Fixed vs variable parts of the message: the entire sentence "Member is invalid" is fixed text for the invalid-value class; the request-derived parts of the refusal live in `fields[].name` (the member path) and `reason`, both of which the emulator already gets right. The emulator appends a trailing `.` that AWS does not.

Source of the defect in the checkout: services/service-com-amazonaws-marketplaceagreement-awsmpcommerceservice-v20200301/src/state/failures.rs:72 defines `FIELD_INVALID = "Member is invalid."` while :79 defines `FIELD_INVALID_REFERENCE = "Member is invalid"`; :122-129 (`RefusalKind::field_message`) maps the invalid-value class to the former. The handler reaches it at services/service-com-amazonaws-marketplaceagreement-awsmpcommerceservice-v20200301/src/ops/list_agreement_cancellation_requests.rs:180-187 via `crate::state::validation_exception("INVALID_PARTY_TYPE", "partyType")`.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture `marketplace-agreement-validation-error-paths`, step `task#2`.
2. Equivalently, by hand: call `ListAgreementCancellationRequests` on the marketplace-agreement endpoint with a `partyType` that satisfies the modeled constraints (length 1..32, `^[A-Za-z]+$`) but is neither `Proposer` nor `Acceptor` — e.g. `{"partyType": "Bystander"}`.
3. Observe HTTP 400 with a `ValidationException` body whose `reason` is `INVALID_PARTY_TYPE`.
4. Read `fields[]` and find the entry with `name` == `partyType`; inspect its `message`.
5. The emulator returns `Member is invalid.`; AWS returns `Member is invalid`. The same divergence appears for every other invalid-value refusal of this service (unknown `catalog`, unknown `agreementType`, bad `nextToken`, and any boundary constraint violation), because they all share one fixed sentence.
