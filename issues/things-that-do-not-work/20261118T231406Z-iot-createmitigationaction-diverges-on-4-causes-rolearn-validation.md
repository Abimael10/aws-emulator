Issue-Source: trace-sweep-divergence
Issue-Trace: iot-mitigation-action-invalid-role-arn-error
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-mitigation-action-invalid-role-arn-error iot-mitigation-action-invalid-role-error iot-mitigation-action-device-certificate-lifecycle iot-mitigation-action-crud-lifecycle iot-mitigation-action-duplicate-create-error iot-mitigation-action-lifecycle-and-errors iot-mitigation-action-error-paths
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|CreateMitigationAction|4xx->2xx|$.message
Issue-Key: iot|CreateMitigationAction|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: iot|CreateMitigationAction|4xx->2xx|status
Issue-Key: iot|CreateMitigationAction|2xx->4xx|$.actionArn
Issue-Key: iot|CreateMitigationAction|2xx->4xx|$.actionId
Issue-Key: iot|CreateMitigationAction|2xx->4xx|status
Issue-Key: iot|CreateMitigationAction|4xx->4xx|$.message
Issue-Key: iot|CreateMitigationAction|4xx->4xx|$.resourceArn
Issue-Key: iot|CreateMitigationAction|4xx->4xx|$.resourceId
Issue-Commit: fc9bc334740807605983ba8f6dbcf0336ba99ba2

# iot CreateMitigationAction diverges on 4 causes: roleArn validation, duplicate-create idempotency and refusal shape

> _Note: this issue was identified against the code base at commit `fc9bc334740807605983ba8f6dbcf0336ba99ba2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMitigationAction

## Description
The iot mitigation-action write path diverges from AWS on FOUR independent causes. All four are in CreateMitigationAction: one accepts input AWS refuses, one refuses a call AWS accepts, and two word/shape a refusal differently from AWS. No open issue covers the mitigation-action plane (the nearest neighbours — 20261114T082236Z-iot-refusal-message-fidelity-7-causes-addthingtothinggroup-createstream.md and 20261115T161531Z-iot-refusal-text-is-emulator-authored-on-six.md — list other operations and do not carry these replay keys). A fixer must close all four before submitting.

Cause 1 of 4: an unparsable roleArn is accepted instead of refused.
AWS: Smithy: CreateMitigationAction.roleArn is @required, target RoleArn @length min 20 max 2048; error InvalidRequestException (client, httpError 400). Length is not the only gate — AWS also parses the value as an IAM role ARN and refuses a syntactically invalid one that satisfies @length with HTTP 400, header x-amzn-errortype 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/' and body message 'Invalid Role ARN:: <roleArn>'. The fixed text of that message is 'Invalid Role ARN:: ' (note the doubled colon); the only variable part is the roleArn value echoed verbatim from the request.
Emulator: answers HTTP 200 with an actionArn/actionId and creates the mitigation action; no message and no x-amzn-errortype header are emitted.
Why: a client that relies on CreateMitigationAction to reject a malformed role gets a mitigation action that exists in the emulator but could never exist on AWS; the account's error-handling path for a bad role is never exercised, and every later Describe/Update/Start on that bogus action succeeds here and fails on AWS.
Fix: input validation for CreateMitigationAction in the iot service's mitigation-action handler — add the roleArn ARN-syntax check after the modeled @length check and map it to InvalidRequestException.
Traces: iot-mitigation-action-invalid-role-arn-error#task#1, iot-mitigation-action-invalid-role-error#task#1.

Cause 2 of 4: re-creating an existing action with identical attributes is refused 409 instead of accepted 200.
AWS: recorded behavior only (the model marks ResourceAlreadyExistsException as a possible error but states no condition). The recording shows CreateMitigationAction on a name that already exists, with the same actionParams and roleArn, answering HTTP 200 with the action's actionArn and actionId rather than a conflict. That is consistent with AWS's own conflict wording for this operation, which fires only 'for account with different attributes' (see Cause 3): the conflict is attribute-sensitive, not name-sensitive.
Emulator: answers HTTP 409 and emits no actionArn/actionId, so the step that should re-affirm the existing action fails outright.
Why: a client that creates its mitigation actions idempotently on every deploy — the normal pattern for a declarative bootstrap — gets a hard 409 on the second run against the emulator where AWS returns the existing action, so the deploy aborts on a path AWS treats as a no-op.
Fix: the conflict precondition in the CreateMitigationAction handler — compare the incoming actionParams/roleArn against the stored action and only raise the conflict when they differ; otherwise return the stored action's arn and id with 200.
Traces: iot-mitigation-action-device-certificate-lifecycle#task#4.

Cause 3 of 4: the genuine duplicate-name conflict is emulator-authored and carries members AWS does not send.
AWS: Smithy: error ResourceAlreadyExistsException (client, httpError 409) with members message, resourceId, resourceArn. On a conflicting CreateMitigationAction the recorded body carries message ONLY — resourceId and resourceArn are absent — and the message is 'Resource already exists for account with different attributes, <mitigation action ARN>'. Fixed text: 'Resource already exists for account with different attributes, '; the single variable part is the ARN of the already-existing mitigation action, taken from the emulator's state (arn:aws:iot:<region>:<account>:mitigationaction/<actionName>).
Emulator: status 409 is right, but the message is 'MitigationAction cannot be created - name already exists (name=<actionName>)' and the body additionally populates resourceArn and resourceId.
Why: a client that matches or logs the AWS sentence, or that treats the presence of resourceArn/resourceId as meaningful, reads a different body than AWS sends for the same conflict — error-message assertions and body-shape assertions both break.
Fix: the serializer for ResourceAlreadyExistsException on the CreateMitigationAction path — emit AWS's sentence with the existing action's ARN interpolated, and stop populating resourceId/resourceArn for this error.
Traces: iot-mitigation-action-crud-lifecycle#task#6, iot-mitigation-action-duplicate-create-error#task#2, iot-mitigation-action-lifecycle-and-errors#task#5.

Cause 4 of 4: the 'no action parameter set' refusal is emulator-authored.
AWS: Smithy: MitigationActionParams doc — 'The set of parameters for this mitigation action. You can specify only one type of parameter (in other words, you can apply only one action for each defined mitigation action)'; error InvalidRequestException (client, httpError 400). When actionParams sets no parameter type at all, AWS answers 400 with message 'Expected only one action parameter to be set'. That message is entirely fixed text — it has no variable part and never names the member or the offending value.
Emulator: status 400 is right, but the message is 'actionParams must define one type of mitigation action, and defines none: each mitigation action can apply only one type of change'.
Why: a client surfacing or matching the service's refusal sentence for a malformed actionParams gets prose AWS never emits, so the same bad request is reported differently to the operator and to any test that pins the message.
Fix: the actionParams cardinality check in CreateMitigationAction's validation — emit AWS's fixed sentence for both the none-set and more-than-one-set cases.
Traces: iot-mitigation-action-error-paths#task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1 (169 fixtures, 1120 steps, score 65.8%).

Cause 1 — fixture iot-mitigation-action-invalid-role-arn-error, step task#1, operation CreateMitigationAction:
  expected status 400, actual status 200
  expected $.message = 'Invalid Role ARN:: not-a-valid-arn-but-long-enough-to-pass-length-check', actual = absent
  expected $headers.x-amzn-errortype = 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/', actual = absent
  also affected: iot-mitigation-action-invalid-role-error#task#1

Cause 2 — fixture iot-mitigation-action-device-certificate-lifecycle, step task#4, operation CreateMitigationAction:
  expected status 200, actual status 409
  expected $.actionArn = 'arn:aws:iot:us-west-2:000000000000:mitigationaction/sim2real-probe-iot-mit-devcert-<suffix>', actual = absent
  expected $.actionId present, actual = absent
  (the same name/params were created earlier in the same fixture at task#2, which passed)

Cause 3 — fixture iot-mitigation-action-crud-lifecycle, step task#6, operation CreateMitigationAction (status 409 both sides):
  expected $.message = 'Resource already exists for account with different attributes, arn:aws:iot:us-west-2:000000000000:mitigationaction/<actionName>'
  actual   $.message = 'MitigationAction cannot be created - name already exists (name=<actionName>)'
  expected $.resourceArn = absent, actual = 'arn:aws:iot:us-west-2:000000000000:mitigationaction/<actionName>'
  expected $.resourceId = absent, actual = the action's generated id
  also affected: iot-mitigation-action-duplicate-create-error#task#2, iot-mitigation-action-lifecycle-and-errors#task#5

Cause 4 — fixture iot-mitigation-action-error-paths, step task#1, operation CreateMitigationAction (status 400 both sides):
  expected $.message = 'Expected only one action parameter to be set'
  actual   $.message = 'actionParams must define one type of mitigation action, and defines none: each mitigation action can apply only one type of change'

Model evidence (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json):
  CreateMitigationAction POST /mitigationactions/actions/{actionName}, code 200; input actionName (@required, @length 0..128, @pattern ^[a-zA-Z0-9_-]+$), roleArn (@required, RoleArn @length 20..2048), actionParams (@required); errors InvalidRequestException(400), ResourceAlreadyExistsException(409), LimitExceededException(410), ThrottlingException(400), InternalFailureException(500).
  ResourceAlreadyExistsException members: message, resourceId, resourceArn.
  MitigationActionParams doc: 'You can specify only one type of parameter ...'.

## Steps to Reproduce
Boot the emulator from this checkout and replay the four fixtures below (or exercise the calls directly against the iot endpoint).

Cause 1: POST /mitigationactions/actions/<name> with actionParams carrying exactly one parameter type, a valid-length but non-ARN roleArn (e.g. a 50-character non-ARN string), and observe the response. Expected: 400, x-amzn-errortype 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/', message 'Invalid Role ARN:: <that value>'. Observed: 200 with actionArn/actionId. Replay: iot-mitigation-action-invalid-role-arn-error#task#1.

Cause 2: CreateMitigationAction <name> with updateDeviceCertificateParams and a valid roleArn (200). DescribeMitigationAction <name>. CreateMitigationAction <name> AGAIN with the identical roleArn and actionParams. Expected: 200 with the action's actionArn and actionId. Observed: 409. Replay: iot-mitigation-action-device-certificate-lifecycle#task#4.

Cause 3: CreateMitigationAction <name> (200), then CreateMitigationAction <name> with DIFFERENT actionParams. Expected: 409 whose body carries only message = 'Resource already exists for account with different attributes, <existing action ARN>'. Observed: 409 with the emulator-authored 'MitigationAction cannot be created - name already exists (name=<name>)' plus resourceArn and resourceId members. Replay: iot-mitigation-action-crud-lifecycle#task#6.

Cause 4: CreateMitigationAction <name> with actionParams = {} (no parameter type set) and a valid roleArn. Expected: 400 message 'Expected only one action parameter to be set'. Observed: 400 with 'actionParams must define one type of mitigation action, and defines none: ...'. Replay: iot-mitigation-action-error-paths#task#1.
