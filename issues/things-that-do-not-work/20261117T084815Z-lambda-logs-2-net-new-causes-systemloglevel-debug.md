Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__logs.log-cwl-system-debug-level-delivers-runtime-done-and-init-records lambda__logs.log-cwl-system-warn-level-still-delivers-a-timeout-report
Issue-Key: lambda__logs.log-cwl-system-debug-level-delivers-runtime-done-and-init-records|run#16|AssertionError|assert order == want, f"platform record sequence in {stream} must be {want!r}; got {order!r}"
Issue-Key: lambda__logs.log-cwl-system-warn-level-still-delivers-a-timeout-report|run#13|AssertionError|assert set(plat) == {"platform.runtimeDone", "platform.report"}, (
Issue-Commit: e5f037222dbb88715a655e98f6932c08deb84ecf

# lambda__logs: 2 net-new causes — SystemLogLevel=DEBUG delivers no platform.init* records to CloudWatch Logs, and SystemLogLevel=WARN still delivers platform.start

> _Note: this issue was identified against the code base at commit `e5f037222dbb88715a655e98f6932c08deb84ecf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Two net-new root causes in the delivery of Lambda system (platform.*) records to CloudWatch Logs under LoggingConfig.SystemLogLevel (Smithy lambda LoggingConfig.SystemLogLevel: enum DEBUG|INFO|WARN, doc: 'Lambda only sends system logs at the selected level of detail and lower, where DEBUG is the highest level and WARN is the lowest'). Open issue 20261109T143515Z-lambda-logs-a-timed-out-invocation-s-platform-report-record.md covers the earlier, different failure of the second check (the timeout's platform.report was missing entirely); that record is now delivered and the check fails on a new cause — platform.start leaking at WARN — so it is not covered there. A fixer must close both causes before submitting.

Cause 1 of 2: at SystemLogLevel=DEBUG the init-phase platform records are never delivered.
AWS: with LoggingConfig {LogFormat: JSON, ApplicationLogLevel: INFO, SystemLogLevel: DEBUG}, one stream holds, in order, platform.initStart, platform.initRuntimeDone (status success), platform.initReport (on-demand, with durationMs), platform.start, platform.runtimeDone (status success, the invocation's requestId, durationMs) and platform.report. DEBUG is the highest system level, so nothing is filtered. Documented at https://docs.aws.amazon.com/lambda/latest/dg/monitoring-cloudwatchlogs-log-level.html; recorded behavior only for the exact ordering.
Emulator: the stream holds only ['platform.start', 'platform.runtimeDone', 'platform.report'] — the three init records are absent (the stream name in the failure text is a per-run value).
Why: a client measuring cold-start cost or diagnosing init failures from CloudWatch Logs sees no init phase at all, even at the most verbose system level.
Fix: init-phase system-record emission in the in-process Lambda runtime's log-delivery path to CloudWatch Logs.
Traces: lambda__logs.log-cwl-system-debug-level-delivers-runtime-done-and-init-records.

Cause 2 of 2: at SystemLogLevel=WARN platform.start is still delivered.
AWS: with SystemLogLevel=WARN a successful invocation's platform records never reach CloudWatch Logs, and for a timed-out invocation only platform.runtimeDone and platform.report (both status 'timeout') are delivered — platform.start is below WARN and is dropped in both cases.
Emulator: the delivered platform record types are ['platform.report', 'platform.runtimeDone', 'platform.start'] where only {'platform.runtimeDone', 'platform.report'} are permitted, so platform.start is delivered despite the WARN filter.
Why: a consumer that relies on SystemLogLevel to suppress per-invocation system chatter still pays for and must parse platform.start records, and cannot use the presence of platform records as a timeout signal.
Fix: system-record level gating in the runtime's CloudWatch Logs delivery path (filter each platform record's level against SystemLogLevel).
Traces: lambda__logs.log-cwl-system-warn-level-still-delivers-a-timeout-report.

## Evidence
Cause 1: expected platform record sequence ['platform.initStart', 'platform.initRuntimeDone', 'platform.initReport', 'platform.start', 'platform.runtimeDone', 'platform.report']; actual ['platform.start', 'platform.runtimeDone', 'platform.report'].
Cause 2: expected the delivered platform record set to be exactly {'platform.runtimeDone', 'platform.report'} (the timeout's records only); actual ['platform.report', 'platform.runtimeDone', 'platform.start'].

## Steps to Reproduce
Replay lambda__logs.log-cwl-system-debug-level-delivers-runtime-done-and-init-records: create a python3.12 function with LoggingConfig {LogFormat: JSON, ApplicationLogLevel: INFO, SystemLogLevel: DEBUG}, invoke it once and read the platform.* records of /aws/lambda/<function> in order. Replay lambda__logs.log-cwl-system-warn-level-still-delivers-a-timeout-report: create the same function with SystemLogLevel: WARN and Timeout 1, invoke it once successfully and once with a sleep that exceeds the timeout, then read which platform.* record types reached the log group.
