Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-vfd-describe-filter-name-and-token-mistakes
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-vfd-describe-filter-name-and-token-mistakes ec2-vfd-firehose-destination-mistakes ec2-vfd-log-format-field-name-refusals ec2-vfd-s3-destination-mistakes-then-recovery ec2-vfd-resource-and-flow-log-id-mistakes-are-atomic ec2-vfd-cloudwatch-logs-destination-and-delivery-role
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeFlowLogs|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|DescribeFlowLogs|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|CreateFlowLogs|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|CreateFlowLogs|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|DescribeFlowLogs|2xx->2xx|$.DescribeFlowLogsResponse.flowLogSet.item.destinationOptions
Issue-Commit: 688abb98d0ad3ae2c07107aa110c7330aa2e99f9

# ec2 VPC Flow Logs refusals and DescribeFlowLogs output diverge from AWS

> _Note: this issue was identified against the code base at commit `688abb98d0ad3ae2c07107aa110c7330aa2e99f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeFlowLogs

## Description
Four net-new causes on the ec2 VPC Flow Logs surface: three refusals that carry the wrong error code or wording, and one describe response that emits a member AWS omits for the destination type in question. EC2 is ec2Query and models no operation errors (smithy_query reports "errors": [] for CreateFlowLogs and DescribeFlowLogs), so the codes and messages are recorded behaviour only.

Cause 1 of 4: DescribeFlowLogs with an invalid NextToken.
AWS: recorded behavior only. 400, Code InvalidParameter, Message "NextToken is not valid." — wholly fixed text, no request-derived part. Smithy: DescribeFlowLogs is @paginated with inputToken/outputToken NextToken (target String).
Emulator: 400, Code InvalidPaginationToken, Message "The passed pagination token is invalid" — a code AWS does not use on this operation and emulator-authored prose.
Why: a client that distinguishes "bad token, restart the scan" from other InvalidParameter refusals by Code never sees the code AWS sends; retry logic keyed on it does not fire. The open issue 20261116T190237Z-ec2-invalid-pagination-token-rejected-with-invalidpaginationtoken-instead.md describes the same class but carries none of these replay keys.
Fix: the shared pagination-token decoder's error in the ec2 service crate, as consumed by the DescribeFlowLogs handler.
Traces: ec2-vfd-describe-filter-name-and-token-mistakes#task#8.

Cause 2 of 4: CreateFlowLogs with LogDestinationType kinesis-data-firehose and a LogDestination ARN that is not a Firehose delivery stream.
AWS: recorded behavior only. 400, Message "Invalid Kinesis Data Firehose LogDestination: <arn>" — fixed text "Invalid Kinesis Data Firehose LogDestination: " plus the LogDestination ARN copied from the request.
Emulator: 400 with "Invalid kinesis-data-firehose destination: <arn>" — the emulator interpolates the wire enum token and the word "destination" instead of AWS's service name and the member name LogDestination.
Why: the message is a requirement; AWS names the service in prose ("Kinesis Data Firehose") and the input member ("LogDestination"), and consumers that assert on it fail.
Fix: the CreateFlowLogs handler's destination validation, which must render the destination-type-specific message rather than templating the enum value.
Traces: ec2-vfd-firehose-destination-mistakes#task#5 (cluster also covers task#6 of that fixture, ec2-vfd-log-format-field-name-refusals#task#6 and ec2-vfd-s3-destination-mistakes-then-recovery#task#6).

Cause 3 of 4: CreateFlowLogs supplying a cross-account delivery role without both role members.
AWS: recorded behavior only. 400, Code InvalidParameter, Message "When providing a role for cross-account delivery, both the DeliverLogsPermissionArn and DeliverCrossAccountRole fields must be set." — fixed text naming the two modelled members. AWS validates this member combination BEFORE it looks for an existing identical flow log.
Emulator: 400, Code FlowLogAlreadyExists, Message "Error. There is an existing Flow Log with the same configuration and log destination." — the duplicate-detection branch runs first, so the request-shape defect is never reported.
Why: wrong code and wrong diagnosis: the caller is told to delete a flow log when what it must do is supply the second role member. A caller that then deletes the existing flow log and retries gets the same refusal.
Fix: validation order in the CreateFlowLogs handler — validate the DeliverLogsPermissionArn/DeliverCrossAccountRole combination before the duplicate-configuration check — and the InvalidParameter message itself.
Traces: ec2-vfd-firehose-destination-mistakes#task#7 (cluster also covers ec2-vfd-resource-and-flow-log-id-mistakes-are-atomic#task#4, ec2-vfd-s3-destination-mistakes-then-recovery#task#3 and #task#4).

Cause 4 of 4: DescribeFlowLogs for a flow log whose LogDestinationType is cloud-watch-logs.
AWS: recorded behavior only, consistent with the model's documentation. Smithy: FlowLog.DestinationOptions (xmlName destinationOptions, target DestinationOptionsResponse) carries fileFormat/hiveCompatiblePartitions/perHourPartition, which are S3-delivery options; AWS omits the destinationOptions element entirely on a flow log delivering to CloudWatch Logs.
Emulator: 200 with destinationOptions present on a cloud-watch-logs flow log, populated with the S3 defaults (fileFormat plain-text, hiveCompatiblePartitions false, perHourPartition false).
Why: a client reading destinationOptions to decide whether a flow log delivers Parquet/Hive-partitioned objects to S3 concludes a CloudWatch Logs flow log has S3 delivery options, and round-tripping the describe output into a create request produces a request AWS refuses.
Fix: the DescribeFlowLogs serializer (and the flow-log record in state), which must emit destinationOptions only when LogDestinationType is s3. The open issue 20261117T033811Z-ec2-iam-logs-ec2-describeflowlogs-reports-the-s3-only-destinationoptions-block.md describes this same behaviour but does not carry this replay key, so the cluster is filed here.
Traces: ec2-vfd-cloudwatch-logs-destination-and-delivery-role#task#10.

## Evidence
Replay of trace set simparity-wire-aws-v1. Four mismatch clusters, service=ec2, empty stub_path, empty owned_by:

1. ec2-vfd-describe-filter-name-and-token-mistakes#task#8 DescribeFlowLogs 400->400: $.Response.Errors.Error.Code expected 'InvalidParameter' got 'InvalidPaginationToken'; $.Response.Errors.Error.Message expected 'NextToken is not valid.' got 'The passed pagination token is invalid'.
2. ec2-vfd-firehose-destination-mistakes#task#5 CreateFlowLogs 400->400: Message expected 'Invalid Kinesis Data Firehose LogDestination: arn:aws:s3:::sim2real-probe-vfd-bdf82dc1-absent' got 'Invalid kinesis-data-firehose destination: arn:aws:s3:::sim2real-probe-vfd-bdf82dc1-absent'. affected_count 4.
3. ec2-vfd-firehose-destination-mistakes#task#7 CreateFlowLogs 400->400: Code expected 'InvalidParameter' got 'FlowLogAlreadyExists'; Message expected 'When providing a role for cross-account delivery, both the DeliverLogsPermissionArn and DeliverCrossAccountRole fields must be set.' got 'Error. There is an existing Flow Log with the same configuration and log destination.'. affected_count 4.
4. ec2-vfd-cloudwatch-logs-destination-and-delivery-role#task#10 DescribeFlowLogs 200->200: $.DescribeFlowLogsResponse.flowLogSet.item.destinationOptions unexpected_present (expected None, got {'fileFormat': 'plain-text', 'hiveCompatiblePartitions': 'false', 'perHourPartition': 'false'}). affected_count 2 (also task#11).

Model grounding (smithy_query, services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json): DescribeFlowLogs @paginated on NextToken, errors []; shape FlowLog member DestinationOptions -> DestinationOptionsResponse, xmlName destinationOptions, alongside LogDestinationType -> LogDestinationType enum.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1).
3. Re-replay ec2-vfd-describe-filter-name-and-token-mistakes#task#8 plus each step in trace_ids. Steps 1-3 are ec2Query CreateFlowLogs/DescribeFlowLogs POSTs with a 400 <Response><Errors><Error> body; step 4 is a 200 DescribeFlowLogs whose flowLogSet item must not contain a destinationOptions element when logDestinationType is cloud-watch-logs.
