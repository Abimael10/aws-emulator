Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__iam__logs.log-vpc-flow-logs-scope-excludes-non-member-eni ec2__iam__logs.log-vpc-flow-logs-traffic-type-filter-excludes-other-action ec2__iam__logs.xsvc-ec2-3-flow-log-delivery-role-without-put-log-events-denied
Issue-Key: ec2__iam__logs.log-vpc-flow-logs-scope-excludes-non-member-eni|_poll_cwl#31|Timeout|time.sleep(20)
Issue-Key: ec2__iam__logs.log-vpc-flow-logs-traffic-type-filter-excludes-other-action|_poll_cwl#33|Timeout|time.sleep(20)
Issue-Key: ec2__iam__logs.xsvc-ec2-3-flow-log-delivery-role-without-put-log-events-denied|_poll_cwl#30|Timeout|time.sleep(20)
Issue-Commit: ee29b9ed80499ffda1e629bc10e5c9c45fea7a64

# ec2__iam__logs: VPC flow logs with a CloudWatch Logs destination never deliver a single record, so every flow-log scope/filter/delivery-permission scenario times out

> _Note: this issue was identified against the code base at commit `ee29b9ed80499ffda1e629bc10e5c9c45fea7a64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFlowLogs

## Description
Cause 1 of 1: an ACTIVE flow log whose LogDestinationType is cloud-watch-logs never writes a record into its log group, so every check that waits for captured traffic polls to the harness cap and no flow-log assertion is ever reached. Failing checks: ec2__iam__logs.log-vpc-flow-logs-scope-excludes-non-member-eni, ec2__iam__logs.log-vpc-flow-logs-traffic-type-filter-excludes-other-action, ec2__iam__logs.xsvc-ec2-3-flow-log-delivery-role-without-put-log-events-denied (all three reported as 'Check timed out after 600.0s').

AWS: Smithy (services/service-com-amazonaws-ec2-amazonec2) models CreateFlowLogs(ResourceIds (required), ResourceType (required), TrafficType, LogDestinationType, LogGroupName, DeliverLogsPermissionArn, LogFormat, MaxAggregationInterval, ...) -> {FlowLogIds, Unsuccessful} with no modeled errors, and DescribeFlowLogs reports FlowLogStatus, DeliverLogsStatus and DeliverLogsErrorMessage. Recorded behavior, per AWS's flow-logs documentation: a flow log created with LogDestinationType=cloud-watch-logs, a LogFormat naming the standard fields, MaxAggregationInterval=60 and a delivery role trusted by vpc-flow-logs.amazonaws.com that allows logs:CreateLogStream and logs:PutLogEvents on the target group delivers, within roughly one aggregation interval plus the publication buffer, one log event per aggregated flow per capture window into that log group: the event message is the configured fields in the configured order (interface-id srcaddr dstaddr srcport dstport protocol packets bytes start end action log-status), packets/bytes are the real counts of the observed flow, action is ACCEPT for an allowed flow and REJECT for one a security group or NACL dropped, log-status is OK, start/end are epoch seconds inside the capture window, and the interface-id is the ENI the flow was observed on. Each scope delivers exactly its members (an ENI flow log only that ENI, a subnet flow log only the subnet's ENIs, a VPC flow log all of them), a TrafficType-filtered log carries only the matching action, and a flow log whose delivery role lacks logs:PutLogEvents stays FlowLogStatus ACTIVE while DescribeFlowLogs reports DeliverLogsStatus FAILED with a non-empty DeliverLogsErrorMessage and its group stays empty.

Emulator: CreateFlowLogs is accepted (one FlowLogId, no Unsuccessful entry), but FilterLogEvents on the destination log group returns no event for any of the three scopes across a 20 s poll loop, so none of the above can be observed; the checks are cut off by the 600 s harness cap with no assertion reached. Hypothesis: the flow-log delivery path to CloudWatch Logs is not implemented — no log stream or log event is ever produced for an ACTIVE flow log, and consequently the DeliverLogsStatus FAILED/SUCCESS distinction for the delivery role is never exercised either.

Why: flow logs are unusable as an observability surface. A client that creates a flow log sees it ACTIVE and then waits forever on an empty log group; traffic auditing, reject-rule diagnosis and 'which ENI did this flow belong to' investigations all read 'no traffic' instead of the recorded flows, and a delivery-permission misconfiguration is indistinguishable from a correct one because both groups stay empty.

Fix: the EC2 flow-log delivery path — the component that aggregates observed per-ENI flows into records and publishes them (CreateLogStream + PutLogEvents under the DeliverLogsPermissionArn role) to the CloudWatch Logs destination, honouring the flow log's scope (NetworkInterface / Subnet / VPC), its TrafficType filter, its LogFormat field order and MaxAggregationInterval — plus the DeliverLogsStatus / DeliverLogsErrorMessage state DescribeFlowLogs serves for a role that cannot write. Related open issues that do NOT carry these checks: 20261111T040849Z-ec2-s3-vpc-flow-logs-with-an-s3-destination-never.md (the same absence for the S3 destination) and 20261102T090835Z-ec2-logs-s3-a-create-complete-flow-log-stack-s-cloudwatch.md (a CloudFormation flow-log stack's destination group missing).

Traces: ec2__iam__logs.log-vpc-flow-logs-scope-excludes-non-member-eni, ec2__iam__logs.log-vpc-flow-logs-traffic-type-filter-excludes-other-action, ec2__iam__logs.xsvc-ec2-3-flow-log-delivery-role-without-put-log-events-denied.

## Evidence
All three checks: suite output 'Error: Check timed out after 600.0s' (verdict mismatch), i.e. no assertion was reached; each blocks in the same helper, _poll_cwl(..., budget_s=1200), which pages logs.filter_log_events(logGroupName=<flow-log group>) every 20 s until the caused flows appear.
Setup common to all three: CreateFlowLogs(ResourceIds=[<eni|subnet|vpc>], ResourceType='NetworkInterface'|'Subnet'|'VPC', TrafficType='ALL'|'ACCEPT'|'REJECT', LogDestinationType='cloud-watch-logs', LogGroupName=<group>, DeliverLogsPermissionArn=<role trusted by vpc-flow-logs.amazonaws.com with logs:CreateLogStream+logs:PutLogEvents on that group>, LogFormat='${interface-id} ${srcaddr} ${dstaddr} ${srcport} ${dstport} ${protocol} ${packets} ${bytes} ${start} ${end} ${action} ${log-status}', MaxAggregationInterval=60) — accepted, one FlowLogId, no Unsuccessful.
Expected (AWS): log events in each group whose message splits into the 12 configured fields, with action ACCEPT for the allowed flow and REJECT for the SG-blocked one, packets > 0, log-status 'OK', start/end inside the run window, and interface-id restricted to the flow log's scope (ENI log: only that ENI; subnet log: only that subnet's ENIs, never the other subnet's; VPC log: all three ENIs). For the delivery-permission check: >= 3 aggregation intervals of the control flow in the good group, DescribeFlowLogs(good).DeliverLogsStatus == 'SUCCESS', and for the role lacking logs:PutLogEvents DeliverLogsStatus == 'FAILED' with a non-empty DeliverLogsErrorMessage, FlowLogStatus still 'ACTIVE' and an empty group.
Actual (emulator): filter_log_events returns no events for any group for the whole poll window; the checks die at the 600 s cap, so DeliverLogsStatus is never verdicted either.

## Steps to Reproduce
1. Create a VPC with two subnets, a client SG and a server SG with ingress from the client SG, and three ENIs (one client, two servers).
2. Create a CloudWatch Logs log group and an IAM role trusted by vpc-flow-logs.amazonaws.com allowing logs:CreateLogStream and logs:PutLogEvents on it.
3. CreateFlowLogs with LogDestinationType='cloud-watch-logs', LogGroupName=<group>, DeliverLogsPermissionArn=<role>, MaxAggregationInterval=60 and the 12-field LogFormat above, once per scope (the server ENI, the server subnet, the VPC).
4. Launch instances on those ENIs and drive TCP connections from the client to each server port (allowed) and to a blocked port.
5. Page FilterLogEvents on each group: AWS produces records within ~1-2 aggregation intervals, scoped to each flow log's resource; the emulator produces none, indefinitely.
6. Repeat step 3 with a second flow log whose role omits logs:PutLogEvents and DescribeFlowLogs it: AWS reports DeliverLogsStatus 'FAILED' with a DeliverLogsErrorMessage and FlowLogStatus 'ACTIVE', its group empty, while the control reports 'SUCCESS' and delivers.
