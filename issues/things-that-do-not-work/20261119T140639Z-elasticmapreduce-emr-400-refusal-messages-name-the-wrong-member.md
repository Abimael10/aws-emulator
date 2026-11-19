Issue-Source: trace-sweep-divergence
Issue-Trace: elasticmapreduce-application-ui-error-paths
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticmapreduce-application-ui-error-paths emr-studio-create-invalid-vpc-error
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: elasticmapreduce|GetPersistentAppUIPresignedURL|4xx->4xx|$.message
Issue-Key: elasticmapreduce|CreateStudio|4xx->4xx|$.Message
Issue-Commit: 365042aa5aff1c761f3edc665c19518d88f112ee

# EMR 400 refusal messages name the wrong member and append a synthesized SDK trailer

> _Note: this issue was identified against the code base at commit `365042aa5aff1c761f3edc665c19518d88f112ee`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetPersistentAppUIPresignedURL

## Description
This issue bundles 2 net-new root causes in the EMR crate (service token `elasticmapreduce`) that share ONE fault: on a 400 InvalidRequestException the emulator authors the exception's message itself instead of serving the message real EMR serves. The HTTP status (400) and the exception type (InvalidRequestException) are correct in both causes; only the message body diverges. In one case the sentence blames a member that is not even part of the operation's input; in the other the emulator appends a client-side SDK trailer (service name, status code, error code and a locally generated request id) into the wire message field.

In each cause the FIXED text of the sentence is the requirement; its variable parts come from the request or from emulator state, as marked. The concrete ids in the recordings are per-run request values and are NOT part of the template.

Cause 1 of 2: GetPersistentAppUIPresignedURL refuses an invalid persistent-app-UI type by naming `ProfilerId`.
AWS: Smithy (com.amazonaws.emr#ElasticMapReduce): GetPersistentAppUIPresignedURL(PersistentAppUIId required, PersistentAppUIType: enum PersistentAppUIType with values SHS|TEZ|YTS, ApplicationId, AuthProxyCall, ExecutionRoleArn) -> errors InternalServerError (500), InvalidRequestException (400). The model gives no message text, so the refusal text is recorded behavior only: the recording shows HTTP 400 with message `PersistentAppUIType is not valid.` — entirely fixed text, naming the input member whose value the service rejected (`PersistentAppUIType`), with no variable part.
Emulator: HTTP 400 with message `ProfilerId is not valid.` — the same `<Member> is not valid.` template, but filled with `ProfilerId`, a name that is not a member of GetPersistentAppUIPresignedURLInput at all (the input's identifier member is `PersistentAppUIId`). Hypothesis: a shared not-valid-message helper in the EMR crate is keyed on the wrong member name for this operation.
Why: the message is the only human- and machine-visible discriminator on InvalidRequestException (the exception type is shared by every input fault of this operation), so a client, CLI wrapper or runbook that classifies the refusal by message does not recognise it, and an operator is pointed at a field (`ProfilerId`) that does not exist in the request they sent — they cannot find the member that must be corrected.
Fix: the input validation / refusal branch for GetPersistentAppUIPresignedURL in the EMR crate's operation handlers — the rejected-enum branch must name `PersistentAppUIType` in the `<Member> is not valid.` template rather than `ProfilerId`.
Traces: elasticmapreduce-application-ui-error-paths#task#3.

Cause 2 of 2: CreateStudio's invalid-VPC refusal carries a client-side SDK trailer, including a generated request id, inside the message.
AWS: Smithy: CreateStudio(Name, AuthMode, VpcId, SubnetIds, ServiceRole, WorkspaceSecurityGroupId, EngineSecurityGroupId, DefaultS3Location all required) -> errors InternalServerException (500), InvalidRequestException (400) whose shape is com.amazonaws.emr#InvalidRequestException{ErrorCode, Message}; the model gives no message text, so this is recorded behavior only. The recording refuses a VpcId that does not resolve with HTTP 400 and an InvalidRequestException whose `Message` is asserted by SHAPE (the recorded text carries per-run content and is redacted to `<shaped-not-asserted>`); the emulator's value does not conform to that shape. What the emulator contributes, and what is in scope here, is the structure of the value it authored: a constant member-name sentence followed by a trailer it synthesizes locally. Hypothesis: AWS's `Message` for this refusal states the unresolved VPC taken from the request (fixed sentence text plus the request's VpcId as the variable part) and does not contain an emulator-generated request id.
Emulator: HTTP 400 with `Message` = `VpcId is not valid. (Service: AWSEditors; Status Code: 400; Error Code: InvalidRequestException; Request ID: <generated uuid>; Proxy: null)`. Two things are wrong with that value independently of any per-run id: (a) the ` (Service: ...; Status Code: ...; Error Code: ...; Request ID: ...; Proxy: null)` trailer is the text an AWS Java SDK client appends to an exception's *local* string representation — it is not part of the `Message` member on the wire, and the emulator is baking its own status code, error code and freshly generated request id into the payload; (b) the sentence itself is a constant naming the member (`VpcId`) and carries no value from the request, so the response does not report WHICH vpc id failed to resolve.
Why: a caller that surfaces or logs `Message` gets a doubly-decorated string — its own SDK appends the same trailer again on top of the embedded one — and any client that matches or parses the message sees a request id and status code where AWS has plain sentence text; the embedded request id also differs on every call, so the field is unstable for callers that hash, diff or snapshot error bodies. Because the sentence names only the member, an operator cannot tell which of the request's vpc/subnet ids was rejected.
Fix: the CreateStudio handler's invalid-VPC refusal and the error serializer for com.amazonaws.emr#InvalidRequestException in the EMR crate — render `Message` as the bare sentence (with the offending value from the request interpolated) and stop appending the SDK-style `(Service: ...; Request ID: ...; Proxy: null)` trailer; the status code, error code and request id belong in the HTTP status and the response's request-id header, not inside `Message`.
Traces: emr-studio-create-invalid-vpc-error#task#3.

Related but NOT covering: open issue 20261118T033253Z-elasticmapreduce-emr-authors-its-own-response-text-and-shape.md describes the same family (EMR authoring its own refusal text) for SetVisibleToAllUsers and RunJobFlow, and 20261119T080047Z-elasticmapreduce-emr-auto-scaling-policy-refusals-name-the-cluster.md covers the auto-scaling refusals naming the wrong id. Neither carries a replay key for either step above.

## Evidence
Replay of wire corpus simparity-wire-aws-v1 against the booted emulator (summary: 390 steps total, 365 passed, 25 mismatched, score 93.6%, emulator_up true, bootstrap_signal false). Both clusters below are net-new: empty `stub_path` and empty `owned_by`.

Cause 1 — fixture elasticmapreduce-application-ui-error-paths, step task#3, GetPersistentAppUIPresignedURL, expected status 400 / actual 400:
  `$.message: value_mismatch (expected 'PersistentAppUIType is not valid.', got 'ProfilerId is not valid.')`
  divergence_key: service=elasticmapreduce|GetPersistentAppUIPresignedURL|status=400->400|keys=$.message
  The whole expected sentence is fixed text; the requirement is that the member named is `PersistentAppUIType`, the enum member the request supplied a bad value for.

Cause 2 — fixture emr-studio-create-invalid-vpc-error, step task#3, CreateStudio, expected status 400 / actual 400:
  `$.Message: value_mismatch (expected '<shaped-not-asserted>', got 'VpcId is not valid. (Service: AWSEditors; Status Code: 400; Error Code: InvalidRequestException; Request ID: 73477362-c17c-46ff-9245-8765e78682dc; Proxy: null)')`
  divergence_key: service=elasticmapreduce|CreateStudio|status=400->400|keys=$.Message
  `73477362-c17c-46ff-9245-8765e78682dc` is a value the emulator generated for this run and is not itself the requirement; the defect is that an SDK-style `(Service: ...; Status Code: ...; Error Code: ...; Request ID: ...; Proxy: null)` trailer is serialized inside the `Message` member at all, and that the sentence preceding it is a constant member name carrying no value from the request. The expected value is shape-asserted, and the emulator's value does not conform to that shape.

## Steps to Reproduce
Boot the emulator and replay these steps from wire trace set simparity-wire-aws-v1 (service token `elasticmapreduce`):
1. Fixture `elasticmapreduce-application-ui-error-paths`, step task#3: call GetPersistentAppUIPresignedURL with a valid PersistentAppUIId and a PersistentAppUIType that is not one of SHS|TEZ|YTS; assert HTTP 400, InvalidRequestException, and that the message is exactly `PersistentAppUIType is not valid.` (it must name PersistentAppUIType, not `ProfilerId`).
2. Fixture `emr-studio-create-invalid-vpc-error`, step task#3: call CreateStudio with all required members set and a VpcId that does not resolve; assert HTTP 400 and InvalidRequestException, and assert the `Message` member contains NO `(Service: ...; Status Code: ...; Error Code: ...; Request ID: ...; Proxy: null)` trailer and no emulator-generated request id — it must be the bare refusal sentence, with the unresolved vpc id from the request interpolated as its variable part.
