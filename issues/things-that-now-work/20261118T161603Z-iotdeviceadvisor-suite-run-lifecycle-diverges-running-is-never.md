Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iotdeviceadvisor.start-suite-run-creates-pending-or-running-run iotdeviceadvisor.stop-suite-run-initiates-stopping-transition iotdeviceadvisor.stop-suite-run-repeat-after-completion-rejected iotdeviceadvisor.mqtt-connect-native-protocol-round-trip iotdeviceadvisor.suite-run-without-device-fails-on-connect-timeout iotdeviceadvisor.start-suite-run-conflict-when-already-running-rejected iotdeviceadvisor.concurrent-start-suite-run-only-one-wins
Issue-Shape-Id: com.amazonaws.iotdeviceadvisor#IotSenateService
Issue-Key: iotdeviceadvisor.start-suite-run-creates-pending-or-running-run|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: iotdeviceadvisor.stop-suite-run-initiates-stopping-transition|_poll_until#6|Timeout|time.sleep(interval_s)
Issue-Key: iotdeviceadvisor.stop-suite-run-repeat-after-completion-rejected|_poll_until#6|Timeout|time.sleep(interval_s)
Issue-Key: iotdeviceadvisor.mqtt-connect-native-protocol-round-trip|_poll_until#42|Timeout|time.sleep(interval_s)
Issue-Key: iotdeviceadvisor.suite-run-without-device-fails-on-connect-timeout|run#7|AssertionError|assert final["status"] == "FAIL", f"expected suite run status FAIL, got {final['status']!r}"
Issue-Key: iotdeviceadvisor.start-suite-run-conflict-when-already-running-rejected|run#10|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: iotdeviceadvisor.concurrent-start-suite-run-only-one-wins|run#12|AssertionError|assert all(msg == expected for _, _code, msg in errs), f"expected message {expected!r} for every loser, got {errs}"
Issue-Commit: 53152f24252eb4c6b050f7ba33da4a3e7b513fd2

# iotdeviceadvisor suite-run lifecycle diverges: RUNNING is never observable, a run with no device ends ERROR instead of FAIL with test results, and the concurrent-start ConflictException message is hand-rolled

> _Note: this issue was identified against the code base at commit `53152f24252eb4c6b050f7ba33da4a3e7b513fd2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartSuiteRun

## Description
Three net-new root causes in the iotdeviceadvisor suite-run lifecycle and its concurrent-start refusal. All three are grounded in failing behavioral checks of this service. (Unrelated open issues 20261113T071450Z-iotdeviceadvisor-getendpoint-vends-the-emulator-s-own-loopback.md and 20261117T175423Z-iotdeviceadvisor-getendpoint-returns-a-host-the-emulator-does-not.md cover GetEndpoint's host only; neither covers anything below.)

Cause 1 of 3: a suite run leaves RUNNING after ~500 ms, so no client can ever observe the RUNNING window.
AWS: Smithy model (services/service-com-amazonaws-iotdeviceadvisor-iotsenateservice/smithy-model/com-amazonaws-iotdeviceadvisor-iotsenateservice.json) declares SuiteRunStatus = PASS|FAIL|CANCELED|PENDING|RUNNING|STOPPING|STOPPED|PASS_WITH_WARNINGS|ERROR and GetSuiteRun returns status plus startTime/endTime/testResult. Recorded behavior only for the durations: a run started with a root group containing one MQTT_Connect case configured with EXECUTION_TIMEOUT=120 (or 300) seconds is reported PENDING briefly, then RUNNING, and stays RUNNING for the test case's execution window — on the order of the configured EXECUTION_TIMEOUT — which is the only window in which Device Advisor accepts the device's MQTT CONNECT. StopSuiteRun is then accepted and moves the run promptly to STOPPING/STOPPED.
Emulator: the run's whole lifecycle is a 500 ms clock. PENDING advances to RUNNING one STATUS_TRANSITION after creation and RUNNING advances to ERROR one more STATUS_TRANSITION later, materialized lazily on every read, so a client that polls GetSuiteRun at any realistic interval (3 s and 5 s in these checks) sees PENDING and then a terminal status and never once sees RUNNING. Every check that waits for RUNNING before acting therefore never proceeds and is cut off by the suite budget ('Check timed out after 60.0s'); the configured EXECUTION_TIMEOUT of the test case is ignored entirely.
Why: a client cannot do anything a real Device Advisor run requires. There is no interval in which the device under test may connect, StopSuiteRun can never be exercised against a live run, and any poll-for-RUNNING driver (the AWS console flow and the documented workflow alike) hangs forever against the emulator while succeeding against AWS.
Fix: the suite-run status transition schedule in the service state — SuiteRun::materialize_due and the STATUS_TRANSITION span in services/service-com-amazonaws-iotdeviceadvisor-iotsenateservice/src/state.rs, together with the next_transition_at the StartSuiteRun handler seeds in src/ops/start_suite_run.rs. The RUNNING dwell must derive from the run's configured test-case EXECUTION_TIMEOUT rather than from one fixed sub-second span, so the RUNNING status is observable for the run's execution window.
Traces: iotdeviceadvisor.start-suite-run-creates-pending-or-running-run, iotdeviceadvisor.stop-suite-run-initiates-stopping-transition, iotdeviceadvisor.stop-suite-run-repeat-after-completion-rejected, iotdeviceadvisor.mqtt-connect-native-protocol-round-trip.

Cause 2 of 3: a run whose device never connects ends ERROR with a no-test-runner errorReason instead of FAIL carrying test results.
AWS: recorded behavior only (the model fixes the vocabulary, not the outcome): a run of a non-qualification suite whose single MQTT_Connect case is never answered by a device times out at the case's EXECUTION_TIMEOUT and reaches the terminal status FAIL, with endTime set and testResult.groups[0].tests[0].status == 'FAIL'. ERROR is reserved for a run the service could not execute at all, not for a test case that ran and failed to see a connection.
Emulator: the run reaches terminal status 'ERROR' and carries errorReason = 'Device Advisor test execution is not available in this emulator: no device test runner is attached, so the suite run could not be executed against the primary device.' (crate::state::NO_TEST_RUNNER_REASON); no testResult groups are produced. Replay: "AssertionError: expected suite run status FAIL, got 'ERROR'".
Why: a client that branches on the terminal status mis-classifies every negative outcome as a service fault rather than as a failed qualification/test run, and anything reading testResult.groups to report per-case outcomes finds nothing to read.
Fix: the terminal-outcome decision for a suite run in services/service-com-amazonaws-iotdeviceadvisor-iotsenateservice/src/state.rs (the 'RUNNING' arm of SuiteRun::materialize_due and the SUCCESSFUL_STATUSES/NO_TEST_RUNNER_REASON policy around it) plus the TestResult projection the GetSuiteRun serializer reads: a run whose test case was never satisfied must end FAIL with an endTime and a per-group/per-case FAIL result.
Traces: iotdeviceadvisor.suite-run-without-device-fails-on-connect-timeout.

Cause 3 of 3: the ConflictException refusing a concurrent StartSuiteRun uses the emulator's own wording instead of the service's message.
AWS: Smithy: StartSuiteRun declares ConflictException (HTTP 400) with member message (Message, length 1..2048). Recorded behavior: starting a second non-parallel run for a suite that already has a run in progress is refused with code ConflictException and the message, which is entirely FIXED text with no variable parts, 'Suite already running with this account. Please stop suite run or wait until the suite finishes running. You can also use device-level endpoint to run suites in parallel.' The same message is returned to every loser of N racing StartSuiteRun calls (exactly one wins).
Emulator: the code and status are right, but the message is 'Suite run <suiteRunId> is still in progress for suite definition <suiteDefinitionId>' — a template of the emulator's own invention (the blocking run id and the definition id substituted from state/request). Replay: "expected message 'Suite already running with this account. ...', got 'Suite run 702307fd-3ca8-4507-b124-f405bfc9dbcb is still in progress for suite definition 921e9903-93f2-4223-bba0-6510a79ec717'" and the same for all five losers in the concurrency check.
Why: the message is part of the contract; a client or test that matches on AWS's text (or surfaces it to an operator, including the hint to use the device-level endpoint for parallel runs) gets neither the expected string nor the documented remediation.
Fix: the refusal message in the StartSuiteRun handler's concurrency branch, services/service-com-amazonaws-iotdeviceadvisor-iotsenateservice/src/ops/start_suite_run.rs (the ConflictException built where a blocking in-progress run is found).
Traces: iotdeviceadvisor.start-suite-run-conflict-when-already-running-rejected, iotdeviceadvisor.concurrent-start-suite-run-only-one-wins.

## Evidence
Cause 1 — iotdeviceadvisor.start-suite-run-creates-pending-or-running-run: 'Error: Check timed out after 60.0s' while polling GetSuiteRun for status == 'RUNNING' (timeout_s=90). Same timeout for stop-suite-run-initiates-stopping-transition (poll RUNNING, 60 s), stop-suite-run-repeat-after-completion-rejected (poll RUNNING, 60 s) and mqtt-connect-native-protocol-round-trip (poll RUNNING, 90 s). Source: src/state.rs declares `pub const STATUS_TRANSITION: Duration = Duration::from_millis(500);` and SuiteRun::materialize_due moves PENDING->RUNNING then RUNNING->ERROR one 500 ms span apart, materialized on read.
Expected: GetSuiteRun reports RUNNING for the run's execution window (EXECUTION_TIMEOUT = 120 s / 300 s in these checks), so a poll at 3-5 s intervals observes it; StopSuiteRun on that live run then yields STOPPING and, after cleanup, STOPPED.
Actual: RUNNING exists for at most 500 ms of wall clock and is never observed; the polls never satisfy their predicate.

Cause 2 — iotdeviceadvisor.suite-run-without-device-fails-on-connect-timeout:
  AssertionError: expected suite run status FAIL, got 'ERROR'
Expected: terminal status 'FAIL', endTime set, testResult.groups[0].tests[0].status == 'FAIL'.
Actual: terminal status 'ERROR' with errorReason NO_TEST_RUNNER_REASON and no testResult groups.

Cause 3 — iotdeviceadvisor.start-suite-run-conflict-when-already-running-rejected:
  AssertionError: expected message 'Suite already running with this account. Please stop suite run or wait until the suite finishes running. You can also use device-level endpoint to run suites in parallel.', got 'Suite run 702307fd-3ca8-4507-b124-f405bfc9dbcb is still in progress for suite definition 921e9903-93f2-4223-bba0-6510a79ec717'
iotdeviceadvisor.concurrent-start-suite-run-only-one-wins: exactly one racer won and the other five were refused HTTP 400 ConflictException, but every loser carried the same emulator-invented message 'Suite run <run id> is still in progress for suite definition <definition id>'.
Expected: HTTP 400, code ConflictException, message exactly the fixed AWS text above (no request- or state-derived parts).
Actual: HTTP 400, code ConflictException, message templated from the blocking run id and the requested definition id.

## Steps to Reproduce
1. Boot the emulator and create an IoT thing + certificate, then CreateSuiteDefinition with intendedForQualification=false and a rootGroup containing one group with one MQTT_Connect (version 0.0.0) test case configured EXECUTION_TIMEOUT=120.
2. StartSuiteRun(suiteDefinitionId=<id>, suiteRunConfiguration={primaryDevice: {thingArn, certificateArn}}).
3. Poll GetSuiteRun every 3 s for status == 'RUNNING' -> never satisfied (Cause 1); the run is already in a terminal status one second after the start.
4. Keep polling to a terminal status -> status is 'ERROR' with errorReason 'Device Advisor test execution is not available in this emulator: ...' and no testResult.groups, where AWS reports 'FAIL' with a FAIL test-case result (Cause 2).
5. Start a second run: StartSuiteRun for the same definition/device while the first is still in progress (or fire 6 StartSuiteRun calls concurrently) -> HTTP 400 ConflictException whose message is 'Suite run <id> is still in progress for suite definition <id>' instead of AWS's fixed 'Suite already running with this account. Please stop suite run or wait until the suite finishes running. You can also use device-level endpoint to run suites in parallel.' (Cause 3).
