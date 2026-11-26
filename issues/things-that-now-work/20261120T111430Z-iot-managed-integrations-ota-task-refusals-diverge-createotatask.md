Issue-Source: trace-sweep-divergence
Issue-Trace: iot-managed-integrations-ota-task-error-paths
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-managed-integrations-ota-task-error-paths
Issue-Shape-Id: com.amazonaws.iotmanagedintegrations#IotManagedIntegrations
Issue-Key: iot-managed-integrations|CreateOtaTask|4xx->4xx|$.Message
Issue-Key: iot-managed-integrations|CreateOtaTask|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: iot-managed-integrations|CreateOtaTask|4xx->4xx|status
Issue-Key: iot-managed-integrations|GetOtaTask|4xx->4xx|$.Message
Issue-Commit: 30d927f99e8e54f81e8ebf641df74b5be52ff769

# iot-managed-integrations OTA task refusals diverge: CreateOtaTask never validates S3Url, GetOtaTask 404 re-worded

> _Note: this issue was identified against the code base at commit `30d927f99e8e54f81e8ebf641df74b5be52ff769`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateOtaTask

## Description
This issue bundles 2 net-new causes, both in the iot-managed-integrations OTA-task error paths: the service decides WHICH refusal an invalid OTA-task request gets, and HOW the not-found refusal is worded, differently from AWS. Both causes must be fixed and re-replayed before this issue is closed.

Cause 1 of 2 — CreateOtaTask resolves the Target managed thing before validating S3Url, so an unusable S3Url is answered 404 ResourceNotFoundException instead of 400 ValidationException (fixture iot-managed-integrations-ota-task-error-paths, step task#2).
AWS: Smithy (services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/smithy-model/com-amazonaws-iotmanagedintegrations-iotmanagedintegrations.json) models CreateOtaTask as POST /ota-tasks -> 201, with S3Url (@required, @length 1..1000) and Target (list, not required), and declares ValidationException (HTTP 400, "A validation error occurred when performing the API request") alongside ResourceNotFoundException (HTTP 404). The model does not state the ordering or the message text, so for those this is recorded behavior only: the recording shows a CreateOtaTask whose S3Url does not name a usable S3 object is refused HTTP 400 with x-amzn-errortype 'ValidationException' and Message 'S3 error occurred due to invalid request: ' — i.e. the refusal is reached on the REQUEST's S3Url, before any Target/ManagedThing lookup, even though the same request's Target names a managed thing this account does not hold. In that message 'S3 error occurred due to invalid request: ' is the FIXED text; what follows the colon is the variable part, the underlying S3 error detail for the offending S3Url (empty in the recording, so no recorded value there is a requirement).
Emulator: the request is answered HTTP 404 with x-amzn-errortype 'ResourceNotFoundException' and Message 'ManagedThing record does not exist' — the handler validates the modelled enums, then walks Target and returns crate::state::managed_thing_reference_not_found(entry) for the first unresolvable entry, and never inspects S3Url beyond the model's @length/@required walk. So S3Url is never validated at all, and the Target lookup pre-empts the refusal AWS makes.
Why: a client that branches on the error class cannot tell a bad artifact URL from a missing device. It sees a 404 ResourceNotFoundException (a retriable-looking "create the thing first" signal) where AWS returns a 400 ValidationException naming an S3 problem, so a caller retries or goes and provisions a managed thing instead of fixing its S3Url, and a correct S3Url is never required at all — a task created with an unusable artifact URL succeeds here and fails on AWS.
Fix: the CreateOtaTask handler — add S3Url admission validation and run it BEFORE the Target/ManagedThing and TaskConfigurationId resolution, returning ValidationException with AWS's 'S3 error occurred due to invalid request: <s3 detail>' wording.
Traces: iot-managed-integrations-ota-task-error-paths#task#2. Code citation: services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/src/ops/create_ota_task.rs:299-312 (the Target resolution loop that fires first) — S3Url is only moved into state at line 348.

Cause 2 of 2 — GetOtaTask's ResourceNotFoundException message uses the emulator's own not-found template instead of AWS's OTA-task sentence (fixture iot-managed-integrations-ota-task-error-paths, step task#3).
AWS: Smithy models GetOtaTask as GET /ota-tasks/{Identifier} -> 200, @readonly, with ResourceNotFoundException (HTTP 404, "The specified resource does not exist"). The model does not state the message text, so for the text this is recorded behavior only: the recording shows GetOtaTask on an Identifier the account holds no OTA task under answers HTTP 404, error type 'ResourceNotFoundException', Message 'Ota task with ID <Identifier> does not exists.' The fixed text is 'Ota task with ID ' + ' does not exists.' (AWS's own capitalisation 'Ota' and its ungrammatical 'does not exists.' included); the only variable part is the Identifier taken verbatim from the request path. The identifier in the recording is a per-run fixture value and is not itself a requirement.
Emulator: the status (404) and error class (ResourceNotFoundException) are right, but the Message is 'OTA task not found for id: <Identifier>' — the shared read-path helper crate::state::ota_task_not_found_on_read, whose template ('OTA task not found for id: ') is emulator-authored and matches neither AWS's prefix nor its trailing sentence.
Why: a client or test that matches AWS's documented/returned sentence (or scrapes the id out of it) never matches; error-message assertions and user-facing surfacing of the refusal show text AWS never emits. This is the same class of defect as the already-open issue 20261029T224743Z-iot-managed-integrations-deleteotataskconfiguration-404-reuses-the-get-not-found-message.md, which covers DeleteOtaTaskConfiguration's message only and does NOT cover GetOtaTask.
Fix: the GetOtaTask not-found branch / the shared state helper it calls — the OTA-task read 404 must carry AWS's 'Ota task with ID <Identifier> does not exists.' wording.
Traces: iot-managed-integrations-ota-task-error-paths#task#3. Code citation: services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/src/ops/get_ota_task.rs:186-194 (crate::state::ota_task_not_found_on_read).

## Evidence
Cause 1 — fixture iot-managed-integrations-ota-task-error-paths, step task#2, operation CreateOtaTask:
  status: expected 400, got 404
  $.Message: expected 'S3 error occurred due to invalid request: ', got 'ManagedThing record does not exist'
  $headers.x-amzn-errortype: expected 'ValidationException', got 'ResourceNotFoundException'
Expected (AWS, recorded): HTTP 400, x-amzn-errortype ValidationException, Message 'S3 error occurred due to invalid request: <s3 detail>' (fixed text 'S3 error occurred due to invalid request: '; the remainder is the S3 error detail for the request's S3Url).
Actual (emulator): HTTP 404, x-amzn-errortype ResourceNotFoundException, Message 'ManagedThing record does not exist' — the Target lookup refusal, reached before S3Url is ever examined.

Cause 2 — fixture iot-managed-integrations-ota-task-error-paths, step task#3, operation GetOtaTask:
  $.Message: expected 'Ota task with ID nonexistentotatask0000001 does not exists.', got 'OTA task not found for id: nonexistentotatask0000001'
Status (404) and error class (ResourceNotFoundException) match; only the sentence diverges. Fixed text per AWS: 'Ota task with ID ' ... ' does not exists.'; variable part: the Identifier from the request path (the fixture's 'nonexistentotatask0000001' is that per-request value, not part of the template).

## Steps to Reproduce
1. git_clone, then boot the emulator and point an iot-managed-integrations client at it.
2. Cause 1: call CreateOtaTask with OtaType='ONE_TIME', an S3Url that does not name a usable S3 object, and Target=['<a ManagedThingId this region holds no managed thing under>']. Observe HTTP 404 ResourceNotFoundException 'ManagedThing record does not exist' instead of HTTP 400 ValidationException 'S3 error occurred due to invalid request: '. Repeat with a Target the account DOES hold to confirm the bad S3Url is accepted and the task is created (AWS refuses it).
3. Cause 2: call GetOtaTask(Identifier='nonexistentotatask0000001') — an id no OTA task exists under in this region. Observe HTTP 404 ResourceNotFoundException with Message 'OTA task not found for id: nonexistentotatask0000001' instead of AWS's 'Ota task with ID nonexistentotatask0000001 does not exists.'
4. Re-replay fixture iot-managed-integrations-ota-task-error-paths steps task#2 and task#3 to confirm both causes are closed.
