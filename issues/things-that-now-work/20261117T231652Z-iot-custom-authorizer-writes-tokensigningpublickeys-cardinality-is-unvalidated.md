Issue-Source: trace-sweep-divergence
Issue-Trace: iot-authorizer-refusal-unresolvable-and-two-key-boundary
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-authorizer-refusal-unresolvable-and-two-key-boundary
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|CreateAuthorizer|4xx->2xx|$.message
Issue-Key: iot|CreateAuthorizer|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: iot|CreateAuthorizer|4xx->2xx|status
Issue-Key: iot|DescribeAuthorizer|4xx->2xx|$.message
Issue-Key: iot|DescribeAuthorizer|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: iot|DescribeAuthorizer|4xx->2xx|status
Issue-Key: iot|DescribeAuthorizer|2xx->2xx|$.authorizerDescription.creationDate
Issue-Key: iot|DescribeAuthorizer|2xx->2xx|$.authorizerDescription.lastModifiedDate
Issue-Related: 20261107T022027Z-iot-registry-and-authorizer-state-10-causes-deletes.md
Issue-Commit: 44962b609081507b4272f265dd79d73cc37af8b1

# iot custom-authorizer writes: tokenSigningPublicKeys cardinality is unvalidated and a created authorizer is reported ACTIVE, leaving a record AWS never creates readable (3 causes)

> _Note: this issue was identified against the code base at commit `44962b609081507b4272f265dd79d73cc37af8b1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAuthorizer

## Description
Three net-new root causes on the IoT custom-authorizer write path, all replayed from the wire fixture iot-authorizer-refusal-unresolvable-and-two-key-boundary. They share one fault surface: CreateAuthorizer records an authorizer for a request AWS refuses, and records it in the wrong status, so every later read of that name disagrees with AWS. Each cause must be closed and re-replayed separately.

Cause 1 of 3: CreateAuthorizer does not validate the cardinality of tokenSigningPublicKeys, so a create AWS refuses 400 is accepted 200 and the authorizer is stored.
AWS: Smithy CreateAuthorizer models InvalidRequestException (HTTP 400, "The request is not valid") and binds tokenSigningPublicKeys to PublicKeyMap (a map of KeyName -> KeyValue with no modelled minimum or maximum entry count), so the cardinality rule is not in the model and is recorded behavior only. The recording shows the refusal is HTTP 400 with the message template `Token signing keys map for authorizer <authorizerName> cannot be null or empty. There must be at least one and at most two token signing public keys in the map`. Fixed text: everything except <authorizerName>; the one variable part is the authorizerName taken from the request's URI label. The sentence also states the rule itself: the map, when present, must hold at least one and at most two keys. Nothing is created, which is why the following DescribeAuthorizer of the same name is a 404 in the recording.
Emulator: the create answers HTTP 200 with authorizerName/authorizerArn and inserts the record; no message and no x-amzn-errortype are produced at all. CreateAuthorizer checks only an upper bound of its own (MAX_TOKEN_SIGNING_PUBLIC_KEYS = 8) and worded with emulator prose (`an authorizer may register at most 8 token signing public keys; this request carries <n>`), so an empty map passes unchecked and a map of three to eight keys — more than AWS's documented maximum of two — is also accepted.
Why: a client relying on AWS's behavior gets an authorizer that real AWS would never have created. Signature validation is then configured with no usable signing key (or with more keys than AWS accepts), the refusal clients key their configuration-error handling on never arrives, and the account is left holding a resource that only exists in the emulator — so the whole misconfiguration passes here and fails at deploy time.
Fix: input validation in the CreateAuthorizer handler (and the same rule in UpdateAuthorizer, which shares the record), replacing the emulator-chosen ceiling of 8 with AWS's one-to-two rule and AWS's sentence for the refusal.
Traces: iot-authorizer-refusal-unresolvable-and-two-key-boundary#task#1 (representative) and #task#3 — two creates in the same cluster, both expected 400 and both answered 200.

Cause 2 of 3: because the refused create was recorded, DescribeAuthorizer answers 200 for a name AWS does not hold.
AWS: Smithy DescribeAuthorizer models ResourceNotFoundException (HTTP 404). A name no authorizer exists under is refused 404 with the message template `Authorizer <authorizerName> not found` — fixed text apart from <authorizerName>, which is the name from the request's URI label.
Emulator: HTTP 200 carrying a full authorizerDescription; no message, no x-amzn-errortype. The emulator's own not-found sentence for this family already matches AWS's wording, so the defect is not the text: it is that the record exists. The same 404 is also missed at the fixture's teardown step, where the authorizer is still readable after the teardown delete did not succeed.
Why: existence checks invert. A client that probes with DescribeAuthorizer to decide whether to create, or that asserts cleanup completed, is told the authorizer is present when AWS reports it absent — and it is handed a description of a resource whose configuration AWS rejected.
Fix: the state written by CreateAuthorizer (nothing may be inserted on a refused create); no change is needed in DescribeAuthorizer's own lookup or refusal wording. This cause is the observable consequence of causes 1 and 3 and must be re-replayed in its own right after they are fixed, because it is the step that proves no record leaked.
Traces: iot-authorizer-refusal-unresolvable-and-two-key-boundary#task#2 (representative), #task#4, #teardown#8.

Cause 3 of 3: a created authorizer is reported in status ACTIVE where AWS reports INACTIVE.
AWS: recorded behavior only — Smithy models CreateAuthorizer$status and AuthorizerDescription$status as AuthorizerStatus (ACTIVE | INACTIVE) and attaches no @default, so the model does not fix the value. The recording shows DescribeAuthorizer reporting authorizerDescription.status = INACTIVE for the authorizer this fixture creates, i.e. an authorizer AWS has not put into use.
Emulator: authorizerDescription.status = ACTIVE. The emulator resolves the create's status member through a constant default of ACTIVE (DEFAULT_AUTHORIZER_STATUS), so an authorizer whose create did not ask to be in use is recorded as in use. Hypothesis: either the requested status is not being carried onto the record or the absent-status default is the wrong one of the two enum values; both land in the same write.
Why: status is the switch that decides whether the authorizer authorizes connections, and it also gates the lifecycle — an ACTIVE authorizer cannot be deleted until it is moved to INACTIVE. A client that creates an authorizer and then tears it down follows AWS's path and is refused here, and a client that reads status to decide whether the authorizer is live is told the opposite of what AWS says. This is visible in the same fixture: the teardown DeleteAuthorizer is answered 400 instead of 200 because the record is ACTIVE (that step is already carried by open issue 20261107T022027Z-iot-registry-and-authorizer-state-10-causes-deletes.md, which is why it is not filed again here).
Fix: the status resolution in CreateAuthorizer's write of the authorizer record (the record constructor's status default in the service state), so the stored status is the one the create asked for and an unspecified status resolves the way AWS reports it.
Traces: iot-authorizer-refusal-unresolvable-and-two-key-boundary#task#6.

Not filed from this fixture: authorizerDescription.creationDate / lastModifiedDate differ only as per-run clock values (the recording asserts their shape, not their value), so they are not a defect. Related open issues that do NOT carry these replay keys and do not cover these causes: 20261106T201842Z-iot-refusal-fidelity-10-net-new-causes-createpolicy.md (DeleteAuthorizer's ACTIVE-delete guard), 20261107T022027Z-iot-registry-and-authorizer-state-10-causes-deletes.md (authorizer deletes/updates), 20261115T161531Z-iot-refusal-text-is-emulator-authored-on-six.md (TestInvokeAuthorizer wording).

## Evidence
Replay of wire fixture iot-authorizer-refusal-unresolvable-and-two-key-boundary (trace set simparity-wire-aws-v1), expected vs actual:

Cause 1 — CreateAuthorizer, steps task#1 and task#3 (cluster service=iot|CreateAuthorizer|status=400->200|keys=$.message,$headers.x-amzn-errortype,status):
  expected: HTTP 400, $.message = 'Token signing keys map for authorizer <authorizerName> cannot be null or empty. There must be at least one and at most two token signing public keys in the map' (recorded with the fixture's generated authorizer name interpolated), $headers.x-amzn-errortype shaped (modelled InvalidRequestException, httpError 400).
  actual:   HTTP 200, no $.message, no $headers.x-amzn-errortype; authorizerName/authorizerArn returned and the record inserted.

Cause 2 — DescribeAuthorizer, steps task#2, task#4, teardown#8 (cluster service=iot|DescribeAuthorizer|status=404->200|keys=$.message,$headers.x-amzn-errortype,status):
  expected: HTTP 404, $.message = 'Authorizer <authorizerName> not found', errortype shaped (modelled ResourceNotFoundException, httpError 404).
  actual:   HTTP 200 with a populated authorizerDescription; $.message and $headers.x-amzn-errortype absent.

Cause 3 — DescribeAuthorizer, step task#6 (cluster service=iot|DescribeAuthorizer|status=200->200|keys=$.authorizerDescription.status,...):
  expected: $.authorizerDescription.status = 'INACTIVE'.
  actual:   $.authorizerDescription.status = 'ACTIVE'.
  (The creationDate/lastModifiedDate entries in the same cluster are per-run clock values and are not part of this finding.)

Code the behavior comes from: services/service-com-amazonaws-iot-awsiotservice/src/ops/create_authorizer.rs lines 212-226 (only the emulator's max-8 key ceiling is enforced; no empty/at-most-two rule) and lines 265-289 (the record is inserted and returned); services/service-com-amazonaws-iot-awsiotservice/src/state.rs line 2154 (DEFAULT_AUTHORIZER_STATUS = "ACTIVE") and line 2233 (status.unwrap_or_else(default)); services/service-com-amazonaws-iot-awsiotservice/src/state/not_found.rs lines 140-142 (the not-found sentence, which already matches AWS).

## Steps to Reproduce
Replay the wire fixture iot-authorizer-refusal-unresolvable-and-two-key-boundary from trace set simparity-wire-aws-v1 against the emulator; all three causes reproduce in that single fixture.

1. (task#1, task#3) POST /authorizer/{authorizerName} — CreateAuthorizer with authorizerFunctionArn set and tokenSigningPublicKeys sent as an empty map (and, for task#3, the second refusable key-map shape in the same cluster). Expect HTTP 400 InvalidRequestException with the 'Token signing keys map for authorizer ... cannot be null or empty. There must be at least one and at most two token signing public keys in the map' message; the emulator answers 200 and creates the authorizer.
2. (task#2, task#4) GET /authorizer/{authorizerName} — DescribeAuthorizer for the name just refused. Expect HTTP 404 ResourceNotFoundException 'Authorizer <name> not found'; the emulator answers 200 with a description.
3. (task#5, task#6) CreateAuthorizer for the two-key boundary case, then GET /authorizer/{authorizerName}. Expect authorizerDescription.status 'INACTIVE'; the emulator reports 'ACTIVE'.
4. (teardown#7, teardown#8) DeleteAuthorizer then DescribeAuthorizer: because the record is ACTIVE the delete is refused 400 and the following describe still answers 200 instead of 404.
