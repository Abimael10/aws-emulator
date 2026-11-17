Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__iam__logs.deploy-ec2-flow-log-deliver-logs-permission-arn-to-cloudwatch-logs-round-trip-and-delete cloudformation__ec2__iam__logs.deploy-ec2-flow-log-missing-deliver-logs-permission-arn-rejected-and-rolled-back
Issue-Key: cloudformation__ec2__iam__logs.deploy-ec2-flow-log-deliver-logs-permission-arn-to-cloudwatch-logs-round-trip-and-delete|_poll_flow_log#12|IndexError|fl = ec2.describe_flow_logs(FlowLogIds=[flow_log_id])["FlowLogs"][0]
Issue-Key: cloudformation__ec2__iam__logs.deploy-ec2-flow-log-missing-deliver-logs-permission-arn-rejected-and-rolled-back|run#9|AssertionError|assert status == "ROLLBACK_COMPLETE", (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cloudformation__ec2__iam__logs: 2 causes — a stack-created AWS::EC2::FlowLog is unknown to DescribeFlowLogs, and one missing DeliverLogsPermissionArn is accepted instead of failing the stack

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeFlowLogs

## Description
Two net-new root causes in the CloudFormation AWS::EC2::FlowLog handler.

Cause 1 of 2: a flow log the stack reports created does not exist in EC2.
AWS: recorded behavior only for the CFN handler; the EC2 contract is that CreateFlowLogs returns a FlowLogId and DescribeFlowLogs(FlowLogIds=[<id>]) then returns exactly that flow log (FlowLogs with one member carrying FlowLogStatus/DeliverLogsStatus). A CREATE_COMPLETE AWS::EC2::FlowLog whose physical id is a flow-log id must be resolvable the same way.
Emulator: DescribeFlowLogs(FlowLogIds=[<the stack's physical id>]) returns an EMPTY FlowLogs list, so the polling read indexes into nothing (IndexError: list index out of range). The resource exists only in the stack's bookkeeping.
Why: every post-deploy verification, drift read and delete path resolves the flow log by its id; a client sees a stack that deployed and a flow log that is not there, and a later DeleteFlowLogs cannot find it either.
Fix: the AWS::EC2::FlowLog resource handler's create path — it must create the flow log in EC2's own state (the state DescribeFlowLogs reads) and return that id as the physical resource id.
Failing check: cloudformation__ec2__iam__logs.deploy-ec2-flow-log-deliver-logs-permission-arn-to-cloudwatch-logs-round-trip-and-delete

Cause 2 of 2: a cloud-watch-logs flow log with no DeliverLogsPermissionArn is created successfully.
AWS: recorded behavior only — a flow log whose LogDestinationType is cloud-watch-logs REQUIRES DeliverLogsPermissionArn; CreateFlowLogs refuses the request, the resource create fails, and the stack ends ROLLBACK_COMPLETE with a resource-handler failure reason of the form 'Resource handler returned message: "<service message>" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)' (the token and the relayed service message are per-run values; the envelope and the handler error code are the requirement).
Emulator: the stack reaches CREATE_COMPLETE — the missing permission ARN is never validated, so a negative deployment silently succeeds.
Why: a template that cannot possibly deliver logs is reported as a healthy deployment; the whole class of 'stack must roll back' tests and guardrails passes against the emulator and fails against AWS.
Fix: the AWS::EC2::FlowLog handler's property validation (required DeliverLogsPermissionArn for a CloudWatch Logs destination) and the failure-to-rollback path that surfaces it.
Failing check: cloudformation__ec2__iam__logs.deploy-ec2-flow-log-missing-deliver-logs-permission-arn-rejected-and-rolled-back

## Evidence
Cause 1: [error] cloudformation__ec2__iam__logs.deploy-ec2-flow-log-deliver-logs-permission-arn-to-cloudwatch-logs-round-trip-and-delete — IndexError: list index out of range at `ec2.describe_flow_logs(FlowLogIds=[flow_log_id])["FlowLogs"][0]`; expected one FlowLogs entry for the id the CREATE_COMPLETE stack reported, got an empty list.

Cause 2: [mismatch] cloudformation__ec2__iam__logs.deploy-ec2-flow-log-missing-deliver-logs-permission-arn-rejected-and-rolled-back — AssertionError: stack ended 'CREATE_COMPLETE', expected ROLLBACK_COMPLETE (the check also asserts the DELETE/CREATE_FAILED event reason matches 'Resource handler returned message: "..." (RequestToken: ..., HandlerErrorCode: ...)').

## Steps to Reproduce
1. Deploy a stack with a VPC and an AWS::EC2::FlowLog (ResourceType VPC, LogDestinationType cloud-watch-logs, a log group and a DeliverLogsPermissionArn role); when it reaches CREATE_COMPLETE, call DescribeFlowLogs(FlowLogIds=[<Ref of the flow log>]).
2. Deploy the same stack with DeliverLogsPermissionArn omitted and poll the stack status plus its events.
