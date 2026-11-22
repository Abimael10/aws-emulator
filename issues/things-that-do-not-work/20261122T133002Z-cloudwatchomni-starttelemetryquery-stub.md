Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatchomni.completed-query-results-stay-readable-after-stop cloudwatchomni.deleted-view-is-no-longer-a-queryable-table cloudwatchomni.query-over-the-default-dataset-needs-a-bounded-time-range cloudwatchomni.query-without-a-from-clause-is-rejected cloudwatchomni.redefining-a-view-changes-what-a-query-over-it-binds cloudwatchomni.stopped-session-leaves-the-list-and-refuses-new-queries cloudwatchomni.view-without-a-datastore-is-stored-but-fails-to-bind-in-a-query
Issue-Shape-Id: com.amazonaws.cloudwatchomni#CloudWatchOmniFrontend
Issue-Key: cloudwatchomni|StartTelemetryQuery|NotImplemented
Issue-Key: cloudwatchomni.query-over-the-default-dataset-needs-a-bounded-time-range|run#4|AssertionError|assert code == "ValidationException", f"expected ValidationException, got {code}: {message!r}"
Issue-Key: cloudwatchomni.query-without-a-from-clause-is-rejected|run#4|AssertionError|assert code == "ValidationException", f"expected ValidationException, got {code}: {message!r}"
Issue-Key: cloudwatchomni.redefining-a-view-changes-what-a-query-over-it-binds|run#8|AssertionError|assert (code, message) == ("ValidationException", (
Issue-Key: cloudwatchomni.view-without-a-datastore-is-stored-but-fails-to-bind-in-a-query|run#7|AssertionError|assert code == "ValidationException", f"expected ValidationException, got {code}: {message!r}"
Issue-Commit: 27c72d454eafc35a12aac6fa1d715ada36555a31

# Implement cloudwatchomni StartTelemetryQuery: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `27c72d454eafc35a12aac6fa1d715ada36555a31`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartTelemetryQuery

## Description
`StartTelemetryQuery` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-cloudwatchomni-cloudwatchomnifrontend/src/ops/start_telemetry_query.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

7 recorded unit(s) fail on their `StartTelemetryQuery` call, so each one unblocks only when `StartTelemetryQuery` works: `cloudwatchomni.completed-query-results-stay-readable-after-stop`, `cloudwatchomni.deleted-view-is-no-longer-a-queryable-table`, `cloudwatchomni.query-over-the-default-dataset-needs-a-bounded-time-range`, `cloudwatchomni.query-without-a-from-clause-is-rejected`, `cloudwatchomni.redefining-a-view-changes-what-a-query-over-it-binds`, `cloudwatchomni.stopped-session-leaves-the-list-and-refuses-new-queries`, `cloudwatchomni.view-without-a-datastore-is-stored-but-fails-to-bind-in-a-query`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-cloudwatchomni-cloudwatchomnifrontend/src/ops/start_telemetry_query.rs`. Replaying the corpus, the emulator refused the `StartTelemetryQuery` call: File "context.py", line 123, in wrapper
    return func(*args, **kwargs)
           ^^^^^^^^^^^^^^^^^^^^^
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the StartTelemetryQuery operation: operation StartTelemetryQuery is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the cloudwatchomni emulator
Call StartTelemetryQuery and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
