Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.log-lambda-report-json-format-platform-and-application-records-field-by-field
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.log-lambda-report-json-format-platform-and-application-records-field-by-field|run#18|AssertionError|assert len(inits) == 1 and recs.index(inits[0]) == 0, (
Issue-Commit: f3fcdc08bdb09b2741d5bd13ee7a493b1992f806

# LogFormat=JSON log stream carries no platform.initStart record; the stream opens with platform.start

> _Note: this issue was identified against the code base at commit `f3fcdc08bdb09b2741d5bd13ee7a493b1992f806`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
One net-new root cause.

Cause 1 of 1: with LoggingConfig.LogFormat=JSON the emulator never emits the platform.initStart telemetry record for a new execution environment. Failing check: lambda.log-lambda-report-json-format-platform-and-application-records-field-by-field.

AWS: the model carries the switch but not the record contents — Smithy: LoggingConfig$LogFormat (enum Text | JSON), LoggingConfig$ApplicationLogLevel, LoggingConfig$SystemLogLevel on CreateFunction/UpdateFunctionConfiguration — so the record set is recorded behavior only. The recording shows that under LogFormat=JSON a cold execution environment's CloudWatch stream is OPENED by exactly one JSON event of type "platform.initStart", emitted before that environment's first "platform.start", and its "record" object carries: functionName (the function's name), functionVersion "$LATEST", initializationType "on-demand", phase "init", instanceId equal to the environment id in the log stream's own name, instanceMaxMemory equal to the configured MemorySize expressed in bytes, plus a top-level ISO-8601 "time". All of those values come from the function's configuration and the environment, not from fixed text. The subsequent per-request records are platform.start, the application records, and platform.report (initDurationMs present on the cold report only), in that order.

Emulator: the stream for the cold request contains NO platform.initStart event at all. The first event in the stream is the platform.start record, e.g. {"time":...,"type":"platform.start","record":{"requestId":...,"functionArn":...,"version":"$LATEST"}}. The check's filter for type=="platform.initStart" returns an empty list, so the replay stops at the first assertion and the remaining JSON-format assertions (report metrics, application-record placement, initDurationMs cold-only, print() staying plain text, no leaked text-format START/END/REPORT lines) were never reached — they are not asserted to be correct by this report.

Why: a consumer of JSON platform telemetry (a log subscription, an observability pipeline, or an extension keyed on the init phase) gets no init signal for a cold start at all: it cannot tell a cold environment from a warm one, cannot attribute initialization, and loses the only records that publish instanceId and instanceMaxMemory. Anything that joins per-request records to their environment through initStart.instanceId silently joins nothing.

Fix: the Lambda invoke path's structured-log emitter — the JSON platform-record writer used when LogFormat=JSON — which must write one platform.initStart record, with the fields above, at the head of each new execution environment's log stream, before that environment's first platform.start.

Traces: lambda.log-lambda-report-json-format-platform-and-application-records-field-by-field. No open issue names platform.initStart or LogFormat (searched both).

## Evidence
Expected (real AWS): first event in the cold environment's /aws/lambda/<fn> stream is a JSON object with type "platform.initStart" and record {functionName:<fn>, functionVersion:"$LATEST", initializationType:"on-demand", phase:"init", instanceId:<the stream's environment id>, instanceMaxMemory:<MemorySize MiB in bytes>} plus an ISO-8601 time; platform.start for the first requestId follows it.

Actual (emulator): no platform.initStart event exists in the stream; the first event is the platform.start record.

Replay assertion: AssertionError: one platform.initStart must open the stream, got [] (first event '{"time":"...","type":"platform.start","record":{"requestId":"...","functionArn":"arn:aws:lambda:<region>:<account>:function:<fn>","version":"$LATEST"}}\n')

## Steps to Reproduce
1. CreateFunction: Runtime python3.12, MemorySize 320, Handler index.handler, a handler that print()s a marker and logs it at INFO, LoggingConfig={LogFormat: JSON, ApplicationLogLevel: INFO, SystemLogLevel: INFO}.
2. Invoke it twice with the same payload (cold then warm) and wait for platform.report records for both request ids in /aws/lambda/<fn>.
3. Read the log stream that holds the cold request's platform.start and list the events of type "platform.initStart": AWS has exactly one, at index 0 of the stream; the emulator has none.
