Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__iam__logs.vpc-flow-log-to-cloudwatch-logs-names-the-group-and-omits-s3-fields
Issue-Key: ec2__iam__logs.vpc-flow-log-to-cloudwatch-logs-names-the-group-and-omits-s3-fields|run#11|AssertionError|assert s3_only == [], (
Issue-Commit: 76cd101d4aa6a8011f19dfb6a2bd3d37c5bd8e14

# ec2: DescribeFlowLogs reports the S3-only DestinationOptions block on a flow log whose LogDestinationType is cloud-watch-logs

> _Note: this issue was identified against the code base at commit `76cd101d4aa6a8011f19dfb6a2bd3d37c5bd8e14`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeFlowLogs

## Description
One net-new root cause, covering one check. The check is not marked ALREADY FILED and is not OWNED by any open issue. The only other open issue over this service's flow-log checks, 20261111T030649Z-ec2-iam-logs-vpc-flow-logs-never-deliver.md, is about the delivery path emitting no records; it does not cover the shape DescribeFlowLogs reports for a flow log, and it does not carry this check's replay key.

Cause 1 of 1: a cloud-watch-logs flow log is read back carrying the S3-only DestinationOptions structure.
AWS: ec2 CreateFlowLogs takes DestinationOptions (FileFormat, HiveCompatiblePartitions, PerHourPartition) only for a flow log delivered to Amazon S3 — Smithy: ec2 FlowLog.DestinationOptions -> DestinationOptionsResponse{FileFormat: DestinationFileFormat, HiveCompatiblePartitions: Boolean, PerHourPartition: Boolean}, whose members are documented in S3 terms ("Indicates whether to use Hive-compatible prefixes for flow logs stored in Amazon S3"). A flow log created with LogDestinationType cloud-watch-logs, a LogGroupName and a DeliverLogsPermissionArn has no destination options, so DescribeFlowLogs' FlowLog member for it carries neither DestinationOptions nor LogDestination; it carries ResourceId, TrafficType, LogDestinationType 'cloud-watch-logs', LogGroupName, DeliverLogsPermissionArn, MaxAggregationInterval (default 600), the default version-2 LogFormat, FlowLogStatus and DeliverLogsStatus. The rule is structural: the destination-options members exist only for the s3 destination type, so which members are present follows LogDestinationType, not one recording's values. Recorded behavior confirms the omission for cloud-watch-logs.
Emulator: DescribeFlowLogs on a cloud-watch-logs flow log returns a FlowLog that additionally carries DestinationOptions populated with defaults — the check reports it read back the S3-shaped key DestinationOptions = {'FileFormat': 'plain-text', 'HiveCompatiblePartitions': False, 'PerHourPartition': False} (the values are a per-run artefact of defaulting; the defect is that the member is serialized at all). Every other asserted field of that same read-back matched, so this is the single divergence: the member set is not conditioned on the destination type. Hypothesis: the flow-log record keeps one struct with destination options always populated, and the serializer emits the member unconditionally rather than only for LogDestinationType s3.
Why: a client that branches on the member set to tell an S3 flow log from a CloudWatch Logs one — the documented way to read a FlowLog, since the two destinations are described by disjoint members — concludes a CloudWatch Logs flow log has an S3 file format and partitioning scheme. Tooling that round-trips a DescribeFlowLogs result back into CreateFlowLogs then sends DestinationOptions with a cloud-watch-logs destination, which real AWS rejects, and configuration-drift detectors report a permanent diff on a resource nobody changed.
Fix: the EC2 DescribeFlowLogs response projection for the FlowLog shape (and the CreateFlowLogs state it reads): destination options must be stored/served only for a flow log whose LogDestinationType is s3, and the member must be omitted for cloud-watch-logs and kinesis-data-firehose.
Traces: ec2__iam__logs.vpc-flow-log-to-cloudwatch-logs-names-the-group-and-omits-s3-fields.

## Evidence
Cause 1 — Expected: DescribeFlowLogs on the cloud-watch-logs flow log returns a FlowLog with none of the S3-only members, i.e. neither 'LogDestination' nor 'DestinationOptions' present (the check computes `s3_only = sorted(k for k in ('LogDestination','DestinationOptions') if k in got)` and requires it to be empty).
Actual: AssertionError: a CloudWatch Logs flow log reports S3-shaped keys {'DestinationOptions': {'FileFormat': 'plain-text', 'HiveCompatiblePartitions': False, 'PerHourPartition': False}} — raised at check_ec2__iam__logs_vpc_flow_log_to_cloudwatch_logs_names_the_group_and_omits_s3_fields.py line 37. 'LogDestination' was correctly absent; 'DestinationOptions' was present. The preceding assertions in the same check passed, so ResourceId, TrafficType=ALL, LogDestinationType='cloud-watch-logs', LogGroupName, DeliverLogsPermissionArn, MaxAggregationInterval=600 and the default version-2 LogFormat all read back correctly. No error code or status divergence is involved: the call is a 200.

## Steps to Reproduce
1. ec2 CreateVpc (any CIDR, e.g. 10.2.0.0/16).
2. Create an IAM role trusted by vpc-flow-logs.amazonaws.com with logs:CreateLogStream/logs:PutLogEvents, and a CloudWatch Logs log group.
3. ec2 CreateFlowLogs with ResourceType=VPC, ResourceIds=[the vpc], TrafficType=ALL, LogDestinationType='cloud-watch-logs', LogGroupName=<the group>, DeliverLogsPermissionArn=<the role arn>, and NO DestinationOptions.
4. ec2 DescribeFlowLogs FlowLogIds=[the new flow log id] and inspect the returned FlowLog member set.
Observed: the FlowLog carries a DestinationOptions block (FileFormat/HiveCompatiblePartitions/PerHourPartition).
Expected: no DestinationOptions member at all for a cloud-watch-logs flow log (it appears only for LogDestinationType s3).
