Issue-Source: trace-sweep-divergence
Issue-Trace: wickr-network-create-and-validation
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wickr-network-create-and-validation
Issue-Shape-Id: com.amazonaws.wickr#WickrAdminApi
Issue-Key: wickr|CreateNetwork|4xx->2xx|$.message
Issue-Related: 20261109T141502Z-wickr-createnetwork-networkname-validation-accepts-invalid-names-and.md
Issue-Commit: 54e739a086e6a8cc29813a5e33a0aae0b96f568c

# CreateNetwork creates on an invalid networkName and answers 201 with an unmodeled success envelope

> _Note: this issue was identified against the code base at commit `54e739a086e6a8cc29813a5e33a0aae0b96f568c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateNetwork

## Description
CreateNetwork's POST /networks write path diverges from AWS in two ways on the same replayed step: it accepts a networkName AWS refuses, and when it does create a network it answers with a status and a body shape the model does not define. A fixer must close BOTH causes before submitting.

Cause 1 of 2: CreateNetwork accepts a networkName that AWS rejects with the modeled 422 ValidationError.
  AWS: recorded behavior only for the name rule itself — Smithy models CreateNetworkRequest.networkName as GenericString, @required, @pattern ^[\S\s]*$, which matches any string, so the validity rule is not expressible from the model. The recording shows AWS refusing the request with HTTP 422 and the modeled error ValidationError (Smithy: ValidationError, @error client, @httpError 422, members `reasons` (ErrorDetailList of ErrorDetail{field,reason}) and `message`), an x-amzn-errortype header naming that error, and body reasons = [{"field": "networkName", "reason": "Network Name is not a valid name"}] with NO top-level `message`. In that message template, "Network Name is not a valid name" is fixed text for this error class and "networkName" is the rejected input member's name taken from the request; no part of it echoes the submitted value, so nothing in it is per-run.
  Emulator: HTTP 201 — the submitted name passes every check the handler applies and the network is created; $.reasons is absent and the x-amzn-errortype header is absent, because no error is raised at all.
  Why: a client relying on AWS to reject an invalid network name instead gets a created network bearing a name AWS would never mint, and a negative test asserting 422/ValidationError sees a success. Every later read of that network then reports state AWS cannot produce.
  Fix: CreateNetwork's input validation in the wickr service crate (the networkName rule must apply AWS's name-validity check, not only a length cap) and the ValidationError it raises.
  Traces: wickr-network-create-and-validation#task#4.

Cause 2 of 2: the CreateNetwork success response uses status 201 and an unmodeled `message` body instead of the modeled 200 CreateNetworkResponse.
  AWS: Smithy binds CreateNetwork to @http(method: POST, uri: "/networks", code: 200), so a successful create answers HTTP 200, and its output shape CreateNetworkResponse has exactly three members — networkId (NetworkId, @length 8..8, @pattern ^[0-9]{8}$), networkName and encryptionKeyArn. There is no `message` member anywhere in the success shape, so AWS never sends a human-readable success string for this operation.
  Emulator: HTTP 201 with a top-level $.message = "Network created successfully" — a status the operation does not model and a body member no success shape declares.
  Why: a client that branches on the modeled success status (200) treats 201 as unexpected, and an SDK deserializing CreateNetworkResponse finds no networkId/networkName to return while carrying a field it cannot map; callers that chain on the returned networkId break.
  Fix: CreateNetwork's handler/serializer in the wickr service crate — emit the modeled 200 with a CreateNetworkResponse body (networkId, networkName, encryptionKeyArn) and drop the synthesized success `message`.
  Traces: wickr-network-create-and-validation#task#4.

Related but not covering: open issue 20261109T141502Z-wickr-createnetwork-networkname-validation-accepts-invalid-names-and.md describes an earlier shape of the accept-invalid-name fault (it records the emulator answering 200 and says nothing about the 201 status or the `message` success envelope), and the replay reports this cluster as unowned by it. The emulator's current response for this step is 201 + {"message": ...}, so the behavior above is filed here in full.

## Evidence
wickr-network-create-and-validation#task#4 (CreateNetwork, POST /networks):
- status: expected 422, got 201
- $.reasons: missing (expected [{'field': 'networkName', 'reason': 'Network Name is not a valid name'}], got None)
- $headers.x-amzn-errortype: missing (expected the ValidationError errortype header, got None)
- $.message: unexpected_present (expected None, got 'Network created successfully')
Model citations: Smithy CreateNetwork @http{POST /networks, code 200}; output CreateNetworkResponse{networkId,networkName,encryptionKeyArn}; error ValidationError @httpError 422 with members reasons (ErrorDetailList of ErrorDetail{field,reason}) and message; input networkName GenericString @required @pattern ^[\S\s]*$.

## Steps to Reproduce
1. POST /networks with accessLevel STANDARD and a networkName that is short enough to pass a plain length cap but is not a valid Wickr network name (the name class the fixture submits).
2. AWS answers HTTP 422 with an x-amzn-errortype header for ValidationError and body reasons = [{"field": "networkName", "reason": "Network Name is not a valid name"}] and no top-level message.
3. The emulator answers HTTP 201 with body {"message": "Network created successfully"} and creates the network.
4. Repeat with a valid networkName: AWS answers HTTP 200 with CreateNetworkResponse{networkId,networkName,encryptionKeyArn}; the emulator answers 201 with the message envelope.
5. Replay fixture wickr-network-create-and-validation, step task#4.
