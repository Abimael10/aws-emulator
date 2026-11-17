Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena.calculation-execution-exceeds-timeout-marked-timed-out athena.calculation-execution-lifecycle-start-get-status-code-stop athena.calculation-ops-reject-unknown-calculation athena.create-presigned-notebook-url-requires-existing-notebook athena.e2e-notebook-session-calculation-teardown athena.e2e-query-lifecycle-dashboard-and-workgroup-teardown athena.get-resource-dashboard-returns-monitoring-url athena.list-executors-fails-after-session-termination athena.list-executors-reflects-session-executors athena.list-notebook-sessions-reflects-active-sessions athena.presigned-notebook-url-reports-ten-minute-token-lifetime athena.session-endpoint-unavailable-before-session-ready athena.session-full-lifecycle athena.session-idle-timeout-auto-terminates athena.session-idle-ttl-transitions-to-terminated athena.session-lifecycle-start-get-status-endpoint-terminate athena.session-ops-reject-unknown-session athena.start-calculation-execution-on-terminated-session-rejected athena.start-calculation-execution-terminated-session-not-found athena.telemetry-resource-dashboard-emitted-for-workgroup athena.telemetry-resource-dashboard-reflects-reservation-usage athena.terminate-session-idempotent-after-terminated athena.terminate-session-idempotent-during-creating
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena|GetSessionStatus|NotImplemented
Issue-Related: 20261104T163600Z-athena-getsession-stub.md
Issue-Commit: 57917fbf623074cc27ff2073937536de1df2e679

# Implement athena GetSessionStatus: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `57917fbf623074cc27ff2073937536de1df2e679`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSessionStatus

## Description
`GetSessionStatus` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-athena-amazonathena/src/ops/get_session_status.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

23 recorded unit(s) fail on their `GetSessionStatus` call, so each one unblocks only when `GetSessionStatus` works: `athena.calculation-execution-exceeds-timeout-marked-timed-out`, `athena.calculation-execution-lifecycle-start-get-status-code-stop`, `athena.calculation-ops-reject-unknown-calculation`, `athena.create-presigned-notebook-url-requires-existing-notebook`, `athena.e2e-notebook-session-calculation-teardown`, `athena.e2e-query-lifecycle-dashboard-and-workgroup-teardown`, `athena.get-resource-dashboard-returns-monitoring-url`, `athena.list-executors-fails-after-session-termination`, `athena.list-executors-reflects-session-executors`, `athena.list-notebook-sessions-reflects-active-sessions`, `athena.presigned-notebook-url-reports-ten-minute-token-lifetime`, `athena.session-endpoint-unavailable-before-session-ready`, `athena.session-full-lifecycle`, `athena.session-idle-timeout-auto-terminates`, `athena.session-idle-ttl-transitions-to-terminated`, `athena.session-lifecycle-start-get-status-endpoint-terminate`, `athena.session-ops-reject-unknown-session`, `athena.start-calculation-execution-on-terminated-session-rejected`, `athena.start-calculation-execution-terminated-session-not-found`, `athena.telemetry-resource-dashboard-emitted-for-workgroup` (+3 more).

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-athena-amazonathena/src/ops/get_session_status.rs`. Replaying the corpus, the emulator refused the `GetSessionStatus` call: File "context.py", line 123, in wrapper
    return func(*args, **kwargs)
           ^^^^^^^^^^^^^^^^^^^^^
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the GetSessionStatus operation: operation GetSessionStatus is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the athena emulator
Call GetSessionStatus and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
