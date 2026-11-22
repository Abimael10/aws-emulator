Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.log-lambda-report-system-log-level-filters-platform-records-independently
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.log-lambda-report-system-log-level-filters-platform-records-independently|run#24|AssertionError|assert c_types == ["platform.start", "platform.report"], (
Issue-Commit: c715480170741a84c2fa772a3d743f7ed03fb9c6

# lambda: at LoggingConfig.SystemLogLevel=INFO the DEBUG-level platform.runtimeDone record is still delivered to CloudWatch Logs

> _Note: this issue was identified against the code base at commit `c715480170741a84c2fa772a3d743f7ed03fb9c6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
1 net-new root cause in the per-level gating of Lambda system (platform.*) records delivered to CloudWatch Logs.

Open issue 20261117T084815Z-lambda-logs-2-net-new-causes-systemloglevel-debug.md is about the same gating path but does NOT cover this cause: its two causes are (1) SystemLogLevel=DEBUG delivering no platform.init* records and (2) SystemLogLevel=WARN leaking platform.start. Neither is the INFO level, and neither concerns platform.runtimeDone. In this check the WARN leg passes (no platform record leaks at WARN) and the failure is on the INFO leg, so a fix for that issue would not close this one.

Cause 1 of 1: SystemLogLevel=INFO does not drop the DEBUG-level platform.runtimeDone record.
AWS: Smithy lambda LoggingConfig.SystemLogLevel is an enum DEBUG|INFO|WARN whose documentation states 'Set this property to filter the system logs for your function that Lambda sends to CloudWatch. Lambda only sends system logs at the selected level of detail and lower, where DEBUG is the highest level and WARN is the lowest.' platform.runtimeDone is a DEBUG-level system record; platform.start and platform.report are INFO-level. Therefore, with LoggingConfig {LogFormat: JSON, ApplicationLogLevel: WARN, SystemLogLevel: INFO}, a successful invocation delivers to the function's log group exactly the platform records platform.start then platform.report for that invocation's requestId, and no platform.runtimeDone — even though the invocation's Tail (LogType=Tail) proves platform.runtimeDone WAS emitted by the execution environment. The gating is per-record-level and independent of ApplicationLogLevel. Documented at https://docs.aws.amazon.com/lambda/latest/dg/monitoring-cloudwatchlogs-log-level.html; recorded behavior only for the exact ordering of the two delivered records.
Emulator: the platform.* records delivered for the invocation's requestId are ['platform.start', 'platform.runtimeDone', 'platform.report'] — platform.runtimeDone, a DEBUG record, is delivered at SystemLogLevel=INFO. No error code or non-2xx status is involved: Invoke succeeds and the divergence is purely in which system records reach CloudWatch Logs. (The requestId in the failure text is a per-run value, not part of the requirement.)
Why: a consumer that sets SystemLogLevel=INFO to pay for and parse only the start/report envelope still receives runtimeDone records, so log volume and cost are higher than AWS's, and any consumer that treats the presence of runtimeDone as evidence the function was configured at DEBUG draws the wrong conclusion about the function's logging configuration.
Fix: the system-record level gating in the in-process Lambda runtime's CloudWatch Logs delivery path — each platform.* record must carry its own level (initStart/initRuntimeDone/initReport/runtimeDone = DEBUG, start/report = INFO) and be dropped when that level is more verbose than the function's LoggingConfig.SystemLogLevel. The gate must be applied independently of ApplicationLogLevel, which governs only application records.
Traces: lambda.log-lambda-report-system-log-level-filters-platform-records-independently.

## Evidence
Failing check lambda.log-lambda-report-system-log-level-filters-platform-records-independently, leg 2 (the levels swapped: ApplicationLogLevel=WARN, SystemLogLevel=INFO):

AssertionError: at SystemLogLevel=INFO exactly platform.start then platform.report must be delivered for <requestId> (runtimeDone is DEBUG), got ['platform.start', 'platform.runtimeDone', 'platform.report']

Expected (AWS): the platform.* record types delivered to /aws/lambda/<function> for the invocation's requestId are, in order, ['platform.start', 'platform.report'].
Actual (emulator): ['platform.start', 'platform.runtimeDone', 'platform.report'].

The check's own control assertion passes first: the Invoke Tail for that same invocation contains platform.runtimeDone, so the record is genuinely emitted by the execution environment and the divergence is in the delivery-side level filter, not in record generation. Leg 1 of the same check (SystemLogLevel=WARN, ApplicationLogLevel=INFO) passes — no platform record and no plain-text START/END/REPORT line leaks at WARN — so the defect is specific to the INFO threshold.

## Steps to Reproduce
1. Create a python3.12 function (Handler index.handler, MemorySize 128, Timeout 30) whose handler logs one INFO and one WARN record through the root logger, with LoggingConfig {LogFormat: 'JSON', ApplicationLogLevel: 'INFO', SystemLogLevel: 'WARN'}.
2. Invoke it twice with LogType='Tail'; confirm the tail shows platform.start and platform.report were emitted and that the log group holds no platform.* record and no plain-text START/END/REPORT line (this leg passes today).
3. UpdateFunctionConfiguration with LoggingConfig {LogFormat: 'JSON', ApplicationLogLevel: 'WARN', SystemLogLevel: 'INFO'} and wait for the function to become ready.
4. Invoke once more with LogType='Tail'. Confirm the tail contains both the application INFO record and platform.runtimeDone (so both were emitted).
5. Poll /aws/lambda/<function> until a platform.report record for that invocation's requestId arrives, then collect the ordered list of platform.* record types for that requestId.
6. AWS yields ['platform.start', 'platform.report']; the emulator yields ['platform.start', 'platform.runtimeDone', 'platform.report'] — the DEBUG-level runtimeDone record is not filtered at SystemLogLevel=INFO.
