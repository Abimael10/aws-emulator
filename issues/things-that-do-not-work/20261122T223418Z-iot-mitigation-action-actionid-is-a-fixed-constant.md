Issue-Source: trace-sweep-divergence
Issue-Trace: iot-mitigation-action-crud-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-mitigation-action-crud-lifecycle iot-mitigation-action-device-certificate-lifecycle iot-mitigation-action-device-certificate-type iot-mitigation-action-duplicate-create-error iot-mitigation-action-enable-logging-type iot-mitigation-action-lifecycle-and-errors iot-mitigation-action-lifecycle-thing-group iot-mitigation-action-thing-group-lifecycle iot-mitigation-action-thing-group-quarantine-lifecycle
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|CreateMitigationAction|2xx->2xx|$.actionId
Issue-Key: iot|DescribeMitigationAction|2xx->2xx|$.actionId
Issue-Key: iot|UpdateMitigationAction|2xx->2xx|$.actionId
Issue-Commit: e9e37bbade8f41fa9d76ef1ae2b72b16e9c820eb

# iot mitigation-action actionId is a fixed constant instead of a per-action identifier

> _Note: this issue was identified against the code base at commit `e9e37bbade8f41fa9d76ef1ae2b72b16e9c820eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMitigationAction

## Description
The iot mitigation-action plane emits ONE fixed actionId for every mitigation action instead of minting and storing a distinct identifier per action. The same constant is returned by the create path, by the read path and by the update path, so the three causes below are three surfaces of one defect; a fixer must close all three before submitting.

The literal UUID seen in the replay is NOT the requirement — a generated id differs in every environment and is never asserted literally. The requirement is that the id be a per-action value: distinct for distinct mitigation actions, and stable for one action across Create/Describe/Update. The recordings show exactly that (two different recorded mitigation actions carry two different ids: iot-mitigation-action-crud-lifecycle records one actionId while iot-mitigation-action-device-certificate-lifecycle records another), whereas the emulator answers every one of them with the same string.

No open issue covers this. The nearest neighbour, 20261118T231406Z-iot-createmitigationaction-diverges-on-4-causes-rolearn-validation.md, lists four CreateMitigationAction causes (roleArn syntax, duplicate-create idempotency, conflict wording, actionParams cardinality) and does NOT carry these replay keys or mention actionId generation.

Cause 1 of 3: CreateMitigationAction returns the same actionId for every distinct action.
AWS: Smithy: CreateMitigationAction POST /mitigationactions/actions/{actionName}, code 200, output members actionArn (MitigationActionArn) and actionId (MitigationActionId); MitigationActionId is a plain string with no modeled pattern or length, so uniqueness is recorded behavior only. The recordings show AWS answering 200 with an actionId that is a fresh UUID-shaped value, different for each mitigation action created, and the actionArn ending in the action's own name.
Emulator: answers 200, but the actionId member carries the same constant value on every call, across nine different fixtures that each create a differently named mitigation action (actionArn is correctly per-name; only actionId is frozen).
Why: a client that keys its mitigation actions by actionId — the id is what DescribeAuditMitigationActionsTask and audit-task reporting correlate on — sees all of its distinct actions collapse onto a single identity. Two actions that are separate resources on AWS are indistinguishable here, so any map, cache or de-duplication keyed on actionId silently merges them, and an id-based lookup resolves to the wrong action.
Fix: id generation in the CreateMitigationAction handler and the iot mitigation-action state — generate a fresh identifier when the action record is created and persist it on that record, rather than returning a literal.
Traces: iot-mitigation-action-crud-lifecycle#task#2; also iot-mitigation-action-device-certificate-lifecycle#task#2, iot-mitigation-action-device-certificate-type#task#1, iot-mitigation-action-duplicate-create-error#setup#1, iot-mitigation-action-enable-logging-type#task#1, iot-mitigation-action-lifecycle-and-errors#setup#3, iot-mitigation-action-lifecycle-thing-group#task#3, iot-mitigation-action-thing-group-lifecycle#task#2, iot-mitigation-action-thing-group-quarantine-lifecycle#task#3.

Cause 2 of 3: DescribeMitigationAction serves that same constant actionId rather than the stored id of the action it read.
AWS: Smithy: DescribeMitigationAction GET /mitigationactions/actions/{actionName}, code 200, output actionName, actionType, actionArn, actionId, roleArn, actionParams, creationDate, lastModifiedDate; errors InvalidRequestException(400), ResourceNotFoundException(404), ThrottlingException(400), InternalFailureException(500). Recorded behavior only for the id's provenance: the recording shows the actionId echoed back equal to the id the matching CreateMitigationAction returned for that action, and different between different actions.
Emulator: returns the fixed constant for every action name it is asked about, so the read path does not reflect any per-action state.
Why: the read path is how a client re-discovers an action's id after a restart or in a different process. Because every action reads back with the same id, a client cannot use Describe to tell its actions apart, and a round-trip assertion (create action A and action B, describe both, compare ids) passes on AWS and fails here.
Fix: the DescribeMitigationAction handler's output serializer — populate actionId from the stored mitigation-action record instead of a constant.
Traces: iot-mitigation-action-crud-lifecycle#task#3; also iot-mitigation-action-crud-lifecycle#task#5, iot-mitigation-action-device-certificate-lifecycle#task#3, iot-mitigation-action-device-certificate-type#task#2, iot-mitigation-action-duplicate-create-error#task#3, iot-mitigation-action-enable-logging-type#task#2, iot-mitigation-action-lifecycle-and-errors#task#4, iot-mitigation-action-lifecycle-thing-group#task#4, iot-mitigation-action-thing-group-lifecycle#task#3, iot-mitigation-action-thing-group-quarantine-lifecycle#task#4.

Cause 3 of 3: UpdateMitigationAction returns the same constant actionId.
AWS: Smithy: UpdateMitigationAction PATCH /mitigationactions/actions/{actionName}, code 200, output actionArn and actionId; errors InvalidRequestException(400), ResourceNotFoundException(404), ThrottlingException(400), InternalFailureException(500). Recorded behavior only: the recording shows the update answering with the SAME id the action was created with — the id is the resource's identity and an update does not change it, while remaining different from any other action's id.
Emulator: answers 200 with the fixed constant, so the update path neither reads nor preserves a per-action id.
Why: a client that updates an action and re-keys its local record on the returned id gets the shared constant, so update responses cannot be attributed to the action they updated; the stability guarantee (id unchanged across update) is untestable because every id is already equal.
Fix: the UpdateMitigationAction handler — echo the stored record's identifier after applying the update, from the same state field Cause 1 should populate.
Traces: iot-mitigation-action-crud-lifecycle#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 169 fixtures, 1120 steps, 843 passed, score 75.3%, 45 divergence clusters. These three clusters are the only ones with no owning open issue and no blocking stub.

Cause 1 - fixture iot-mitigation-action-crud-lifecycle, step task#2, operation CreateMitigationAction, status 200 on both sides:
  $.actionId: expected a per-action generated id (harness asserts shape, not the literal: '<shaped-not-asserted>'), actual the fixed constant '4ba3a211-392b-4ee7-9a73-59ccdc648da2'
  cluster affected_count 9 - the same constant was returned for nine differently named mitigation actions created by nine different fixtures (iot-mitigation-action-device-certificate-lifecycle#task#2, iot-mitigation-action-device-certificate-type#task#1, iot-mitigation-action-duplicate-create-error#setup#1, iot-mitigation-action-enable-logging-type#task#1, iot-mitigation-action-lifecycle-and-errors#setup#3, iot-mitigation-action-lifecycle-thing-group#task#3, iot-mitigation-action-thing-group-lifecycle#task#2, iot-mitigation-action-thing-group-quarantine-lifecycle#task#3)

Cause 2 - fixture iot-mitigation-action-crud-lifecycle, step task#3, operation DescribeMitigationAction, status 200 on both sides:
  $.actionId: flagged as value_mismatch - the emulator serves the frozen constant rather than the id stored for the action being read; cluster affected_count 10, spanning every mitigation-action fixture's Describe step

Cause 3 - fixture iot-mitigation-action-crud-lifecycle, step task#4, operation UpdateMitigationAction, status 200 on both sides:
  $.actionId: flagged as value_mismatch - same frozen constant returned by the update path

Cross-fixture evidence that AWS ids are per-action, not constant: the recorded CreateMitigationAction response in iot-mitigation-action-device-certificate-lifecycle#task#4 carries actionId '879d6cf7-3b24-4b9c-aac6-a2df5516c66a', while iot-mitigation-action-crud-lifecycle records '4ba3a211-392b-4ee7-9a73-59ccdc648da2' - two different recorded actions, two different ids. The emulator answers both (and the other seven) with the single value above. Neither literal is a requirement; their being DIFFERENT from each other is.

Model evidence (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json):
  CreateMitigationAction output: actionArn -> MitigationActionArn, actionId -> MitigationActionId (http POST /mitigationactions/actions/{actionName}, code 200)
  DescribeMitigationAction output includes actionId -> MitigationActionId (http GET, code 200)
  UpdateMitigationAction output: actionArn, actionId (http PATCH, code 200)
  MitigationActionId: plain string, no pattern/length constraint - so uniqueness is recorded behavior, not a modeled constraint

## Steps to Reproduce
Boot the emulator from this checkout and drive the iot endpoint (or replay the fixtures named below).

Cause 1: POST /mitigationactions/actions/<nameA> with a valid roleArn and actionParams carrying exactly one parameter type; note the returned actionId. Then POST /mitigationactions/actions/<nameB> (a different name, any valid params) and note its actionId. Expected: two different ids, each actionArn ending in its own name. Observed: both responses carry the identical actionId '4ba3a211-392b-4ee7-9a73-59ccdc648da2'. Replay: iot-mitigation-action-crud-lifecycle#task#2 (and any of the eight other create steps listed in the evidence).

Cause 2: after creating <nameA> and <nameB> above, GET /mitigationactions/actions/<nameA> and GET /mitigationactions/actions/<nameB>. Expected: each response's actionId equals the id its own create returned, and the two differ. Observed: both describe responses return the same constant. Replay: iot-mitigation-action-crud-lifecycle#task#3 and #task#5.

Cause 3: PATCH /mitigationactions/actions/<nameA> changing actionParams or roleArn. Expected: 200 whose actionId is the id <nameA> was created with (unchanged by the update) and distinct from <nameB>'s. Observed: 200 with the same shared constant. Replay: iot-mitigation-action-crud-lifecycle#task#4.
