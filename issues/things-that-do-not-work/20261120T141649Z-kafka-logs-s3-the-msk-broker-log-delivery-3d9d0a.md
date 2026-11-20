Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty
Issue-Key: kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty|_await_data_plane#1|Timeout|last = _invoke(ctx, lam, fn, {"op": "kafka_ready", "bootstrap": bootstrap,
Issue-Commit: 46754bdb7f534c1c0384162513e101992564f05a

# kafka__logs__s3: the MSK broker-log delivery check still reaches no verdict — the whole per-check budget is spent before any destination is read

> _Note: this issue was identified against the code base at commit `46754bdb7f534c1c0384162513e101992564f05a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCluster

## Description
One root cause (Cause 1 of 1), the only failing check in this replay the report binds to no open issue.

Cause 1 of 1: a provisioned MSK cluster created with LoggingInfo.BrokerLogs never becomes observable, so nothing about broker-log delivery is ever measured — not the wired CloudWatch Logs group, not the S3 prefix, not the unwired group staying empty. Failing check: kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty.

AWS: Smithy (services/service-com-amazonaws-kafka-kafka) models LoggingInfo with required member BrokerLogs, and BrokerLogs with CloudWatchLogs{Enabled (required), LogGroup}, S3{Enabled (required), Bucket, Prefix} and Firehose — the model declares the destinations but says nothing about latency, so the timing is recorded behavior only. Against real AWS the scenario completes end to end: CreateCluster(KafkaVersion, NumberOfBrokerNodes a multiple of the client-subnet count, PLAINTEXT, LoggingInfo.BrokerLogs{CloudWatchLogs:{Enabled:true, LogGroup:<wired group>}, S3:{Enabled:true, Bucket:<bucket>, Prefix:<prefix>}}) answers State 'CREATING'; DescribeClusterV2 then reports State 'ACTIVE'; GetBootstrapBrokers publishes a non-empty BootstrapBrokerString; that advertised endpoint accepts a TCP connection on the PLAINTEXT listener and answers an ApiVersions/metadata request; and the broker's own log lines subsequently appear as events in the wired log group and as non-empty objects under the declared S3 prefix, while a log group the cluster was never wired to receives no events at all.
Emulator: the check is killed by the harness — the only detail the replay carries is `Error: Check timed out after 60.0s`. No step-level assertion is reached, so no destination is ever read and no verdict is produced: the emulator makes no observable progress through CreateCluster -> ACTIVE -> published bootstrap endpoint -> served data plane -> delivered broker logs inside a client's budget. Hypothesis (neither the model nor the recording states where it stalls): the run is consumed by one of the first observable waits — DescribeClusterV2 never reporting 'ACTIVE', GetBootstrapBrokers never publishing a BootstrapBrokerString, or the advertised bootstrap endpoint never accepting a connection — each of which open kafka issues report for other checks (20261114T065925Z-kafka-msk-asynchronous-lifecycles-never-settle-channel-configuration-delete.md, 20261114T205343Z-kafka-msk-data-plane-and-lifecycle-bootstrap-string-carries.md). An emulator is not real hardware: these transitions must settle in a client's working budget, not in MSK's 20-40 minute provisioning time.
Why: a client cannot verify that MSK honours the destinations it declared in LoggingInfo — the whole broker-logs feature is unobservable. A cluster that accepted a LoggingInfo and delivers nothing is indistinguishable from a cluster that is merely still coming up, which is exactly the confusion this check exists to remove, and any operator runbook or alarm built on broker logs reaching CloudWatch Logs or S3 silently never fires.
Fix: the kafka (MSK) cluster state machine and its data plane — the lifecycle that must settle CREATING -> ACTIVE promptly, the GetBootstrapBrokers endpoint publication and the listener it advertises, and the broker-log delivery path that writes broker log lines to the CloudWatch Logs group and the S3 bucket/prefix named in LoggingInfo.BrokerLogs (and nowhere else).
Related open issues, which the replay report does NOT bind to this failure (they do not carry this run's failure signature) and which therefore do not cover it: 20261118T131546Z-kafka-logs-s3-the-msk-broker-log-delivery--r2.md and 20261119T132710Z-kafka-logs-s3-the-msk-provisioned-cluster-broker.md. If the same root cause is addressed there, this check should be re-replayed to confirm it.
Traces: kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: kafka__logs__s3 0/1 (0%), 1 failed. Overall 181/238.

### kafka__logs__s3
[mismatch] kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty
Error: Check timed out after 60.0s

Expected (AWS): the run proceeds past the cluster becoming ACTIVE (DescribeClusterV2 State 'ACTIVE'), past a non-empty BootstrapBrokerString from GetBootstrapBrokers, and past a served Kafka request on the advertised PLAINTEXT listener, then asserts that broker log events land in the wired CloudWatch Logs group and as non-empty objects under the declared S3 prefix while the unwired log group holds no events.
Actual (emulator): no assertion is reached at all before the harness terminates the check; every one of those observations is missing — no cluster state read, no bootstrap endpoint, no delivered log event, no S3 object.

Per-run values are not requirements: the 60 s figure is this run's harness budget, and the cluster name, bucket name, log-group names and S3 prefix are generated per run. The requirement is that a cluster created with LoggingInfo reaches ACTIVE, publishes a bootstrap endpoint that answers, and delivers broker logs to exactly the declared destinations within a client's budget.

## Steps to Reproduce
1. Boot the emulator from this checkout and run suite simparity-behavioral-aws-v1; observe kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty end as 'Check timed out'.
2. Minimal reproduction: logs:CreateLogGroup twice (one 'wired', one 'unwired'); s3:CreateBucket.
3. kafka:CreateCluster with two brokers (one per client subnet), kafka.t3.small, KafkaVersion 3.9.x, PLAINTEXT/unauthenticated, and LoggingInfo.BrokerLogs {CloudWatchLogs:{Enabled:true, LogGroup:<wired>}, S3:{Enabled:true, Bucket:<bucket>, Prefix:<prefix>}}. Expect State 'CREATING'.
4. Poll DescribeClusterV2 for State: expect 'ACTIVE' within seconds on an emulator; time how long it takes.
5. GetBootstrapBrokers: expect a non-empty BootstrapBrokerString; open a TCP connection to it on 9092 and issue an ApiVersions/metadata request — expect a Kafka response.
6. Create a topic with 3 partitions, produce ~20 records, then: logs:FilterLogEvents on the wired group (expect broker log events), s3:ListObjectsV2 under the prefix (expect at least one non-empty object), logs:FilterLogEvents on the unwired group (expect no events).
7. Re-replay the check; it must reach those assertions instead of being killed at its budget.
