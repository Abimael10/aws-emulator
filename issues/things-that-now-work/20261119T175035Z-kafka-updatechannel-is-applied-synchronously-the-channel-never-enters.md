Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka.channel-concurrent-update-race-rejected-or-serialized
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka.channel-concurrent-update-race-rejected-or-serialized|run#16|AssertionError|assert winning_value is not None, (
Issue-Commit: 5370ef7dc9ed89bab2d3b2b1f9cf43f883224b35

# UpdateChannel is applied synchronously — the channel never enters UPDATING, so a second concurrent UpdateChannel is accepted instead of refused with 400 BadRequestException

> _Note: this issue was identified against the code base at commit `5370ef7dc9ed89bab2d3b2b1f9cf43f883224b35`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateChannel

## Description
ONE net-new root cause in the kafka service, grounded in the single failing behavioral check in this replay that the suite report marked neither ALREADY FILED nor OWNED by an open issue (every other failure in this run is owned).

=== Cause 1 of 1: UpdateChannel mutates the stored channel in place and leaves it ACTIVE, so the MSK channel lifecycle never shows the UPDATING state and the in-flight-update conflict refusal is unreachable. (Failing check: kafka.channel-concurrent-update-race-rejected-or-serialized.)

AWS: Smithy com.amazonaws.kafka#Kafka — UpdateChannel is PUT /v1/clusters/{ClusterArn}/channels/{ChannelArn}, code 200, @idempotent; input members ChannelArn (@httpLabel, required), ClusterArn (@httpLabel, required), S3DestinationUpdate (whose DataFreshnessInSeconds is required, 'Allowed range: 300 to 900'); output is only {channelArn (required), clusterOperationArn}, i.e. UpdateChannel echoes no destination field and returns a cluster-operation handle, which is the model's statement that the update is an ASYNCHRONOUS cluster operation. DescribeChannel (GET same path, 200) REQUIRES status, drawn from the ChannelStatus enum [CREATING, ACTIVE, UPDATING, DELETING, FAILED, SUSPENDING, SUSPENDED]; UPDATING exists precisely because an accepted UpdateChannel moves the channel ACTIVE -> UPDATING and back to ACTIVE when the operation completes. Consequently, a second UpdateChannel issued while the channel is still UPDATING is refused: 400 BadRequestException (modeled error of UpdateChannel, members invalidParameter/message), with the message AWS words for this error class — recorded behavior only for the exact text, which the recording fixes as the fixed string 'Channel cannot be updated in state UPDATING. Channel must be in ACTIVE state to be updated.' Its one variable part is the state name, taken from the channel's own state in service state; no request value, id, name or timestamp appears in it. AWS is also permitted to SERIALIZE the two updates (the second finds the channel back in ACTIVE and is accepted), so the requirement is that the state machine exposes UPDATING at all and refuses an overlapping update while it holds — not that any particular ordering wins.

Emulator: after CreateClusterV2 (Provisioned, express.m7g.large, 3 AZ-distinct client subnets) -> ACTIVE, CreateTopic -> ACTIVE, CreateChannel -> ACTIVE, three rounds of (UpdateChannel with a new S3DestinationUpdate.DataFreshnessInSeconds, then an immediate DescribeChannel) show the first UpdateChannel answered 200 every time and the channel's Status ALREADY back to 'ACTIVE' on the very next read — recorded rounds (first result, status observed before the second call, second result): [(('ok', 300), 'ACTIVE', None), (('ok', 450), 'ACTIVE', None), (('ok', 300), 'ACTIVE', None)]. The channel is never observed in UPDATING in any round, so the overlapping second UpdateChannel can never be issued and the 400 BadRequestException refusal is unreachable code. The emulator applies the destination update synchronously inside the UpdateChannel handler instead of parking the channel in UPDATING under a cluster operation that settles on a later probe.

Why: a client that updates a channel's S3 buffering and then waits for the operation (poll DescribeChannel for UPDATING -> ACTIVE, or DescribeClusterOperationV2 on the returned clusterOperationArn) sees a lifecycle it cannot drive: UPDATING never appears, so a waiter written against AWS either exits immediately on a state it never saw or waits for a transition that never happens. Worse, the conflict guard is gone: two concurrent UpdateChannel calls are BOTH accepted and the last writer silently overwrites the first, where AWS either refuses the second with 400 BadRequestException or serializes it; a client relying on that refusal to detect a racing operator or a retried request gets a lost update with no error.

Fix: the channel state machine in the kafka service's state layer plus the UpdateChannel handler and its state-precondition validation — an accepted UpdateChannel must set the stored channel's status to UPDATING, attach the cluster operation it returns, and only apply S3DestinationUpdate/IcebergDestinationUpdate and return to ACTIVE when that operation settles; while the status is not ACTIVE, UpdateChannel must refuse with 400 BadRequestException carrying the state-based message above (or serialize behind the in-flight operation), and DescribeChannel must project UPDATING for the duration.

Traces: kafka.channel-concurrent-update-race-rejected-or-serialized. Related open issues that do NOT carry this check's replay key (the suite reports it unowned, so none of them covers it; named only for the neighbourhood): 20261111T024655Z-kafka-msk-channel-update-never-becomes-observable-channel-update.md (the mutation not converging at all), 20261114T065925Z-kafka-msk-asynchronous-lifecycles-never-settle-channel-configuration-delete.md (channel/configuration/replicator lifecycles never settling), 20261104T171616Z-kafka-updatechannel-stub.md and 20261029T175822Z-kafka-listchannels-stub.md (scaffold stubs — note UpdateChannel here answers 200 and mutates state, so it is no longer a bare stub).

## Evidence
Failing check (verdict 'mismatch'), listed under the 'kafka' service heading of the suite report:
  kafka.channel-concurrent-update-race-rejected-or-serialized
  AssertionError: no round sent the second UpdateChannel while the first kept the channel UPDATING and saw it rejected; (first, status before second, second) per round: [(('ok', 300), 'ACTIVE', None), (('ok', 450), 'ACTIVE', None), (('ok', 300), 'ACTIVE', None)]

Expected (AWS), per the Smithy model and the check's own asserts:
  1. UpdateChannel(ChannelArn, ClusterArn, S3DestinationUpdate={DataFreshnessInSeconds: <300..900>}) on an ACTIVE channel -> 200 {channelArn, clusterOperationArn}.
  2. An immediately following DescribeChannel -> 200 with Status == 'UPDATING' (ChannelStatus enum member) while that cluster operation is in flight.
  3. A second UpdateChannel issued in that window -> HTTP 400, error code 'BadRequestException', message exactly 'Channel cannot be updated in state UPDATING. Channel must be in ACTIVE state to be updated.' — fixed text except the state name, which comes from the channel's state in the emulator's own state (no request value, generated name, ARN, id or timestamp belongs in it). AWS may instead serialize the second call, in which case the channel is seen back in ACTIVE first; the check retries such a round.
  4. Once the channel settles ACTIVE again, DescribeChannel.S3DestinationConfiguration.DataFreshnessInSeconds equals the accepted winner's value only, with no trace of a rejected update.

Actual: step 2 never holds. In all three rounds the first UpdateChannel returned 200 and the next DescribeChannel already reported Status 'ACTIVE' (values 300/450/300 applied synchronously), so no overlapping second UpdateChannel could be sent, no 400 BadRequestException was ever served, and the channel is never observable in UPDATING.

Per-run values in this replay — generated cluster/topic/channel/bucket/role names, ARNs, account id, region, host and port, timestamps and request ids — are not requirements; the requirement is the ACTIVE -> UPDATING -> ACTIVE transition and the 400/BadRequestException refusal of an overlapping update.

## Steps to Reproduce
Boot the emulator (kafka service) and run:
1. CreateClusterV2 a Provisioned cluster: Provisioned.BrokerNodeGroupInfo.InstanceType 'express.m7g.large', 3 AZ-distinct ClientSubnets plus the default security group, a KafkaVersion that ListKafkaVersions reports ACTIVE below the 4.x line, NumberOfBrokerNodes 3. Poll DescribeClusterV2 until State == 'ACTIVE'.
2. CreateTopic on that cluster (PartitionCount 1, ReplicationFactor 2); poll DescribeTopic until Status == 'ACTIVE' and read its TopicArn.
3. Create an S3 bucket and an IAM role trusted by kafka.amazonaws.com with s3:PutObject/GetBucketLocation/ListBucket on that bucket.
4. CreateChannel(ChannelName, ClusterArn, TopicConfigurationList=[{TopicArn, RecordConverter:{ValueConverter:'BYTE_ARRAY'}}], S3DestinationConfiguration={DataFreshnessInSeconds:600, ServiceExecutionRoleArn:<role>, Storage:{BucketArn, CompressionType:'NONE', StorageClass:'STANDARD', OutputPrefix:'data/'}, DeadLetterQueueS3:{BucketArn, ErrorOutputPrefix:'dlq/'}}); poll DescribeChannel until Status == 'ACTIVE'.
5. Call UpdateChannel(ChannelArn, ClusterArn, S3DestinationUpdate={DataFreshnessInSeconds: 450}) and assert 200.
6. IMMEDIATELY call DescribeChannel and record Status. Expected 'UPDATING'; the emulator reports 'ACTIVE' — this is the divergence.
7. While that first update is in flight, call UpdateChannel(ChannelArn, ClusterArn, S3DestinationUpdate={DataFreshnessInSeconds: 900}) and assert it is refused with HTTP 400, Error.Code 'BadRequestException' and Error.Message 'Channel cannot be updated in state UPDATING. Channel must be in ACTIVE state to be updated.' (or that the channel was already back in ACTIVE because the service serialized the two operations).
8. After the channel settles ACTIVE, assert DescribeChannel.S3DestinationConfiguration.DataFreshnessInSeconds == 450 (the accepted winner) with no trace of the refused 900.
9. Replay kafka.channel-concurrent-update-race-rejected-or-serialized and confirm it passes.
