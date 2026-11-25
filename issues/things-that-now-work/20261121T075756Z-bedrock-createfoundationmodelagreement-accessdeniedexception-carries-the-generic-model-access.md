Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock.create-foundation-model-agreement-conflict
Issue-Shape-Id: com.amazonaws.bedrock#AmazonBedrockControlPlaneService
Issue-Key: bedrock.create-foundation-model-agreement-conflict|run#9|AssertionError|assert message is not None and "agreement api" in message.lower(), (
Issue-Commit: 1af00915806e1b56b9f199d941e452ba8fd476f6

# bedrock CreateFoundationModelAgreement AccessDeniedException carries the generic model-access use-case message instead of naming the Agreement API surface

> _Note: this issue was identified against the code base at commit `1af00915806e1b56b9f199d941e452ba8fd476f6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFoundationModelAgreement

## Description
CreateFoundationModelAgreement reaches the correct refusal CLASS but serves the wrong error message for it: the account-class restriction on the Agreement API is reported with the generic "submit your use case details" model-access prose, so the message names a different remediation than the one AWS names. One cause.

Cause 1 of 1: the AccessDeniedException message for a refused CreateFoundationModelAgreement does not name the Agreement API surface (failing check bedrock.create-foundation-model-agreement-conflict).

AWS: Smithy com.amazonaws.bedrock#CreateFoundationModelAgreement -> POST /create-foundation-model-agreement, success http code 202; input offerToken (OfferToken, required) and modelId (BedrockModelId, required, @length max 140, @pattern ^[a-z0-9-]{1,63}[.]{1}[a-z0-9-]{1,63}([a-z0-9-]{1,63}[.]){0,2}[a-z0-9-]{1,63}([:][a-z0-9-]{1,63}){0,2}(/[a-z0-9]{12}|)$); output modelId (required); modeled errors AccessDeniedException (client, httpError 403), ConflictException 400, ValidationException 400, ResourceNotFoundException 404, ThrottlingException 429, InternalServerException 500. The model states the error class and its 403 status but says nothing about the message text, so for the message this is recorded behavior only: the recording this check encodes shows that when the account class is not permitted to create a foundation-model agreement, AWS refuses the MUTATING call with AccessDeniedException whose message names the Agreement API surface specifically (the check asserts the message contains "agreement api", case-insensitive), while the READ side of the same surface (ListFoundationModelAgreementOffers) keeps returning an outstanding offer and GetFoundationModelAvailability keeps reporting unchanged agreementAvailability/authorizationStatus/entitlementAvailability/regionAvailability. The fixed text of that message is the sentence identifying the Agreement API as the restricted surface; its only variable parts would be request/state values such as the modelId or the account identifier, never a per-run token. The 403 status and the AccessDeniedException code are taken from the recorded response and agree with the model's httpError 403.

Emulator: for the same request the emulator does raise AccessDeniedException (so the class and status are right), but its message is 'You must submit your use case details before you can request access to a model.' - the generic model-access / use-case-submission message, which names neither the Agreement API nor any Agreement-surface restriction. Replay detail: AssertionError "expected the denial message to name the Agreement API surface specifically (this account class's restriction), not a generic permissions message; got 'You must submit your use case details before you can request access to a model.'" (check_bedrock_create_foundation_model_agreement_conflict.py line 60).

Why: a client that is refused on the Agreement API is told to perform a remediation that will not unblock it. The AccessDeniedException message is the only part of this refusal that distinguishes "this account class may not use the Agreement API at all" from "this account has not yet submitted use-case details for model access" - the two have different, non-overlapping fixes (the first is an account-class/entitlement restriction, the second is PutUseCaseForModelAccess). An operator or a provisioning script keyed on the message will call PutUseCaseForModelAccess and retry forever against a surface that is structurally closed, and support tooling that greps the denial text to classify model-access failures mis-buckets every Agreement-API denial as a missing use-case submission. Hypothesis: the handler routes every AccessDenied on the model-access/agreement surface through one shared message constant (the use-case one) rather than emitting the Agreement-API restriction message on the CreateFoundationModelAgreement path.

Fix: the CreateFoundationModelAgreement handler's error mapping / message construction - emit the Agreement-API restriction message (naming the Agreement API surface) with HTTP 403 and x-amzn-errortype AccessDeniedException on this path, and keep the use-case-submission message on the model-access paths that actually record it. The read side must stay unaffected: the denied call consumes no offer and changes no availability field. Distinct from the open issue 20261121T032137Z-bedrock-createfoundationmodelagreement-answers-404-resourcenotfoundexception-with-an-end.md, which covers the same operation answering 404 ResourceNotFoundException with an end-of-life message for an unusable modelId - a different input path, a different error class, and a different message; that issue does not cover this AccessDenied message, and the replay lists this check with no owner.

Traces: behavioral check bedrock.create-foundation-model-agreement-conflict.

## Evidence
Failing check: bedrock.create-foundation-model-agreement-conflict (verdict mismatch; service heading ### bedrock; not marked ALREADY FILED and not marked OWNED by any issue).

Replay detail (tail):
  File "check_bedrock_create_foundation_model_agreement_conflict.py", line 60, in run
    assert message is not None and "agreement api" in message.lower(), (
  AssertionError: expected the denial message to name the Agreement API surface specifically (this account class's restriction), not a generic permissions message; got 'You must submit your use case details before you can request access to a model.'

What passed before the failure (so the divergence is isolated to the message):
  - ListFoundationModelAgreementOffers returned an outstanding offer for a candidate marketplace model, twice independently (positive control on the read side).
  - CreateFoundationModelAgreement raised ClientError with Error.Code == "AccessDeniedException" - the class assertion at line 56 passed.
  - The failure is solely on Error.Message.

Expected vs actual:
  expected: AccessDeniedException (HTTP 403) whose message names the Agreement API surface (case-insensitive substring "agreement api"), identifying the account-class restriction on the Agreement API.
  actual:   AccessDeniedException (HTTP 403) with message 'You must submit your use case details before you can request access to a model.'

Requirement split for the message: the fixed text is the sentence naming the Agreement API as the surface this account class may not use; the only legitimately variable parts are values drawn from the request or emulator state (the modelId, an account identifier). No part of it is a per-run value such as a request id, a timestamp or an offer token - each ListFoundationModelAgreementOffers call mints a fresh encrypted offerToken, and the check deliberately asserts only the PRESENCE of an outstanding offer, never token equality, so nothing here depends on a live account or a per-run value. The emulator's string is not a wording variant of the required message: it is a different error's message (the model-access use-case-submission prose).

Model evidence: Smithy com.amazonaws.bedrock#CreateFoundationModelAgreement - POST /create-foundation-model-agreement, success code 202; input offerToken (OfferToken, required), modelId (BedrockModelId, required, @length max 140, @pattern ^[a-z0-9-]{1,63}[.]{1}[a-z0-9-]{1,63}([a-z0-9-]{1,63}[.]){0,2}[a-z0-9-]{1,63}([:][a-z0-9-]{1,63}){0,2}(/[a-z0-9]{12}|)$); output modelId required; errors AccessDeniedException (client, httpError 403), ConflictException 400, ValidationException 400, ResourceNotFoundException 404, ThrottlingException 429, InternalServerException 500. The model constrains the class and status only; the message text is recorded behavior.

## Steps to Reproduce
1. Clone this checkout and boot the emulator from it.
2. Replay the behavioral suite simparity-behavioral-aws-v1 and run check bedrock.create-foundation-model-agreement-conflict (group error-semantics, services [bedrock]).
3. Call ListFoundationModelAgreementOffers on the bedrock control-plane endpoint for a marketplace model that has an outstanding offer (the check probes anthropic.claude-v2, anthropic.claude-3-sonnet-20240229-v1:0, anthropic.claude-3-haiku-20240307-v1:0, meta.llama3-70b-instruct-v1:0, cohere.command-text-v14, ai21.j2-ultra-v1 in order) and take its offerToken.
4. Call CreateFoundationModelAgreement with that offerToken and modelId.
5. Observe the emulator's refusal: AccessDeniedException with Error.Message 'You must submit your use case details before you can request access to a model.'
6. Expected (recorded AWS behavior): AccessDeniedException (HTTP 403) whose Error.Message names the Agreement API surface - the account-class restriction on that API - rather than the generic model-access use-case-submission message.
7. Confirm the rest of the check still holds after the fix: ListFoundationModelAgreementOffers must still show an outstanding offer after the denied call, and GetFoundationModelAvailability's agreementAvailability / authorizationStatus / entitlementAvailability / regionAvailability must be unchanged.
8. Re-replay bedrock.create-foundation-model-agreement-conflict to confirm the fix.
