Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot__logs.v2-logging-delivers-broker-entries-to-cloudwatch-and-disableall-stops-them
Issue-Key: iot__logs.v2-logging-delivers-broker-entries-to-cloudwatch-and-disableall-stops-them|_mqtt_connect#26|Timeout|if state["connected"].wait(timeout=20):
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# IoT v2 logging delivers no broker entries to CloudWatch Logs, so the enable/DISABLED contrast never reaches a verdict

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetV2LoggingOptions

## Description
Cause 1 of 1: the v2 logging path produces no log events for broker activity.
AWS: recorded behavior only — with v2 logging configured (a role carrying arn:aws:iam::aws:policy/service-role/AWSIoTLogging and a logging level set through SetV2LoggingOptions), IoT writes broker entries for client connect/subscribe/publish activity into the service-owned log group AWSIotLogsV2; setting the level to DISABLED stops new entries while the already-delivered ones stay readable.
Emulator: the check makes no progress and is killed by the harness at its 60 s budget — no entry ever appears in AWSIotLogsV2, so neither the delivery half nor the DISABLED half becomes observable.
Why: AWSIotLogsV2 is the only place an operator can see broker-side connection and publish failures; with it permanently empty, every IoT troubleshooting workflow and every alarm built on those entries is dead, and a consumer polling the group waits forever.
Fix: the IoT broker's v2 logging emission path (the log-level/role configuration applied to connect/subscribe/publish events) and its delivery into the AWSIotLogsV2 log group.
Failing check: iot__logs.v2-logging-delivers-broker-entries-to-cloudwatch-and-disableall-stops-them

## Evidence
[mismatch] iot__logs.v2-logging-delivers-broker-entries-to-cloudwatch-and-disableall-stops-them — 'Error: Check timed out after 60.0s'. Expected: broker entries delivered to the AWS-owned log group AWSIotLogsV2 within the delivery deadline, then no new entries after the logging level is set to DISABLED. Actual: no verdict; nothing delivered.

## Steps to Reproduce
1. Attach the AWSIoTLogging service-role policy to a role and call SetV2LoggingOptions with that role and a logging level.
2. Connect an MQTT client and publish/subscribe, then poll the AWSIotLogsV2 log group for broker entries.
3. Set the logging level to DISABLED and confirm no further entries arrive while the existing ones remain.
