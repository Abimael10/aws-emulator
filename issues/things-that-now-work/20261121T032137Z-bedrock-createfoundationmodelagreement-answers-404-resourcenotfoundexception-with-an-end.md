Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-foundation-model-agreement-and-access
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-foundation-model-agreement-and-access
Issue-Shape-Id: com.amazonaws.bedrock#AmazonBedrockControlPlaneService
Issue-Key: bedrock|CreateFoundationModelAgreement|4xx->4xx|$.message
Issue-Key: bedrock|CreateFoundationModelAgreement|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: bedrock|CreateFoundationModelAgreement|4xx->4xx|status
Issue-Commit: ddedefe2ba7626fc434a3b439556791987e3d078

# bedrock CreateFoundationModelAgreement answers 404 ResourceNotFoundException with an end-of-life message where AWS raises 400 ValidationException for an unusable modelId

> _Note: this issue was identified against the code base at commit `ddedefe2ba7626fc434a3b439556791987e3d078`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFoundationModelAgreement

## Description
CreateFoundationModelAgreement mis-classifies a modelId it will not sell an agreement for: it answers the 404 resource-absent class instead of the 400 client-validation class, and it substitutes an emulator-authored end-of-life message for AWS's fixed invalid-identifier validation message. One cause.

Cause 1 of 1: CreateFoundationModelAgreement returns HTTP 404 / ResourceNotFoundException with an end-of-life message where AWS returns HTTP 400 / ValidationException with the invalid-identifier message (fixture bedrock-foundation-model-agreement-and-access, step task#3).

AWS: Smithy bedrock#CreateFoundationModelAgreement -> POST /create-foundation-model-agreement, success http code 202. Input: offerToken (target OfferToken, required) and modelId (target BedrockModelId, required, @length max 140, @pattern ^[a-z0-9-]{1,63}[.]{1}[a-z0-9-]{1,63}([a-z0-9-]{1,63}[.]){0,2}[a-z0-9-]{1,63}([:][a-z0-9-]{1,63}){0,2}(/[a-z0-9]{12}|)$). Output: modelId (required). Modeled errors: ValidationException (client, httpError 400, doc "Input validation failed. Check your request parameters and retry the request."), ConflictException 400, AccessDeniedException 403, ResourceNotFoundException (client, httpError 404, doc "The specified resource Amazon Resource Name (ARN) was not found. Check the Amazon Resource Name (ARN) and try your request again."), ThrottlingException 429, InternalServerException 500. The recorded response for this step is HTTP 400 with body member message "The provided model identifier is invalid." and an x-amzn-errortype naming the validation error, i.e. AWS places a model identifier it will not create an agreement for in the ValidationException class, not the ResourceNotFoundException class. That message is entirely FIXED text for this error class: it interpolates nothing from the request and nothing from service state, so no part of it is a per-run value and the whole string is the requirement. The status 400 and the ValidationException error code are taken from the recorded response and agree with the model's ValidationException httpError 400.

Emulator: for the same request it answers HTTP 404 with header x-amzn-errortype: ResourceNotFoundException and body message "This model version has reached the end of its life. Please refer to the AWS documentation for more details." - a different status, a different error code and a wholly different message. The replay reports, on this cluster, status (expected 400, got 404), $.message (value_mismatch: expected 'The provided model identifier is invalid.', got 'This model version has reached the end of its life. Please refer to the AWS documentation for more details.') and $headers.x-amzn-errortype (shape_mismatch, got 'ResourceNotFoundException').

Why: a client handed an unusable modelId is told the wrong thing three times over. The HTTP status moves the fault out of the non-retryable client-validation class into the resource-absent class, which SDK resource waiters and provisioning scripts treat as "not there yet" and poll on, so a subscription/agreement workflow loops on a request that should have failed outright. The x-amzn-errortype header is what every AWS SDK uses to build the typed exception, so caller code that catches ValidationException never fires and catches ResourceNotFoundException instead. And the message asserts a lifecycle fact - that this model version has reached end of life - that the request never established, so an operator diagnoses a deprecated model and "fixes" it by pinning another version rather than correcting a malformed/unknown identifier. Hypothesis: the handler resolves modelId against its catalog and, on any miss, falls through to a single end-of-life not-found path instead of first validating the identifier against the BedrockModelId pattern/length and the known-model set.

Fix: the CreateFoundationModelAgreement handler's request validation and error mapping - validate modelId before resolving it and raise ValidationException (HTTP 400, x-amzn-errortype ValidationException) with message "The provided model identifier is invalid." for an identifier the service will not create an agreement for, reserving ResourceNotFoundException (404) and the end-of-life prose for the cases the recordings actually show them on. This is the same mis-classification already filed for a different operation in the open issue 20261116T011319Z-bedrock-listfoundationmodelagreementoffers-answers-404-resourcenotfoundexception-where-aws-raises.md (ListFoundationModelAgreementOffers); that issue does not cover CreateFoundationModelAgreement, and the replay reports this cluster with an empty owned_by. The earlier stub issue 20261101T114400Z-bedrock-createfoundationmodelagreement-stub.md is resolved (the operation now answers) and therefore covers nothing here.

Traces: bedrock-foundation-model-agreement-and-access#task#3.

## Evidence
Cause 1 - fixture bedrock-foundation-model-agreement-and-access, step task#3, service bedrock, operation CreateFoundationModelAgreement, verdict mismatch, owned_by empty, stub_path empty.
  divergence key: service=bedrock|CreateFoundationModelAgreement|status=400->404|keys=$.message,$headers.x-amzn-errortype,status
  expected_status 400, actual_status 404
  detail:
    status: expected 400, got 404
    $.message: value_mismatch - expected 'The provided model identifier is invalid.', got 'This model version has reached the end of its life. Please refer to the AWS documentation for more details.'
    $headers.x-amzn-errortype: shape_mismatch - expected '<shaped-not-asserted>', got 'ResourceNotFoundException'

Requirement split for the message: "The provided model identifier is invalid." is 100% fixed text for this error class - it has no variable part drawn from the request or from emulator state (no model id, name, number or timestamp is interpolated), so it must be reproduced verbatim. The emulator's string is not a wording variant of it but a different error's message.

Model evidence: Smithy bedrock#CreateFoundationModelAgreement - POST /create-foundation-model-agreement, success code 202; input offerToken (OfferToken, required) and modelId (BedrockModelId, required, @length max 140, @pattern ^[a-z0-9-]{1,63}[.]{1}[a-z0-9-]{1,63}([a-z0-9-]{1,63}[.]){0,2}[a-z0-9-]{1,63}([:][a-z0-9-]{1,63}){0,2}(/[a-z0-9]{12}|)$); output modelId required; errors ValidationException 400, ConflictException 400, AccessDeniedException 403, ResourceNotFoundException 404, ThrottlingException 429, InternalServerException 500. Smithy com.amazonaws.bedrock#ValidationException: client error, httpError 400, member message (NonBlankString). Smithy com.amazonaws.bedrock#ResourceNotFoundException: client error, httpError 404.

## Steps to Reproduce
1. Clone this checkout, boot the emulator from it, and replay the wire corpus simparity-wire-aws-v1.
2. Replay fixture bedrock-foundation-model-agreement-and-access, step task#3: POST /create-foundation-model-agreement against the bedrock endpoint with the fixture's offerToken and its unusable modelId.
3. Observe the emulator's response: HTTP 404, response header x-amzn-errortype: ResourceNotFoundException, body member message "This model version has reached the end of its life. Please refer to the AWS documentation for more details."
4. Expected (recorded AWS behavior): HTTP 400, x-amzn-errortype naming ValidationException, body member message exactly "The provided model identifier is invalid."
5. Re-replay bedrock-foundation-model-agreement-and-access#task#3 to confirm a fix; all three keys (status, $.message, $headers.x-amzn-errortype) must pass.
