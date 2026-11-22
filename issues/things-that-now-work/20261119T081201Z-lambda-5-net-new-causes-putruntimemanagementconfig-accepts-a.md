Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.deep-lambda-3-runtime-pin-foreign-runtime-arn-rejected lambda.deep-lambda-3-runtime-pin-manual-arn-survives-cold-start lambda.event-invoke-failure-not-surfaced lambda.log-lambda-report-log-level-with-text-format-rejected lambda.log-lambda-report-system-log-level-filters-platform-records-independently lambda.log-tail-python-json-extra-fields-merge-into-the-record
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.deep-lambda-3-runtime-pin-foreign-runtime-arn-rejected|_error#12|AssertionError|raise AssertionError(f"the call was expected to be refused but succeeded: {result!r}")
Issue-Key: lambda.deep-lambda-3-runtime-pin-manual-arn-survives-cold-start|run#12|AssertionError|assert cold_version.startswith("python:3.12."), (
Issue-Key: lambda.event-invoke-failure-not-surfaced|run#22|AssertionError|assert nf_msg == expected_nf_msg, (
Issue-Key: lambda.log-lambda-report-log-level-with-text-format-rejected|run#23|AssertionError|assert '"message": "app-info-t2"' in tail2, (
Issue-Key: lambda.log-lambda-report-system-log-level-filters-platform-records-independently|run#19|AssertionError|assert f'"message": "app-info-c"' in tail_c and "platform.runtimeDone" in _tail_types(tail_c), (
Issue-Key: lambda.log-tail-python-json-extra-fields-merge-into-the-record|run#11|AssertionError|assert r.get("a") == m, f"extra key 'a' must be top-level {m!r}: {r!r}"
Issue-Commit: 15eaad9fa74cff8f1dc14d7f5abd0b99014b7462

# lambda: 5 net-new causes — PutRuntimeManagementConfig accepts a foreign runtime's version ARN, INIT_START names the runtime identifier instead of the runtime build, the Event-invoke not-found message appends ':$LATEST', ApplicationLogLevel is applied to the invoke log tail, and a Python log record's extra= keys are dropped

> _Note: this issue was identified against the code base at commit `15eaad9fa74cff8f1dc14d7f5abd0b99014b7462`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRuntimeManagementConfig / Invoke

## Description
FIVE independent net-new root causes in the Lambda runtime-management surface, the asynchronous invoke's refusal wording, and the runtime's JSON log emission. ALL FIVE must be closed before this issue is submitted.

Cause 1 of 5: PutRuntimeManagementConfig accepts a runtime version ARN that belongs to a DIFFERENT runtime.
AWS: PutRuntimeManagementConfig with UpdateRuntimeOn='Manual' and a RuntimeVersionArn whose runtime build is not the function's configured Runtime is refused with HTTP 400 InvalidParameterValueException (Smithy: PutRuntimeManagementConfig -> InvalidParameterValueException, errorKind client, httpError 400; model services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json), message: fixed text 'Runtime version ARN (' + the RuntimeVersionArn echoed from the request + ') is not compatible with your function's runtime (' + the function's configured Runtime + ').' The refusal changes nothing: GetRuntimeManagementConfig still reports UpdateRuntimeOn 'Auto' with no RuntimeVersionArn, and the next cold environment still initialises on the function's own runtime version ARN. The function's OWN ARN is accepted for a Manual pin (positive control). Only the ARN and the runtime identifier are variable.
Emulator: the call is answered HTTP 200 and the pin is applied — the response carries UpdateRuntimeOn 'Manual' and RuntimeVersionArn set to the nodejs22.x function's runtime version ARN on a python3.12 function.
Why: the compatibility precondition is gone, so a caller can pin a function to a runtime version that cannot run it; the negative path a deployment pipeline relies on (refuse the bad pin, leave the function in Auto) never happens and the mis-pin is only discovered as a broken invoke.
Fix: PutRuntimeManagementConfig's validation — resolve the supplied RuntimeVersionArn's runtime build, compare it with the function's configured Runtime, and refuse an incompatible one with the InvalidParameterValueException above, persisting nothing.
Failing check ids: lambda.deep-lambda-3-runtime-pin-foreign-runtime-arn-rejected.

Cause 2 of 5: the INIT_START platform record names the runtime IDENTIFIER where AWS names the runtime BUILD version.
AWS: a cold environment's INIT_START record is 'INIT_START Runtime Version: <runtime-build>\tRuntime Version ARN: <arn>' where <runtime-build> is the concrete build of the managed runtime — for the python3.12 runtime it is 'python:3.12.<patch-build>', i.e. the family name, a colon, and the version — not the runtime identifier 'python3.12'. Recorded behavior only: log line formats are not in the Smithy model (Lambda developer guide, runtime management / runtime logs). The variable part is the build suffix.
Emulator: the record now exists (a previously filed defect) but reads 'Runtime Version: python3.12' — the runtime identifier, with no family:version form and no build.
Why: INIT_START is the only record of which runtime version an environment actually initialised on. A client auditing runtime-version rollout, or correlating the pinned RuntimeVersionArn with the build that ran, cannot parse the documented 'family:version' form and cannot distinguish two builds of the same runtime identifier.
Fix: the execution environment's INIT_START emitter — render the resolved runtime build in the '<family>:<major>.<minor>.<build>' form that corresponds to the runtime version ARN it also prints.
Failing check ids: lambda.deep-lambda-3-runtime-pin-manual-arn-survives-cold-start.

Cause 3 of 5: the asynchronous (InvocationType=Event) not-found refusal appends ':$LATEST' to the ARN.
AWS: Invoke with InvocationType='Event' on a function that does not exist is refused SYNCHRONOUSLY with HTTP 404 ResourceNotFoundException (Smithy: Invoke -> ResourceNotFoundException, errorKind client, httpError 404), message: fixed text 'Function not found: ' + the function ARN arn:<partition>:lambda:<region>:<account-id>:function:<name> and NOTHING else — no resolved qualifier on this path. Recorded behavior only for the wording; the ARN parts come from the request's FunctionName and the credential scope.
Emulator: same status and code, message 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>:$LATEST' — a ':$LATEST' suffix AWS does not emit for the Event path.
Why: a client that compares the refusal, or re-derives the resource it addressed, is told about a qualifier it never requested; the async and sync paths' messages are part of the error contract and differ.
Fix: the Invoke handler's not-found path — append the resolved qualifier only on the synchronous (RequestResponse/DryRun) path, and leave the Event path's message at the unqualified ARN; the shared 'Function not found: ' message helper is currently qualifier-suffixing unconditionally. NOTE: open issue 20261118T165433Z-lambda-9-causes-invoke-s-not-found-message.md, cause 1, requires exactly the opposite on the synchronous path (':$LATEST' present); the suffix must be conditioned on the invocation type rather than added or removed globally, or fixing one will re-break the other.
Failing check ids: lambda.event-invoke-failure-not-surfaced.

Cause 4 of 5: ApplicationLogLevel is applied to the invocation's own log output, so records below the level never appear in the invoke log tail.
AWS: with LoggingConfig.LogFormat='JSON', ApplicationLogLevel governs which application records Lambda DELIVERS to CloudWatch Logs; it does not suppress what the runtime emits for the invocation. An invoke with LogType='Tail' therefore returns, in X-Amz-Log-Result, every record the application logger produced for that request — including records below ApplicationLogLevel when the application's own logger level admits them (here the handler calls logging.getLogger().setLevel(INFO) / setLevel(DEBUG) itself) — alongside the platform.* records; only the CloudWatch stream is filtered. Recorded behavior only: no Smithy statement covers the tail's contents. Hypothesis: the emulator configures the runtime's root logger from ApplicationLogLevel and so drops the record at emission, which also removes it from the tail.
Emulator: at LogFormat=JSON with ApplicationLogLevel=WARN, the tail contains the platform.initRuntimeDone / initReport / start / runtimeDone / report records and the WARNING record, but no record whose message is the INFO one the handler logged — the INFO record is absent from the tail entirely. Platform records are NOT filtered by SystemLogLevel in the tail (the sibling assertion passes), so the filtering is specific to application records.
Why: the invoke tail is the caller's in-band view of what its handler actually logged. A client that reads the tail to diagnose one request sees a different, silently shortened log than AWS returns, and cannot distinguish 'the handler did not log it' from 'delivery filtered it' — which is the exact distinction ApplicationLogLevel exists to make.
Fix: the runtime's log pipeline — emit every record the application logger admits into the invocation's log tail, and apply ApplicationLogLevel only on the CloudWatch delivery leg (the same place SystemLogLevel is applied to platform.* records).
Failing check ids: lambda.log-lambda-report-log-level-with-text-format-rejected, lambda.log-lambda-report-system-log-level-filters-platform-records-independently.

Cause 5 of 5: a Python logging call's extra= dict is dropped from the JSON log record.
AWS: with LogFormat='JSON' the Python runtime's structured formatter writes one JSON object per record carrying the standard keys (timestamp, level, message, logger, requestId) and MERGES the keys of the logging call's extra= mapping in at the TOP LEVEL of that same object, preserving JSON types — a string stays a string, a list stays a list, a nested mapping stays an object, a boolean stays a boolean (Lambda developer guide, python-logging). Recorded behavior only. The key names and values come from the handler's call; the standard keys are the fixed part.
Emulator: the record is {'timestamp': ..., 'level': 'INFO', 'message': 'X-<marker>', 'logger': 'root', 'requestId': ...} — the extra= keys ('a', 'b', 'nested' in the check) are absent from the record altogether.
Why: structured logging is the whole point of LogFormat=JSON; a client that emits correlation ids, tenant ids or metric dimensions through extra= and then queries them with a CloudWatch Logs Insights field expression finds the fields missing, so its log-based alarms, filters and dashboards return nothing.
Fix: the Python runtime's JSON log formatter — merge the record's extra attributes (the LogRecord fields that are not standard logging attributes) into the emitted object as top-level keys, serialising each value by JSON type rather than stringifying it.
Failing check ids: lambda.log-tail-python-json-extra-fields-merge-into-the-record.

## Evidence
Behavioral suite replay against the emulator built from this checkout; service lambda. All six failing checks below are net-new (not owned by an open issue's replay key).

Cause 1 — [mismatch] lambda.deep-lambda-3-runtime-pin-foreign-runtime-arn-rejected:
  AssertionError: the call was expected to be refused but succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'UpdateRuntimeOn': 'Manual', 'FunctionArn': 'arn:aws:lambda:us-west-2:000000000000:function:sim2real-behav-rtpin-575ddf677e05', 'RuntimeVersionArn': 'arn:aws:lambda:us-west-2::runtime:7ad2204137a58e1f61496a08d32e0e9ecd3364677fc39ba47428b78ecbda3cdf'}
  The ARN offered was the nodejs22.x function's runtime version ARN; the target function's Runtime is python3.12. Expected instead: 400 InvalidParameterValueException, message "Runtime version ARN (<that arn>) is not compatible with your function's runtime (python3.12).", and GetRuntimeManagementConfig still 'Auto' with no ARN.

Cause 2 — [mismatch] lambda.deep-lambda-3-runtime-pin-manual-arn-survives-cold-start:
  AssertionError: a python3.12 function's INIT_START must name a python:3.12 build, got 'python3.12'
  (the value is read out of the cold environment's 'INIT_START Runtime Version: <v>\tRuntime Version ARN: <arn>' line).

Cause 3 — [mismatch] lambda.event-invoke-failure-not-surfaced:
  AssertionError: expected message 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-behav-absent-d7a6393120da' invoking an unknown function, got 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-behav-absent-d7a6393120da:$LATEST'
  The call was lam.invoke(FunctionName=<absent name>, InvocationType='Event', Payload=b'{}'); the code (ResourceNotFoundException) and status (404) already match. The account id, region and generated function name are this run's values.

Cause 4 — [mismatch] lambda.log-lambda-report-log-level-with-text-format-rejected:
  AssertionError: control: the handler must have EMITTED the INFO record (tail): '{"time":...,"type":"platform.initRuntimeDone",...}\n{..."platform.initReport"...}\n{..."platform.start"...}\n{"timestamp": "...", "level": "WARNING", "message": "app-warn-t2", "logger": "root", "requestId": "..."}\n{..."platform.runtimeDone"...}\n{..."platform.report"...}\n'
  — the WARNING record and all platform records are in the tail; the INFO record the same handler logged (root logger level INFO) is not. Config at that point: LogFormat=JSON, ApplicationLogLevel=WARN.
  and [mismatch] lambda.log-lambda-report-system-log-level-filters-platform-records-independently:
  AssertionError: control: the tail must show the INFO record and platform.runtimeDone WERE emitted: '<same shape: platform.* records plus only the WARNING application record>'
  Config: LogFormat=JSON, ApplicationLogLevel=WARN, SystemLogLevel=INFO; handler's root logger at DEBUG. platform.runtimeDone IS present, so only the application record is missing.

Cause 5 — [mismatch] lambda.log-tail-python-json-extra-fields-merge-into-the-record:
  AssertionError: extra key 'a' must be top-level 'm-7f4f64f0b8ab': {'timestamp': '2026-11-19T07:50:20.852Z', 'level': 'INFO', 'message': 'X-m-7f4f64f0b8ab', 'logger': 'root', 'requestId': 'cf52b6d0-61f3-4dae-8a53-6871e25b8c2a'}
  The handler logged logging.getLogger().info("X-" + m, extra={"a": m, "b": [3, "x"], "nested": {"k": True}}); AWS's record carries a (string), b (list) and nested (object) beside the standard keys.

(All ARNs, request ids, markers, timestamps, generated function names and the account id above are this run's values; the requirements are the statuses, codes, message texts, line layouts and key sets described per cause.)

## Steps to Reproduce
1. Clone the repo, build and boot the emulator from this branch, and replay the simparity-behavioral-aws-v1 suite filtered to service lambda.
2. Cause 1: create a python3.12 function and a nodejs22.x function; read the nodejs function's RuntimeVersionConfig.RuntimeVersionArn from GetFunctionConfiguration; call PutRuntimeManagementConfig(FunctionName=<python fn>, UpdateRuntimeOn='Manual', RuntimeVersionArn=<nodejs arn>). Observe HTTP 200 and the pin applied. Then confirm the function's OWN ARN is still accepted. Replay lambda.deep-lambda-3-runtime-pin-foreign-runtime-arn-rejected.
3. Cause 2: invoke a freshly created python3.12 function cold and read the INIT_START event from its log stream; observe 'Runtime Version: python3.12' instead of a 'python:3.12.<build>' value. Replay lambda.deep-lambda-3-runtime-pin-manual-arn-survives-cold-start.
4. Cause 3: Invoke a function name that does not exist with InvocationType='Event' and no Qualifier; read Error.Message and observe the ':$LATEST' suffix. Replay lambda.event-invoke-failure-not-surfaced.
5. Cause 4: create a python3.12 function whose handler calls logging.getLogger().setLevel(INFO) at module scope and logs one INFO and one WARNING record per invoke; set LoggingConfig LogFormat='JSON', ApplicationLogLevel='WARN'; Invoke with LogType='Tail' and base64-decode X-Amz-Log-Result. Observe the INFO record is absent from the tail (it must be present; only the CloudWatch stream is filtered). Repeat with ApplicationLogLevel='WARN', SystemLogLevel='INFO'. Replay lambda.log-lambda-report-log-level-with-text-format-rejected and lambda.log-lambda-report-system-log-level-filters-platform-records-independently.
6. Cause 5: with LogFormat='JSON', invoke a handler that calls logging.getLogger().info("X-"+m, extra={"a": m, "b": [3, "x"], "nested": {"k": True}}) and parse the record from the tail; observe only the standard keys. Replay lambda.log-tail-python-json-extra-fields-merge-into-the-record.
