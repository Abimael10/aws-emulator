Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iotdeviceadvisor.get-suite-run-report-not-ready-before-completion
Issue-Shape-Id: com.amazonaws.iotdeviceadvisor#IotSenateService
Issue-Key: iotdeviceadvisor|GetSuiteRunReport|ResourceNotFoundException
Issue-Commit: 53152f24252eb4c6b050f7ba33da4a3e7b513fd2

# iotdeviceadvisor GetSuiteRunReport answers 404 ResourceNotFoundException for an existing run of a non-qualification suite instead of 400 ValidationException

> _Note: this issue was identified against the code base at commit `53152f24252eb4c6b050f7ba33da4a3e7b513fd2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSuiteRunReport

## Description
REOPENS 20261024T134949Z-iotdeviceadvisor-getsuiterunreport-stub.md: its fix left the call refused.

Cause 1 of 1: GetSuiteRunReport conflates 'this suite is not intended for qualification' (and 'this run has no report yet') with 'no such suite run', and refuses with the wrong error class and status for a run that demonstrably exists.
AWS: Smithy: GetSuiteRunReport (GET /suiteDefinitions/{suiteDefinitionId}/suiteRuns/{suiteRunId}/report) declares ResourceNotFoundException (404), ValidationException (400) and InternalServerException (500); ResourceNotFoundException is the not-found class, ValidationException is 'Sends a validation exception'. Recorded behavior: asking for the qualification report of a run whose suite definition was created with intendedForQualification=false is refused with HTTP 400, code ValidationException, and a message stating that the suite (definition) is not intended for qualification — the phrase 'not intended for qualification' is the fixed part of that text; any ids it carries come from the request. The refusal is structural, so AWS returns the SAME ValidationException while the run is PENDING/RUNNING and after it has reached a terminal status — it is never 'not ready yet' and never 'not found'.
Emulator: the call is refused with HTTP 404, code ResourceNotFoundException, message 'Unable to find resource with suiteRunId: <suiteRunId>' — for the exact suiteDefinitionId/suiteRunId pair GetSuiteRun answers 200 for in the same check. The handler resolves the pair through store.qualification_report(..) and maps every None (not a qualification definition, or a run whose status is not in SUCCESSFUL_STATUSES) onto the not-found message keyed on suiteRunId.
Why: a client cannot tell a mistyped/aged-out run id from a suite that structurally has no qualification report. Retry-on-404 logic (waiting for a run id to become visible) loops forever, and nothing surfaces the real reason — the definition's intendedForQualification=false — so the operator is told the run does not exist when it does.
Fix: the refusal mapping in the GetSuiteRunReport handler, services/service-com-amazonaws-iotdeviceadvisor-iotsenateservice/src/ops/get_suite_run_report.rs (the `else` branch around line 168 that turns every qualification_report() miss into ResourceNotFoundException via crate::state::not_found_message("suiteRunId", ..)), together with the qualification_report predicate in src/state.rs: an unknown definition/run pair keeps 404 ResourceNotFoundException, while an existing run of a definition that is not intended for qualification must be 400 ValidationException whose message says the suite is not intended for qualification.
Traces: iotdeviceadvisor.get-suite-run-report-not-ready-before-completion.

## Evidence
Check iotdeviceadvisor.get-suite-run-report-not-ready-before-completion:
  AssertionError: before completion: expected ValidationException, got {'Message': 'Unable to find resource with suiteRunId: a1fd16c0-628d-4ea3-9560-85a33b91cf67', 'Code': 'ResourceNotFoundException'}
The same run id had just been read successfully: GetSuiteRun(suiteDefinitionId, suiteRunId) returned status PENDING/RUNNING immediately before the report call.
Expected: HTTP 400, code ValidationException, message containing the fixed phrase 'not intended for qualification' (variable parts, if any, taken from the request's suiteDefinitionId/suiteRunId), both while the run is PENDING/RUNNING and after it reaches a terminal status.
Actual: HTTP 404, code ResourceNotFoundException, message 'Unable to find resource with suiteRunId: <suiteRunId>' (fixed text 'Unable to find resource with suiteRunId: ', variable part the request's suiteRunId).

## Steps to Reproduce
1. Boot the emulator; create an IoT thing + certificate.
2. CreateSuiteDefinition with suiteDefinitionConfiguration.intendedForQualification=false and a rootGroup holding one MQTT_Connect test case.
3. StartSuiteRun for that definition with the thing/certificate as primaryDevice; note suiteRunId.
4. GetSuiteRun(suiteDefinitionId, suiteRunId) -> 200, status PENDING or RUNNING (the run exists).
5. GetSuiteRunReport(suiteDefinitionId, suiteRunId) -> observe HTTP 404 ResourceNotFoundException 'Unable to find resource with suiteRunId: <id>' instead of HTTP 400 ValidationException reporting that the suite is not intended for qualification.
6. Repeat step 5 after the run reaches a terminal status: AWS returns the identical ValidationException; the emulator again returns the 404.
