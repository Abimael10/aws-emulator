Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot__logs.v2-logging-delivers-broker-entries-to-cloudwatch-and-disableall-stops-them
Issue-Key: iot__logs.v2-logging-delivers-broker-entries-to-cloudwatch-and-disableall-stops-them|run#47|Timeout|time.sleep(5)
Issue-Commit: 8ce2175ae74f721b468acbd371bce616782e6120

# iot__logs: IoT V2 logging still delivers no broker entries to AWSIotLogsV2, so the delivery / disableAllLogs / restore contrast never reaches a verdict

> _Note: this issue was identified against the code base at commit `8ce2175ae74f721b468acbd371bce616782e6120`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetV2LoggingOptions

## Description
Cause 1 of 1: with V2 logging configured and enabled, the emulator's IoT broker emits no log events into the service-owned CloudWatch Logs group AWSIotLogsV2, so the whole enable -> disableAllLogs -> restore contrast can never be observed and the check burns its entire budget without an answer. Failing check: iot__logs.v2-logging-delivers-broker-entries-to-cloudwatch-and-disableall-stops-them.

AWS: Smithy (com.amazonaws.iot#AWSIotService): SetV2LoggingOptions POST /v2LoggingOptions takes roleArn (AwsArn), defaultLogLevel (enum LogLevel), disableAllLogs (boolean, default false), eventConfigurations, returns 200 with an empty body and models only InternalException/InvalidRequestException/ServiceUnavailableException; GetV2LoggingOptions GET /v2LoggingOptions echoes roleArn, defaultLogLevel and disableAllLogs, or 404 NotConfiguredException when nothing is configured. The delivery side is recorded behavior only: on real AWS, once a role carrying arn:aws:iam::aws:policy/service-role/AWSIoTLogging is set with defaultLogLevel=DEBUG and disableAllLogs=false, an mTLS MQTT session against the account's iot:Data-ATS endpoint causes IoT to write JSON entries into the AWS-created log group AWSIotLogsV2 — a Connect entry whose clientId is the session's client id, status "Success" and principalId the authenticating certificate's certificateId, plus an entry whose topicName is the published topic. Setting disableAllLogs=true with the same role and level stops new entries for a fresh session, and setting it back to false makes an identical session log again. Those member names and values are the requirement; the particular client ids, certificate ids, topics and timestamps in any one run are just values from the request and state.

Emulator: no entry naming the session's client id ever appears in AWSIotLogsV2. The check makes no progress and is killed by the harness: 'Error: Check timed out after 60.0s'. Because the positive control (entries present while logging is enabled) never becomes true, neither the absence leg under disableAllLogs=true nor the restore leg is ever reached, so the emulator produces no verdict for any of the three legs.

Why: AWSIotLogsV2 is the only operator-visible record of broker-side connect/subscribe/publish activity and of authorization or connection failures. With that group permanently empty, every IoT troubleshooting workflow, every metric filter and every alarm built on those entries is dead, and a client polling FilterLogEvents on AWSIotLogsV2 waits until its own deadline instead of seeing the session it just made. The disableAllLogs switch is equally unobservable, so a consumer cannot tell a correctly suppressed fleet from a broker that simply never logs.

Fix: the IoT broker's V2 logging emission path — the component that applies the SetV2LoggingOptions role/defaultLogLevel/disableAllLogs configuration to broker connect/subscribe/publish events and delivers the resulting JSON entries into the AWSIotLogsV2 log group in CloudWatch Logs (creating that group on first delivery), plus the disableAllLogs gate on that path.

Related already-filed work, named so a fixer can join them rather than duplicate: open issue 20261117T115022Z-iot-logs-iot-v2-logging-delivers-no-broker-entries-to.md describes the same symptom but is keyed to a different replay signature (its Issue-Key pins a CONNACK wait frame inside _mqtt_connect) and the suite did not report this check as owned by it; fixed issue 20261120T100647Z-iot-logs-iot-v2-logging-never-delivers-broker.md claimed this delivery path, yet the check still reaches no verdict on this commit. This is a timeout with no response from the emulator, not a refusal, so it is reported as the still-open delivery gap rather than as a reopened refusal.

Traces: iot__logs.v2-logging-delivers-broker-entries-to-cloudwatch-and-disableall-stops-them (mismatch, 'Check timed out after 60.0s').

## Evidence
[mismatch] iot__logs.v2-logging-delivers-broker-entries-to-cloudwatch-and-disableall-stops-them — 'Error: Check timed out after 60.0s'.

Expected (AWS): after SetV2LoggingOptions(roleArn=<AWSIoTLogging role>, defaultLogLevel='DEBUG', disableAllLogs=False) and one mTLS MQTT session that connects and publishes once at QoS 1, FilterLogEvents on logGroupName='AWSIotLogsV2' with filterPattern '"<clientId>"' returns JSON entries including one with eventType='Connect', clientId=<the session's client id>, status='Success', principalId=<the authenticating certificate's certificateId>, and one with topicName=<the published topic> and the same clientId. After SetV2LoggingOptions(..., disableAllLogs=True) a fresh session yields no entries for its client id over the hold window; after setting disableAllLogs back to False, an identical session again yields a Connect entry with that client id and the same principalId.

Actual (emulator): no AWSIotLogsV2 entry for the session's client id ever appears; the check never passes its positive-control poll and is terminated by the harness at its 60 s budget with no verdict on the delivery, suppression or restore legs.

## Steps to Reproduce
1. Create an IAM role trusted by iot.amazonaws.com and attach arn:aws:iam::aws:policy/service-role/AWSIoTLogging.
2. Call iot SetV2LoggingOptions with roleArn=<that role>, defaultLogLevel='DEBUG', disableAllLogs=False; confirm GetV2LoggingOptions echoes roleArn, defaultLogLevel='DEBUG' and disableAllLogs=False.
3. Create an IoT policy allowing iot:Connect/Publish/Subscribe/Receive, call CreateKeysAndCertificate(setAsActive=True) and attach the policy to the certificate ARN.
4. Resolve the broker host with DescribeEndpoint(endpointType='iot:Data-ATS'), open an mTLS MQTT 3.1.1 session on port 8883 with a unique client id, publish once at QoS 1 to a unique topic, and disconnect.
5. Poll CloudWatch Logs FilterLogEvents(logGroupName='AWSIotLogsV2', startTime=<just before the session>, filterPattern='"<clientId>"') — expect a Connect entry (clientId, status 'Success', principalId = the certificate id) and an entry naming the published topic; observe that none ever arrives.
6. Call SetV2LoggingOptions(..., disableAllLogs=True), run a second session, and confirm no entries for its client id; then set disableAllLogs=False and confirm an identical third session logs again. Both legs are unreachable while step 5 never succeeds.
