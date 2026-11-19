Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs
Issue-Key: elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs|_body#1|AssertionError|assert inside, (
Issue-Commit: e822fa4ca12543e97642b0a71000bedf3ee319c6

# elasticache__logs: delivered slow-log records are synthesised — ClientAddress is the placeholder '?:0' and ClientName is empty instead of the connection that issued the command

> _Note: this issue was identified against the code base at commit `e822fa4ca12543e97642b0a71000bedf3ee319c6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCacheCluster

## Description
Cause 1 of 1: a slow-log record delivered to CloudWatch Logs does not describe the client connection that issued the slow command.
AWS: recorded/documented behavior only — the ElastiCache slow-log record schema is not in the Smithy model; it is defined by the engine's SLOWLOG entries as reshaped by ElastiCache log delivery (https://docs.aws.amazon.com/AmazonElastiCache/latest/dg/Log_Delivery.html). A delivered JSON record carries CacheClusterId, CacheNodeId, Id, Timestamp, Duration (us), Command, ClientAddress and ClientName, where ClientAddress is the <ip>:<port> of the connection the engine actually served the command on (the client's address — for a command issued from a Lambda ENI in the cluster's VPC, an address inside that VPC's CIDR) and ClientName is the connection's CLIENT SETNAME value (empty only when the client set none).
Emulator: the record is delivered to the configured log group, but its ClientAddress is the fixed placeholder string '?:0' — no IP and port 0 — and ClientName is empty. Because '?:0' lies in no VPC CIDR, the record demonstrably did not come from the connection that ran the command: it is a record the emulator authored around the command text rather than one recorded by the engine from its own client table. The check's assertion names exactly this: the delivered ClientAddress is not inside the executor VPC's randomly chosen CIDR even though the command was issued from a Lambda ENI in that VPC.
Why: a client cannot attribute a slow command to the caller that made it. Operational use of the slow log — finding which application host, connection or named client is driving latency, correlating a slow command with an ENI/security group, or distinguishing two workloads sharing a cluster — is impossible, and the placeholder '?:0' is not even a parseable address for tooling that splits it into host and port.
Fix: the ElastiCache slow-log record projection in the log-delivery path — populate ClientAddress from the engine connection that executed the command (its peer address and port) and ClientName from that connection's name, rather than emitting fixed placeholders. The record must be derived from the engine's own SLOWLOG entry for the served connection.
Traces: elasticache__logs.slow-log-delivery-lands-the-engine-command-in-cloudwatch-logs.

Related open issues that do NOT cover this check (the suite left it unowned): 20261118T131546Z-elasticache-logs-elasticache-the-served-cache-engine-answers-slowlog-with.md, 20261108T145640Z-elasticache-logs-elasticache-a-slow-log-logdeliveryconfiguration-naming-a-cloudwatch.md, 20261105T090541Z-elasticache-logs-the-slow-log-delivery-check-cannot.md.

## Evidence
Expected: the delivered slow-log record's ClientAddress is the <ip>:<port> of the connection that issued the command — for a command issued from a Lambda ENI inside the cluster's VPC, an address inside that VPC's CIDR — and ClientName is that connection's name.

Actual (AssertionError): "the delivered record's ClientAddress '?:0' is not inside the executor VPC CIDR 10.128.0.0/16, which was chosen at random seconds before the cluster was created. The command was issued from a Lambda ENI in that VPC, so a record that did not come from our connection is a synthesised record rather than a delivered one. Full record: {'CacheClusterId': '<cluster-id>', 'CacheNodeId': '0001', 'Id': 4, 'Timestamp': <epoch>, 'Duration (us)': 6, 'Command': 'GETRANGE <key> 0 -1', 'ClientAddress': '?:0', 'ClientName': ''}".

The fixed, environment-independent part of this divergence is the literal placeholder ClientAddress '?:0' and the empty ClientName; the cluster id, VPC CIDR, key name, Id, Timestamp and Duration are per-run values and are not part of the requirement.

## Steps to Reproduce
1. Create a VPC with a randomly chosen CIDR, subnets and a cache subnet group; create a single-node Redis/Valkey cache cluster in it with a slow-log LogDeliveryConfiguration (DestinationType cloudwatch-logs) naming an existing log group, ApplyImmediately=true.
2. Lower slowlog-log-slower-than so ordinary commands are recorded, and set slowlog-max-len.
3. From a Lambda function attached to a subnet of that same VPC, open a connection to the cluster endpoint and issue a command slow enough to be recorded (e.g. GETRANGE on a large value).
4. Poll the destination log group for the delivered slow-log record and inspect ClientAddress/ClientName: ClientAddress is '?:0' and ClientName is '' instead of the Lambda ENI's address inside the VPC CIDR.
