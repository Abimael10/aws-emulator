Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty
Issue-Key: kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty|_invoke#1|Timeout|return out
Issue-Commit: c439b0c06ad4c0c74cea84caf475018e0421a0f9

# kafka__logs__s3: the MSK provisioned-cluster broker-log flow never completes — nothing in the CreateCluster -> ACTIVE -> Kafka data plane -> LoggingInfo delivery chain finishes inside the replay budget

> _Note: this issue was identified against the code base at commit `c439b0c06ad4c0c74cea84caf475018e0421a0f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCluster

## Description
Cause 1 of 1: the MSK broker-log delivery flow never reaches any of its observable milestones.
Failing check: kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty

AWS: recorded behavior only — the Smithy model says nothing about delivery, only about the control-plane shapes. The recorded run walks a chain of observable milestones: CreateCluster (kafka) with LoggingInfo.BrokerLogs naming an enabled CloudWatchLogs.LogGroup, an enabled S3 destination (Bucket + Prefix) and Firehose disabled is accepted with State CREATING or ACTIVE; DescribeClusterV2 then reports that exact LoggingInfo back; the cluster reaches State=ACTIVE; GetBootstrapBrokers publishes a BootstrapBrokerString of well-formed host:port endpoints; that PLAINTEXT listener accepts TCP from inside the cluster's own VPC and then answers a hand-framed Kafka ApiVersions request with error_code 0 advertising api keys 0 (Produce), 3 (Metadata), 19 (CreateTopics) and 32 (DescribeConfigs); CreateTopics and Produce succeed; recognisable Apache Kafka broker log lines (a log4j level token together with a kafka logger name) then appear in the wired CloudWatch log group and as a NON-EMPTY object under the configured S3 prefix, while a second log group deliberately left out of LoggingInfo receives no events and no streams.
Emulator: the replay reports only "Error: Check timed out after 60.0s" — the flow produced no milestone at all within the budget, so no assertion in the chain was ever evaluated. Hypothesis: the emulator either never advances the provisioned cluster to ACTIVE, never publishes a bootstrap endpoint that accepts a connection, or serves no Kafka wire protocol behind it; the suite output does not record which, because the first unmet milestone simply polls. Nothing here depends on a real account or a live clock — the emulator provisions nothing physical, so a run that cannot reach ACTIVE and a served broker in 60 s is a stall in the emulator, not provisioning latency.
Why: a client building on MSK gets a cluster it can create and describe but never talk to, and broker logs that never arrive at the CloudWatch log group or the S3 prefix its LoggingInfo names. Every downstream consumer — a log-based alarm, an S3 archive, a consumer application — silently sees nothing, with no error to key on.
Fix: the kafka service's cluster lifecycle (CreateCluster/DescribeClusterV2 state progression to ACTIVE and GetBootstrapBrokers publishing a reachable endpoint), the broker data plane behind that endpoint, and the BrokerLogs delivery path that writes broker log content to the configured CloudWatch log group and S3 prefix and to nothing else.
Traces: kafka__logs__s3.msk-broker-logs-reach-cloudwatch-logs-and-s3-while-an-unwired-log-group-stays-empty.

## Evidence
expected: CreateCluster accepted (State CREATING|ACTIVE) -> DescribeClusterV2 echoes LoggingInfo.BrokerLogs (CloudWatchLogs.Enabled=true with the given LogGroup, S3.Bucket/Prefix as given, Firehose.Enabled=false) -> State=ACTIVE -> GetBootstrapBrokers publishes BootstrapBrokerString -> the PLAINTEXT listener accepts TCP and answers ApiVersions with error_code 0 and api keys {0,3,19,32} -> CreateTopics + Produce succeed -> Kafka broker log lines appear in the wired log group and in a non-empty object under the S3 prefix, while the unwired log group stays empty (no events, no streams).
actual:   Error: Check timed out after 60.0s — no milestone reached, no assertion evaluated.

## Steps to Reproduce
1. Stand up a VPC with subnets and a security group admitting tcp/9092 among its members, and an in-VPC Lambda executor (used only as the in-VPC client).
2. Create two CloudWatch log groups ('wired' and 'unwired') and an S3 bucket.
3. kafka CreateCluster: a provisioned cluster with 2 brokers in those subnets, EncryptionInfo.EncryptionInTransit.ClientBroker=PLAINTEXT, ClientAuthentication.Unauthenticated.Enabled=true, and LoggingInfo.BrokerLogs = {CloudWatchLogs:{Enabled:true,LogGroup:<wired>}, S3:{Enabled:true,Bucket:<bucket>,Prefix:<prefix>}, Firehose:{Enabled:false}}.
4. Poll DescribeClusterV2 to State=ACTIVE and assert it echoes that LoggingInfo; then GetBootstrapBrokers.
5. From the in-VPC executor, open TCP to each bootstrap endpoint and send a hand-framed ApiVersions request; then CreateTopics and Produce records.
6. Poll the wired log group for events and the S3 prefix for a non-empty object, checking the content holds a log4j level token and a kafka logger name; hold the unwired log group empty throughout.
Against the emulator none of steps 4-6 produce an observation and the run ends at the suite's 60 s cap.
