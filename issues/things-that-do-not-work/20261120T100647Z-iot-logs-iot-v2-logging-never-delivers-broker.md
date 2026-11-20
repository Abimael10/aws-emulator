Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot__logs.v2-logging-delivers-broker-entries-to-cloudwatch-and-disableall-stops-them
Issue-Key: iot__logs.v2-logging-delivers-broker-entries-to-cloudwatch-and-disableall-stops-them|_poll_until#27|Timeout|time.sleep(interval_s)
Issue-Commit: 657b97543b8e608c75aab9edda765f25072992b5

# iot__logs: IoT V2 logging never delivers broker log entries to CloudWatch Logs, so neither delivery nor disableAllLogs suppression can be observed

> _Note: this issue was identified against the code base at commit `657b97543b8e608c75aab9edda765f25072992b5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetV2LoggingOptions

## Description
1 cause.

Cause 1 of 1: with V2 logging configured and enabled, the broker emits no log entries into CloudWatch Logs.
AWS: SetV2LoggingOptions(roleArn=<logging role>, defaultLogLevel='DEBUG', disableAllLogs=false) makes the IoT broker deliver per-session log entries to the service-owned CloudWatch Logs group 'AWSIotLogsV2'; the entries carry the session's client id, the certificate principal and the topic involved, and appear within the delivery window. With disableAllLogs=true a fresh MQTT session produces no new entries, and restoring disableAllLogs=false makes delivery resume. Recorded behavior only for the entry contents (the entry shape is not in the Smithy model); GetV2LoggingOptions does read back roleArn and disableAllLogs as set.
Emulator: GetV2LoggingOptions reflects the configuration, but the check then waits for entries in 'AWSIotLogsV2' and the replay records 'Check timed out after 60.0s' — no broker entry is ever delivered, so the positive leg never completes and the disableAllLogs suppression leg is never reached.
Why: a client that relies on IoT V2 logging to observe or audit broker activity (connects, publishes, authorization outcomes) gets an empty log group while the control plane reports logging as enabled — so an operator debugging a device, or a control that asserts logging is on, is misled, and the disableAllLogs switch has no observable effect either way.
Fix: the IoT V2 logging emitter in the iot data-plane/broker component — the broker must emit session/topic log events through the configured logging role into the AWSIotLogsV2 CloudWatch Logs group, and must honour defaultLogLevel and disableAllLogs when deciding whether to emit.
Traces: iot__logs.v2-logging-delivers-broker-entries-to-cloudwatch-and-disableall-stops-them.

## Evidence
Expected: after SetV2LoggingOptions(roleArn=<role>, defaultLogLevel='DEBUG', disableAllLogs=false) is confirmed by GetV2LoggingOptions, an MQTT session's log entries (client id, certificate principal, topic) appear in the CloudWatch Logs group 'AWSIotLogsV2' within the delivery window; then with disableAllLogs=true a fresh session adds no entries, and restoring the flag logs again.
Actual: the configuration reads back correctly but no log entry is ever delivered to 'AWSIotLogsV2'; the check blocks on that wait and the replay reports 'Check timed out after 60.0s'.

## Steps to Reproduce
1. Build and boot the emulator.
2. Create an IAM role trusted by iot.amazonaws.com and attach arn:aws:iam::aws:policy/service-role/AWSIoTLogging.
3. Call iot.SetV2LoggingOptions(roleArn=<role arn>, defaultLogLevel='DEBUG', disableAllLogs=False) and confirm with GetV2LoggingOptions.
4. Open an mTLS MQTT session at the published iot:Data-ATS endpoint, subscribe and publish on a topic.
5. Call logs.FilterLogEvents / GetLogEvents on log group 'AWSIotLogsV2' looking for entries naming the session's client id, certificate principal and topic — none appear.
6. (Unreachable today) Set disableAllLogs=True, open a fresh session, and confirm no new entries; restore and confirm delivery resumes.
Replay: iot__logs.v2-logging-delivers-broker-entries-to-cloudwatch-and-disableall-stops-them
