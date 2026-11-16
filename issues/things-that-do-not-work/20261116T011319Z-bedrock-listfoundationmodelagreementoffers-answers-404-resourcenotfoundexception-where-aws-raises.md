Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-foundation-model-agreement-and-access
Issue-Step: setup#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-foundation-model-agreement-and-access
Issue-Shape-Id: com.amazonaws.bedrock#AmazonBedrockControlPlaneService
Issue-Key: bedrock|ListFoundationModelAgreementOffers|4xx->4xx|status
Issue-Key: bedrock|ListFoundationModelAgreementOffers|4xx->4xx|$.message
Issue-Key: bedrock|ListFoundationModelAgreementOffers|4xx->4xx|$headers.x-amzn-errortype
Issue-Commit: 0be2d8cde67a061e1bdd818ed7a4b8faca2d2bae

# bedrock ListFoundationModelAgreementOffers answers 404 ResourceNotFoundException where AWS raises 400 ValidationException for an unusable modelId

> _Note: this issue was identified against the code base at commit `0be2d8cde67a061e1bdd818ed7a4b8faca2d2bae`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListFoundationModelAgreementOffers

## Description
ListFoundationModelAgreementOffers mis-classifies a modelId it cannot serve offers for: it answers the 404 not-found class instead of the 400 validation class, and it substitutes an emulator-authored end-of-life message for AWS's fixed validation message. Both bundled causes are the same mis-classification on the same operation, reached by two different steps of the same fixture.

Cause 1 of 2: ListFoundationModelAgreementOffers returns HTTP 404 where AWS returns HTTP 400 (fixture bedrock-foundation-model-agreement-and-access, step setup#1).

AWS: Smithy bedrock#ListFoundationModelAgreementOffers -> GET /list-foundation-model-agreement-offers/{modelId}, http code 200, readonly. Input: modelId @httpLabel, required, target BedrockModelId @length max 140 @pattern ^[a-z0-9-]{1,63}[.]{1}[a-z0-9-]{1,63}([a-z0-9-]{1,63}[.]){0,2}[a-z0-9-]{1,63}([:][a-z0-9-]{1,63}){0,2}(/[a-z0-9]{12}|)$; offerType @httpQuery (OfferType enum). Output: modelId (required) and offers (required). Modeled errors: ValidationException (client, httpError 400, doc "Input validation failed. Check your request parameters and retry the request."), AccessDeniedException 403, ResourceNotFoundException (client, httpError 404, doc "The specified resource Amazon Resource Name (ARN) was not found."), ThrottlingException 429, InternalServerException 500. The recorded response for this step is HTTP 400, i.e. AWS places a modelId it will not serve offers for in the ValidationException class, not the ResourceNotFoundException class.

Emulator: answers HTTP 404 instead of 400 for the same request. The replay reports only the status key for this cluster (status: expected 400, got 404).

Why: the 400 and 404 classes are retried and handled differently by every AWS SDK and by client code. ValidationException is a non-retryable request-shape fault that tells the caller to correct the modelId; ResourceNotFoundException tells the caller the model exists as a concept but the resource is absent, which SDK-level resource-waiters and provisioning scripts treat as "not there yet" and poll on. A client that probes model availability before requesting an agreement therefore loops on a request it should have failed outright, and error-class-based branching (catch ValidationException) never fires.

Cause 2 of 2: for an invalid model identifier the emulator returns 404 ResourceNotFoundException with an end-of-life message instead of 400 ValidationException with AWS's invalid-identifier message (fixture bedrock-foundation-model-agreement-and-access, step task#2).

AWS: same operation and same modeled error set as Cause 1. The recording shows AWS answering HTTP 400 with body member message exactly "The provided model identifier is invalid." and an x-amzn-errortype naming ValidationException. That message is entirely FIXED text for this error class -- it interpolates nothing from the request or from state, so no part of it is a per-run value; the whole string is the requirement. The status 400 and the ValidationException error code come from the recorded response and match the model's ValidationException httpError 400.

Emulator: answers HTTP 404 with x-amzn-errortype ResourceNotFoundException and message "This model version has reached the end of its life. Please refer to the AWS documentation for more details." -- a different status, a different error code and a wholly different message. The replay reports status (expected 400, got 404), $.message (value_mismatch) and $headers.x-amzn-errortype (shape_mismatch) on this cluster.

Why: a caller handed an unusable modelId is told the wrong thing three times over. The HTTP status moves the fault out of the client-validation class into the resource-absent class; the x-amzn-errortype header is what the SDK uses to construct the typed exception, so code that catches ValidationException sees ResourceNotFoundException instead; and the message asserts a lifecycle fact (the model version has reached end of life) that the request never established, so an operator reading the error diagnoses a deprecated model rather than a malformed identifier and "fixes" it by pinning a different version. Hypothesis: the handler resolves modelId against its catalog and, on any miss, falls through to a single end-of-life not-found path rather than first validating the identifier against the BedrockModelId pattern/length and the known-model set.

Fix: the ListFoundationModelAgreementOffers handler's request validation and error mapping -- validate modelId before resolving it and raise ValidationException (HTTP 400, x-amzn-errortype ValidationException) with the message "The provided model identifier is invalid." for an identifier the service will not serve offers for, and reserve ResourceNotFoundException (404) and the end-of-life prose for the cases the recording actually shows them on. The same mapping governs both bundled causes, which is why they are one issue.

Traces: bedrock-foundation-model-agreement-and-access#setup#1 (Cause 1) and bedrock-foundation-model-agreement-and-access#task#2 (Cause 2). No open issue owns either cluster -- the replay reports both with an empty owned_by, and the only open bedrock agreement issue, 20261101T114400Z-bedrock-createfoundationmodelagreement-stub.md, covers the unimplemented CreateFoundationModelAgreement scaffold, a different operation.

## Evidence
Cause 1 -- fixture bedrock-foundation-model-agreement-and-access, step setup#1, operation ListFoundationModelAgreementOffers, verdict mismatch, owned_by empty.
  divergence key: service=bedrock|ListFoundationModelAgreementOffers|status=400->404|keys=status
  detail: status: expected 400, got 404

Cause 2 -- fixture bedrock-foundation-model-agreement-and-access, step task#2, operation ListFoundationModelAgreementOffers, verdict mismatch, owned_by empty.
  divergence key: service=bedrock|ListFoundationModelAgreementOffers|status=400->404|keys=$.message,$headers.x-amzn-errortype,status
  detail:
    status: expected 400, got 404
    $.message: value_mismatch -- expected 'The provided model identifier is invalid.', got 'This model version has reached the end of its life. Please refer to the AWS documentation for more details.'
    $headers.x-amzn-errortype: shape_mismatch -- expected '<shaped-not-asserted>', got 'ResourceNotFoundException'

Requirement split for the Cause 2 message: "The provided model identifier is invalid." is 100% fixed text for this error class; it has no variable part drawn from the request or from emulator state, so it must be reproduced verbatim. The emulator's substitute string is not a wording variant of it but a different error's message.

Model evidence: Smithy bedrock#ListFoundationModelAgreementOffers -- GET /list-foundation-model-agreement-offers/{modelId}, code 200, readonly; input modelId @httpLabel required BedrockModelId @length max 140 @pattern ^[a-z0-9-]{1,63}[.]{1}[a-z0-9-]{1,63}([a-z0-9-]{1,63}[.]){0,2}[a-z0-9-]{1,63}([:][a-z0-9-]{1,63}){0,2}(/[a-z0-9]{12}|)$, offerType @httpQuery; output modelId + offers both required; errors ValidationException 400, AccessDeniedException 403, ResourceNotFoundException 404, ThrottlingException 429, InternalServerException 500. Smithy ValidationException: client error, httpError 400. Smithy ResourceNotFoundException: client error, httpError 404.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Cause 1: replay fixture bedrock-foundation-model-agreement-and-access, step setup#1 -- GET /list-foundation-model-agreement-offers/{modelId} against the bedrock endpoint with the fixture's setup modelId. Observe HTTP 404; the recording has HTTP 400.
3. Cause 2: replay the same fixture's step task#2 -- the same operation with an invalid model identifier. Observe HTTP 404, response header x-amzn-errortype: ResourceNotFoundException, and body message "This model version has reached the end of its life. Please refer to the AWS documentation for more details."
4. Expected for step task#2: HTTP 400, x-amzn-errortype naming ValidationException, body message exactly "The provided model identifier is invalid."
5. Both steps must be re-replayed to confirm a fix: a change that only remaps the task#2 message still leaves setup#1 answering 404.
