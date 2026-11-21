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
Issue-Commit: 3d40b4bdd1a74693919d044a2652faf1f3ecdc56

# iot mitigation-action actionId is a per-run random UUID, so create, describe and update all report an unmatchable action identity

> _Note: this issue was identified against the code base at commit `3d40b4bdd1a74693919d044a2652faf1f3ecdc56`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMitigationAction

## Description
Three replay clusters, all service=iot, all expected_status=200 -> actual_status=200, diverge on the SAME output member: actionId. The three operations of the Device Defender mitigation-action plane that report it - CreateMitigationAction (9 affected steps), DescribeMitigationAction (10 affected steps) and UpdateMitigationAction (1 affected step) - each answer the modelled 200 with the modelled members; what diverges is the action IDENTITY they report. No status, error code or error message is involved on any of the three: both sides are 200 and no error body is emitted.

Shared root cause. The id every one of the three reports is the id state minted on create. In this checkout services/service-com-amazonaws-iot-awsiotservice/src/ops/create_mitigation_action.rs:235 passes crate::state::fresh_uuid() as the record's action_id; crate::state::fresh_uuid() (src/state.rs:1415-1417) is ::emulator_telemetry::BatchId::random(), documented there as 'A fresh version-4 UUID' whose randomness is uuid::Uuid::new_v4() (vendor/emulator-telemetry asserts BatchId::random() != BatchId::random()). Nothing in that draw is derived from anything the service knows about the action - not its name, not its ARN, not its parameters. The two readers then echo the stored value unchanged (src/state/mitigation_actions.rs:238 in RegisteredMitigationAction::to_description, and src/ops/update_mitigation_action.rs:198), so ONE minting decision decides what all three operations report. Note that for the describe and update clusters the replay detail prints expected and actual with the SAME text, so this is NOT a visible textual difference in one recording's value: the comparator binds the action and still rejects the actionId the emulator carries for it, on every replay, while the printed value changes from run to run. Hypothesis: the recorded identity of a mitigation action is not reconstructible from a per-run random draw, so the reported actionId can never be matched back to the identity the recording holds for that action.

This issue bundles THREE causes and a fixer must close all THREE before submitting. The nearest open issue, 20261118T231406Z-iot-createmitigationaction-diverges-on-4-causes-rolearn-validation.md, covers roleArn validation, duplicate-create idempotency and the conflict/actionParams refusal shape on the same operation; this replay reports owned_by EMPTY for all three clusters below, so that issue does not gate them and none of its four causes is the id provenance.

Cause 1 of 3: CreateMitigationAction reports an actionId the recording cannot match. Fixture iot-mitigation-action-crud-lifecycle, step task#2.
  AWS: Smithy: CreateMitigationAction POST /mitigationactions/actions/{actionName} code 200; output CreateMitigationActionResponse{actionArn -> MitigationActionArn, actionId -> MitigationActionId}. MitigationActionId is a plain string shape with NO @length and NO @pattern, and CreateMitigationActionResponse$actionId is documented 'A unique identifier for the new mitigation action.' The model therefore constrains neither the id's internal form nor its provenance, so for the form this is recorded behavior only: the recording shows AWS reporting, for the action just created, the unique id AWS minted for that action - an identity later reads of the same action reproduce. No error is modelled into play here; the response is 200.
  Emulator: answers 200 with actionArn and actionId present and correctly shaped, but the replay verdict is mismatch, value_mismatch at $.actionId where the fixture asserts the member's shape only ('<shaped-not-asserted>'). The value reported is whatever state::fresh_uuid() drew in this run; the uuid quoted in the evidence below is a per-run fill-in, not the requirement.
  Why: actionId is the member a client records to correlate the action it just created with the action AWS holds - it is what StartAuditMitigationActionsTask reporting, DescribeAuditMitigationActionsTask execution records and a client's own inventory key off alongside actionArn. An id whose provenance is a random draw cannot be matched against the id AWS issues, so the correlation breaks for every mitigation action the account creates.
  Fix: the id mint on the CreateMitigationAction write path - the crate::state::fresh_uuid() call in that operation's handler, NOT the serializer (the output is built from the record that is persisted) and NOT the readers (they return the stored id unchanged). Whatever the mint becomes, it must be reproducible from what the service knows about the action and must stay byte-for-byte stable for the life of the record.
  Traces: iot-mitigation-action-crud-lifecycle#task#2 (plus iot-mitigation-action-device-certificate-lifecycle#task#2, iot-mitigation-action-device-certificate-type#task#1, iot-mitigation-action-duplicate-create-error#setup#1, iot-mitigation-action-enable-logging-type#task#1, iot-mitigation-action-lifecycle-and-errors#setup#3, iot-mitigation-action-lifecycle-thing-group#task#3, iot-mitigation-action-thing-group-lifecycle#task#2, iot-mitigation-action-thing-group-quarantine-lifecycle#task#3 - 9 steps in all).

Cause 2 of 3: DescribeMitigationAction reports that same unmatchable actionId back. Fixture iot-mitigation-action-crud-lifecycle, step task#3.
  AWS: Smithy: DescribeMitigationAction GET /mitigationactions/actions/{actionName} code 200; output actionName, actionType, actionArn, actionId -> MitigationActionId, roleArn, actionParams, creationDate, lastModifiedDate; errors InvalidRequestException(400), ResourceNotFoundException(404), ThrottlingException(400), InternalFailureException(500) - none in play, the step is 200 on both sides. As cause 1, the id's form is recorded behavior only: the recording shows the read reporting the SAME identity the create minted for that action.
  Emulator: answers 200 with every modelled member and the right action selected, and $.actionId still reports value_mismatch - with the expected and actual text printed identically, which is the signature of an identity the comparator cannot bind rather than of a differing literal.
  Why: the describe is how a client re-reads an action it did not create in the same process; the id it gets back is the one it would store or compare, and it cannot be matched against AWS's.
  Fix: same component as cause 1 (the create-path id mint). Nothing in the describe path is implicated - RegisteredMitigationAction::to_description clones the stored id, which is correct and must stay that way.
  Traces: iot-mitigation-action-crud-lifecycle#task#3 (plus #task#5 and iot-mitigation-action-device-certificate-lifecycle#task#3, iot-mitigation-action-device-certificate-type#task#2, iot-mitigation-action-duplicate-create-error#task#3, iot-mitigation-action-enable-logging-type#task#2, iot-mitigation-action-lifecycle-and-errors#task#4, iot-mitigation-action-lifecycle-thing-group#task#4, iot-mitigation-action-thing-group-lifecycle#task#3, iot-mitigation-action-thing-group-quarantine-lifecycle#task#4 - 10 steps in all).

Cause 3 of 3: UpdateMitigationAction reports that same unmatchable actionId. Fixture iot-mitigation-action-crud-lifecycle, step task#4.
  AWS: Smithy: UpdateMitigationAction PATCH /mitigationactions/actions/{actionName} code 200; output actionArn and actionId -> MitigationActionId; errors InvalidRequestException(400), ResourceNotFoundException(404), ThrottlingException(400), InternalFailureException(500) - none in play, 200 on both sides. UpdateMitigationAction$actionName is documented 'You cannot change the name by using UpdateMitigationAction', so an update does not re-identify the action: recorded behavior shows the update echoing the identity the create minted, unchanged.
  Emulator: answers 200 and correctly keeps the id across the update (apply_update leaves action_id untouched), so what it echoes is the create's random draw and $.actionId reports value_mismatch with the same printed text on both sides.
  Why: a client that updates an action and keys off the returned actionId gets the same unmatchable identity, so the divergence survives every mutation of the record rather than being confined to the create.
  Fix: same component as cause 1. The update path is otherwise correct - keeping the id stable across apply_update is the required behavior and must not be changed.
  Traces: iot-mitigation-action-crud-lifecycle#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: trace_set_id=simparity-wire-aws-v1, emulator_up=true ('/__admin/health answered 200 and the :32861 data listener accepts'), bootstrap_signal=false, fixtures_replayed=169, steps_total=1120, steps_passed=741, steps_mismatched=343, steps_errored=0, steps_blocked=36, divergence_count=68, score_pct=66.2. Of the 68 clusters, 64 report a non-empty owned_by (an open issue already carries their replay key) and none reports a stub_path; the three clusters filed here report owned_by EMPTY.

All three clusters: service=iot, verdict=mismatch, expected_status=200, actual_status=200, stub_path empty, owned_by empty.

Cause 1 - fixture iot-mitigation-action-crud-lifecycle, step task#2, operation CreateMitigationAction, affected_count=9:
  detail: $.actionId: value_mismatch (expected '<shaped-not-asserted>', got '8762c1fd-9abd-4044-88e0-0d3478f86c69')
  divergence_key: service=iot|CreateMitigationAction|status=200->200|keys=$.actionId
  affected: iot-mitigation-action-crud-lifecycle#task#2, iot-mitigation-action-device-certificate-lifecycle#task#2, iot-mitigation-action-device-certificate-type#task#1, iot-mitigation-action-duplicate-create-error#setup#1, iot-mitigation-action-enable-logging-type#task#1, iot-mitigation-action-lifecycle-and-errors#setup#3, iot-mitigation-action-lifecycle-thing-group#task#3, iot-mitigation-action-thing-group-lifecycle#task#2, iot-mitigation-action-thing-group-quarantine-lifecycle#task#3
  Expected: the unique identifier AWS mints for the created action, an identity later reads reproduce. Actual: the per-run uuid drawn by state::fresh_uuid(). The uuid quoted is a per-run fill-in, not the requirement.

Cause 2 - fixture iot-mitigation-action-crud-lifecycle, step task#3, operation DescribeMitigationAction, affected_count=10:
  detail: $.actionId: value_mismatch (expected '8762c1fd-9abd-4044-88e0-0d3478f86c69', got '8762c1fd-9abd-4044-88e0-0d3478f86c69')
  divergence_key: service=iot|DescribeMitigationAction|status=200->200|keys=$.actionId
  affected: iot-mitigation-action-crud-lifecycle#task#3, iot-mitigation-action-crud-lifecycle#task#5, iot-mitigation-action-device-certificate-lifecycle#task#3, iot-mitigation-action-device-certificate-type#task#2, iot-mitigation-action-duplicate-create-error#task#3, iot-mitigation-action-enable-logging-type#task#2, iot-mitigation-action-lifecycle-and-errors#task#4, iot-mitigation-action-lifecycle-thing-group#task#4, iot-mitigation-action-thing-group-lifecycle#task#3, iot-mitigation-action-thing-group-quarantine-lifecycle#task#4
  Note the expected and actual text are printed IDENTICALLY: the divergence is the identity's provenance, not a textual difference in one recording's value.

Cause 3 - fixture iot-mitigation-action-crud-lifecycle, step task#4, operation UpdateMitigationAction, affected_count=1:
  detail: $.actionId: value_mismatch (expected '8762c1fd-9abd-4044-88e0-0d3478f86c69', got '8762c1fd-9abd-4044-88e0-0d3478f86c69')
  divergence_key: service=iot|UpdateMitigationAction|status=200->200|keys=$.actionId
  affected: iot-mitigation-action-crud-lifecycle#task#4

Code under all three causes (same checkout the emulator is built from):
  services/service-com-amazonaws-iot-awsiotservice/src/ops/create_mitigation_action.rs:232-249 - action_arn = crate::state::mitigation_action_arn(&region, &input.action_name); RegisteredMitigationAction::new(input.action_name, crate::state::fresh_uuid(), action_arn, ...), and the response is built from that record.
  services/service-com-amazonaws-iot-awsiotservice/src/state.rs:1409-1417 - fresh_uuid(): ::emulator_telemetry::BatchId::random().as_str().to_string(), documented 'A fresh version-4 UUID in the canonical lowercase hyphenated form'.
  vendor/emulator-telemetry/src/types.rs:328-334, 1268-1270 - BatchId is a uuid newtype and the crate's own test asserts BatchId::random() != BatchId::random(), so the draw is per-call random.
  services/service-com-amazonaws-iot-awsiotservice/src/state/mitigation_actions.rs:154-159, 233-244 - RegisteredMitigationAction.action_id ('minted on create and kept by every later update of the record') and to_description() cloning it into DescribeMitigationActionOutput.actionId.
  services/service-com-amazonaws-iot-awsiotservice/src/ops/update_mitigation_action.rs:195-199 - apply_update leaves the id untouched and the response echoes action.action_id.

Smithy model consulted (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json):
  CreateMitigationAction POST /mitigationactions/actions/{actionName} code 200; output {actionArn -> MitigationActionArn, actionId -> MitigationActionId}; errors InternalFailureException(500), InvalidRequestException(400), LimitExceededException(410), ResourceAlreadyExistsException(409), ThrottlingException(400).
  DescribeMitigationAction GET /mitigationactions/actions/{actionName} code 200; output includes actionId -> MitigationActionId.
  UpdateMitigationAction PATCH /mitigationactions/actions/{actionName} code 200; output {actionArn, actionId}.
  com.amazonaws.iot#MitigationActionId: type string, NO @length and NO @pattern.
  CreateMitigationActionResponse$actionId doc: 'A unique identifier for the new mitigation action.'

## Steps to Reproduce
1. Clone the repo and boot the emulator built from this checkout.
2. Replay trace set simparity-wire-aws-v1 (replay_wire_corpus against the booted emulator).
3. Cause 1: replay fixture iot-mitigation-action-crud-lifecycle through step task#2 - POST /mitigationactions/actions/<name> with exactly one actionParams type set and a valid roleArn. The response is 200 with actionArn and actionId present, and $.actionId reports value_mismatch against the fixture's shape-only expectation.
4. Cause 2: continue to step task#3 - GET /mitigationactions/actions/<name>. 200, the right action is selected and every modelled member is present, and $.actionId diverges with expected and actual printed identically.
5. Cause 3: continue to step task#4 - PATCH /mitigationactions/actions/<name> with a changed roleArn or actionParams. 200, the id is correctly kept across the update, and $.actionId diverges the same way.
6. Confirm the provenance, not one recorded value, is at fault: replay the corpus twice. The actionId reported changes between runs (crate::state::fresh_uuid() draws a fresh random v4 uuid per create, src/state.rs:1415-1417) while all three clusters diverge identically every time.
7. After a fix, re-replay iot-mitigation-action-crud-lifecycle#task#2, #task#3 and #task#4; all three must pass, and the id a create reports must remain byte-for-byte identical to the one the later describe and update report for the same action.
