Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty
Issue-Key: kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty|_await_data_plane#1|Timeout|time.sleep(2)
Issue-Commit: c208dffe0dd114dceb7c782194c5df9ad164c45a

# kafka__logs__s3: the MSK broker-log delivery check reaches no verdict — the whole per-check budget is spent before any destination is read

> _Note: this issue was identified against the code base at commit `c208dffe0dd114dceb7c782194c5df9ad164c45a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCluster

## Description
One root cause (Cause 1 of 1), grounded in the one failing kafka__logs__s3 check the replay report binds to no open issue.

Cause 1 of 1: the provisioned MSK cluster's log-delivery scenario never becomes observable — the run is killed at its budget with no assertion reached, so nothing about LoggingInfo.BrokerLogs delivery (the wired CloudWatch Logs group, the S3 prefix, the unwired group staying empty) is ever measured.

AWS: recorded behavior only for the timing (the Smithy model declares no latency). Against real AWS this scenario completes: CreateCluster(KafkaVersion, LoggingInfo.BrokerLogs{CloudWatchLogs.Enabled+LogGroup, S3.Enabled+Bucket/Prefix}) answers State 'CREATING', DescribeClusterV2 then reports State 'ACTIVE', GetBootstrapBrokers publishes a BootstrapBrokerString, the PLAINTEXT listener accepts a TCP connection and serves a Kafka metadata request, and the broker's INFO-level log lines subsequently appear in the wired log group and as non-empty objects under the S3 prefix while the unwired log group stays empty.
Emulator: the check is terminated by the harness with `Error: Check timed out after 60.0s` — the only detail the replay carries. No step-level assertion was reached, so no destination was read and no verdict was produced. Hypothesis (neither the model nor the recording states where it stalls): the run is consumed by one of its first observable waits — DescribeClusterV2 never reporting 'ACTIVE' (the poll's own ceiling is 2700 s at a 30 s interval), GetBootstrapBrokers never publishing a BootstrapBrokerString, or the advertised bootstrap endpoint never accepting a connection — all of which open kafka issues report for other checks (20261114T065925Z-kafka-msk-asynchronous-lifecycles-never-settle-channel-configuration-delete.md, 20261109T211434Z-kafka-msk-bootstrap-endpoints-are-published-as-real-aws.md, 20261117T031317Z-kafka-msk-native-sasl-iam-data-plane-five-more.md). An emulator is not real hardware: these transitions must settle in a client's working budget, not in MSK's 20-40 minute provisioning time.
Why: a client cannot verify that MSK honours the destinations it declared in LoggingInfo — the feature is entirely unobservable. Worse, a cluster that accepted a LoggingInfo and delivers nothing is indistinguishable from a cluster that is merely still coming up, which is exactly the confusion this check exists to remove.
Fix: the kafka (MSK) state machine and its data plane — the cluster lifecycle that must settle CREATING -> ACTIVE promptly, the GetBootstrapBrokers endpoint publication, and the broker-log delivery path that writes broker log lines to the configured CloudWatch Logs group and S3 prefix.
Related open issue, which does not carry this failure signature (it reports an earlier blocker of the same check — the Lambda python runtime missing the `select` module): 20261111T015651Z-kafka-logs-s3-the-msk-broker-log-delivery-7fe3ca.md.
Traces: kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: kafka__logs__s3 0/1 (1 failed).

kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty [mismatch]
Error: Check timed out after 60.0s

Expected (AWS): the run proceeds past `_await_active` (DescribeClusterV2 State 'ACTIVE'), `_bootstrap` (a non-empty BootstrapBrokerString), and a served Kafka metadata request, then asserts that broker log events land in the wired CloudWatch Logs group and as non-empty objects under the S3 prefix, while the unwired log group holds no events.
Actual (emulator): the check produces no assertion at all before the harness kills it; every one of those observations is missing. The 60 s figure is this run's harness budget and the cluster/bucket/log-group names are per-run values — the requirement is that the cluster reaches ACTIVE, publishes a bootstrap endpoint that answers, and delivers broker logs to the declared destinations within a client's budget.

## Steps to Reproduce
1. Boot the emulator from this checkout and run suite simparity-behavioral-aws-v1; observe kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty end as 'Check timed out'.
2. Minimal reproduction: CreateLogGroup (wired) and a second CreateLogGroup (unwired); CreateBucket; CreateCluster with two brokers, PLAINTEXT, and LoggingInfo.BrokerLogs {CloudWatchLogs: {Enabled: true, LogGroup: <wired>}, S3: {Enabled: true, Bucket: <bucket>, Prefix: <prefix>}}.
3. Poll DescribeClusterV2 for State: expect 'ACTIVE' within seconds on an emulator; time how long it takes.
4. GetBootstrapBrokers: expect a non-empty BootstrapBrokerString, then open a TCP connection to it and issue a Kafka metadata request.
5. Produce records, then FilterLogEvents on the wired group (expect broker log events), ListObjectsV2 under the S3 prefix (expect a non-empty object), and FilterLogEvents on the unwired group (expect none).
6. Re-replay the check; it must reach those assertions instead of being killed at its budget.
