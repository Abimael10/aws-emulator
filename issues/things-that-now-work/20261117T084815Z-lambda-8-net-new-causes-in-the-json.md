Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.log-lambda-report-json-format-platform-and-application-records-field-by-field lambda.log-lambda-report-system-log-level-filters-platform-records-independently lambda.log-tail-node-json-error-argument-adds-error-fields lambda.log-tail-node-json-object-argument-stays-structured lambda.log-tail-node-json-unhandled-error-nests-the-error-in-message lambda.log-tail-python-json-logging-exception-carries-error-fields lambda.log-tail-python-json-unhandled-error-record-uses-log-level-key lambda.log-tail-runtime-env-exposes-log-format-and-level
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.log-lambda-report-json-format-platform-and-application-records-field-by-field|run#18|AssertionError|assert len(inits) == 1 and recs.index(inits[0]) == 0, (
Issue-Key: lambda.log-lambda-report-system-log-level-filters-platform-records-independently|run#13|AssertionError|assert leaked == [], (
Issue-Key: lambda.log-tail-node-json-error-argument-adds-error-fields|run#12|AssertionError|assert r.get("errorType") == "RangeError", f"errorType must come from the first error: {r!r}"
Issue-Key: lambda.log-tail-node-json-object-argument-stays-structured|run#11|AssertionError|assert one.get("message") == {"marker": m, "data": 12.3, "flag": False, "list": [1, "two"]}, (
Issue-Key: lambda.log-tail-node-json-unhandled-error-nests-the-error-in-message|run#13|AssertionError|assert isinstance(msg, dict), f"the error must be a JSON object message, got {msg!r}"
Issue-Key: lambda.log-tail-python-json-logging-exception-carries-error-fields|run#15|AssertionError|assert isinstance(st, list) and len(st) == 1, f"stackTrace must be a one-frame list: {st!r}"
Issue-Key: lambda.log-tail-python-json-unhandled-error-record-uses-log-level-key|run#11|AssertionError|assert r.get("log_level") == "ERROR", f"the unhandled-error record's log_level {r!r}"
Issue-Key: lambda.log-tail-runtime-env-exposes-log-format-and-level|run#6|AssertionError|assert body1 == {"AWS_LAMBDA_LOG_FORMAT": None, "AWS_LAMBDA_LOG_LEVEL": None}, (
Issue-Commit: e5f037222dbb88715a655e98f6932c08deb84ecf

# lambda: 8 net-new causes in the JSON-LogFormat log pipeline — no platform.init* records, SystemLogLevel never filters platform records, Node/Python JSON error records malformed, AWS_LAMBDA_LOG_FORMAT set under Text

> _Note: this issue was identified against the code base at commit `e5f037222dbb88715a655e98f6932c08deb84ecf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Eight net-new root causes in the Lambda invoke log pipeline, all reached through LoggingConfig (Smithy lambda LoggingConfig{LogFormat: enum JSON|Text, ApplicationLogLevel: enum TRACE|DEBUG|INFO|WARN|ERROR|FATAL, SystemLogLevel: enum DEBUG|INFO|WARN, LogGroup}). They are adjacent to, but not covered by, open issue 20261107T091747Z-lambda-loggingconfig-is-ignored-by-the-invoke-log.md (LoggingConfig ignored outright) and 20261107T212447Z-lambda-logs-4-net-new-causes-in-cloudwatch-logs-delivery.md: the emulator now does emit JSON records, and these eight are defects in the records it emits. A fixer must close all eight before submitting.

Cause 1 of 8: the JSON log stream carries no platform.initStart record.
AWS: under LogFormat=JSON the function's log stream opens with one platform.initStart record (followed, for an on-demand cold start, by platform.initRuntimeDone and platform.initReport) before the first platform.start. Smithy: LoggingConfig.LogFormat enum JSON|Text; recorded behavior only for the per-record ordering.
Emulator: the stream's first event is platform.start and no platform.initStart record exists at all — 'AssertionError: one platform.initStart must open the stream, got []'.
Why: a client doing cold-start accounting from structured logs (init duration, init failures, init-vs-invoke attribution) finds no init records and concludes every environment was warm.
Fix: the in-process Lambda runtime's JSON log emitter — init-phase system records.
Traces: lambda.log-lambda-report-json-format-platform-and-application-records-field-by-field.

Cause 2 of 8: SystemLogLevel is not applied to platform.* records.
AWS: Smithy doc for LoggingConfig.SystemLogLevel — 'Lambda only sends system logs at the selected level of detail and lower, where DEBUG is the highest level and WARN is the lowest'. At SystemLogLevel=WARN a successful invocation's platform.start, platform.runtimeDone and platform.report are system records below WARN and are not delivered.
Emulator: with SystemLogLevel=WARN CloudWatch Logs holds platform.start, platform.runtimeDone and platform.report for every invocation — 'AssertionError: SystemLogLevel=WARN must deliver no platform.* record, CloudWatch holds [{... platform.start ...}, {... platform.runtimeDone ...}, {... platform.report ...}, ...]'. The request ids, timestamps and metric values in that list are per-run values, not part of the requirement; the requirement is that no platform.* record is delivered at WARN.
Why: the client's system-log level control is inert, so log volume, cost and downstream parsers that assume WARN-only system records all see records they explicitly filtered out.
Fix: system-record level gating in the runtime's log emitter (filter by SystemLogLevel before delivery).
Traces: lambda.log-lambda-report-system-log-level-filters-platform-records-independently.

Cause 3 of 8: a Node.js console.error carrying Error arguments produces a JSON record with no error fields.
AWS: under LogFormat=JSON a nodejs console.error(string, err1, err2) yields ONE ERROR record whose message formats all arguments and whose errorType, errorMessage and stackTrace are taken from the FIRST Error argument only (errorType is that error's constructor name, e.g. 'RangeError'; stackTrace is headed '<Type>: <message>' followed by frame lines). Documented at https://docs.aws.amazon.com/lambda/latest/dg/nodejs-logging.html; recorded behavior only for the per-field split.
Emulator: the record is {timestamp, level: 'ERROR', message, requestId} with no errorType, errorMessage or stackTrace member, and message is the concatenation of the marker and BOTH errors' type/message/stack text.
Why: a consumer that keys alerts or dashboards off errorType/errorMessage gets nothing, and the first error is no longer distinguishable from the second.
Fix: the Node.js runtime's JSON log formatter (Error-argument extraction).
Traces: lambda.log-tail-node-json-error-argument-adds-error-fields.

Cause 4 of 8: a single object argument to console.log is stringified instead of staying a JSON object.
AWS: under LogFormat=JSON a nodejs console.log with ONE object argument keeps that object as the record's `message` member, with JSON types intact (numbers stay numbers, booleans stay booleans, arrays stay arrays); the same object passed AFTER a string argument is util.format-formatted into a string message.
Emulator: the single-object call's message is the util.inspect text form — a string such as "{ marker: '<marker>', data: 12.3, flag: false, list: [ 1, 'two' ] }" (the marker and numbers shown are this run's values, not part of the requirement) — instead of the object.
Why: structured-logging consumers cannot query fields of the logged object; every field becomes opaque text with single-quoted, non-JSON syntax.
Fix: the Node.js runtime's JSON log formatter (single-object-argument path).
Traces: lambda.log-tail-node-json-object-argument-stays-structured.

Cause 5 of 8: an unhandled Node.js error is logged with a string message instead of a nested error object.
AWS: under LogFormat=JSON a nodejs handler that throws produces one ERROR record whose `message` is an OBJECT {errorType, errorMessage, stackTrace} agreeing field-for-field with the Invoke error payload, and no plain-text 'Invoke Error' line is written.
Emulator: `message` is the bare error message string (the thrown error's message text) and carries no errorType/errorMessage/stackTrace object — 'AssertionError: the error must be a JSON object message, got <message string>'.
Why: a client correlating the Invoke error payload with the log record cannot match them, and the stack trace of a failed invocation is lost from the structured record.
Fix: the Node.js runtime's unhandled-error reporting path under LogFormat=JSON.
Traces: lambda.log-tail-node-json-unhandled-error-nests-the-error-in-message.

Cause 6 of 8: Python logging.exception's JSON record puts the raw traceback text in stackTrace.
AWS: under LogFormat=JSON logging.exception() produces ONE ERROR record whose stackTrace is a list of FRAME strings (one entry per stack frame — here exactly one, the handler frame), with errorType and errorMessage describing the caught exception and a `location` naming the handler's file, function and line. Documented at https://docs.aws.amazon.com/lambda/latest/dg/python-logging.html.
Emulator: stackTrace is the output of traceback.format_exception verbatim — a three-entry list whose first entry is 'Traceback (most recent call last):\n' and whose last entry is the exception line ('<Type>: <value>\n'), with the frame text embedded in the middle entry.
Why: parsers that index frames, or that count stackTrace length to find the throwing frame, read a header line and the exception repetition as frames.
Fix: the Python runtime's JSON log formatter (exception serialization).
Traces: lambda.log-tail-python-json-logging-exception-carries-error-fields.

Cause 7 of 8: the unhandled-Python-exception JSON record uses the key `level` instead of `log_level`.
AWS: under LogFormat=JSON an unhandled Python exception is logged as one structured record {timestamp, log_level: 'ERROR', errorMessage, errorType, requestId, stackTrace} that agrees with the Invoke error payload, and no plain-text traceback is written. The member name log_level is the requirement; 'ERROR' is its fixed value for this record class.
Emulator: the record is emitted with 'level': 'ERROR' and no log_level member at all (the rest of the record — errorType, errorMessage, stackTrace, requestId — is present).
Why: a consumer filtering or indexing on log_level (the documented Python JSON key) sees no level on precisely the records that signal failure.
Fix: the Python runtime's JSON record serializer (key naming for the unhandled-error record).
Traces: lambda.log-tail-python-json-unhandled-error-record-uses-log-level-key.

Cause 8 of 8: AWS_LAMBDA_LOG_FORMAT is present in the execution environment of a function with no LoggingConfig.
AWS: with the default Text format neither AWS_LAMBDA_LOG_FORMAT nor AWS_LAMBDA_LOG_LEVEL is set in the execution environment; they appear (as 'JSON' and the configured application level, e.g. 'WARN') only after LoggingConfig switches the function to LogFormat=JSON. Documented at https://docs.aws.amazon.com/lambda/latest/dg/monitoring-cloudwatchlogs-log-level.html.
Emulator: a function created without LoggingConfig already reports AWS_LAMBDA_LOG_FORMAT='Text' (AWS_LAMBDA_LOG_LEVEL correctly absent) — 'AssertionError: under Text neither variable is set: {\'AWS_LAMBDA_LOG_FORMAT\': \'Text\', \'AWS_LAMBDA_LOG_LEVEL\': None}'.
Why: logging libraries (and handler code) that branch on the mere presence of AWS_LAMBDA_LOG_FORMAT take the structured-logging branch on a Text-format function, so the function's output format differs from AWS for identical code.
Fix: execution-environment variable seeding in the invoke path (only set these when LoggingConfig selects them).
Traces: lambda.log-tail-runtime-env-exposes-log-format-and-level.

## Evidence
Cause 1: expected one platform.initStart opening the stream; actual inits == [] and the first event is '{"time":"...","type":"platform.start","record":{"requestId":"...","version":"$LATEST"}}'.
Cause 2: expected no platform.* record at SystemLogLevel=WARN; actual platform.start, platform.runtimeDone and platform.report delivered for every invocation.
Cause 3: expected r['errorType'] == 'RangeError' (first Error argument); actual record {'timestamp','level':'ERROR','message','requestId'} with no errorType and both errors' stacks concatenated into message.
Cause 4: expected message == {'marker': <marker>, 'data': 12.3, 'flag': False, 'list': [1, 'two']}; actual message == "{ marker: '<marker>', data: 12.3, flag: false, list: [ 1, 'two' ] }" (a string).
Cause 5: expected message to be a dict {errorType, errorMessage, stackTrace}; actual message == '<thrown error message>' (a string).
Cause 6: expected stackTrace to be a one-frame list; actual ['Traceback (most recent call last):\n', '  File "index.py", line 7, in handler\n    raise KeyError(...)\n', "KeyError: '...'\n"].
Cause 7: expected r['log_level'] == 'ERROR'; actual record carries 'level': 'ERROR' and no log_level.
Cause 8: expected {'AWS_LAMBDA_LOG_FORMAT': None, 'AWS_LAMBDA_LOG_LEVEL': None} under the default Text format; actual {'AWS_LAMBDA_LOG_FORMAT': 'Text', 'AWS_LAMBDA_LOG_LEVEL': None}.

## Steps to Reproduce
Replay the eight listed behavioral checks against the emulator. In outline: (1) create a python3.12 function with LoggingConfig {LogFormat: JSON} and read its log group — the first record is platform.start, no platform.initStart; (2) create the same function with SystemLogLevel=WARN, invoke it and read the group — platform.start/runtimeDone/report are present; (3)-(5) create a nodejs22.x function with LoggingConfig {LogFormat: JSON} and invoke handlers that call console.error(string, new RangeError(...), new SyntaxError(...)), console.log({...}) and `throw new TypeError(...)`, then read the JSON records; (6)-(7) same with a python3.12 handler that calls logging.exception() inside an except block, and one that raises an unhandled ValueError; (8) create a python3.12 function with NO LoggingConfig and invoke a handler returning os.environ.get for AWS_LAMBDA_LOG_FORMAT and AWS_LAMBDA_LOG_LEVEL.
