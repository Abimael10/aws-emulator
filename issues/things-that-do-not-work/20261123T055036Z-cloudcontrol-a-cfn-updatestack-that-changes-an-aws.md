Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudcontrol.deploy-cfn-update-reflected-in-getresource
Issue-Shape-Id: com.amazonaws.cloudcontrol#CloudApiService
Issue-Key: cloudcontrol.deploy-cfn-update-reflected-in-getresource|run#14|WaiterError|cfn.get_waiter("stack_update_complete").wait(
Issue-Commit: 7de16721fc47223af60b1bba7682783f47cdc260

# cloudcontrol: a CFN UpdateStack that changes an AWS::SQS::Queue's MessageRetentionPeriod in place ends UPDATE_ROLLBACK_COMPLETE instead of UPDATE_COMPLETE

> _Note: this issue was identified against the code base at commit `7de16721fc47223af60b1bba7682783f47cdc260`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
ONE net-new root cause in the cloudcontrol service's CloudFormation-backed AWS::SQS::Queue update path. A fixer must close this one cause before submitting.

Cause 1 of 1: a stack update whose only change is the MessageRetentionPeriod of an AWS::SQS::Queue — a mutable, non-create-only property — fails and rolls back; the stack settles UPDATE_ROLLBACK_COMPLETE instead of UPDATE_COMPLETE, so no later assertion of the check (GetResource reflecting the new value, stack delete, post-delete ResourceNotFoundException) is ever reached. Failing check: cloudcontrol.deploy-cfn-update-reflected-in-getresource.

AWS: Smithy (sqs, services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json) SetQueueAttributesRequest.Attributes documents "MessageRetentionPeriod – The length of time, in seconds, for which Amazon SQS retains a message. Valid values: An integer representing seconds, from 60 (1 minute) to 1,209,600 (14 days). Default: 345,600 (4 days)." Both values in the trace (345600, the default, and 1209600, the documented maximum) are therefore legal, and the attribute is settable on a live queue, so CloudFormation applies this change in place through SetQueueAttributes with no replacement and no interruption. The status progression itself is recorded behavior only: UpdateStack is accepted, the stack goes UPDATE_IN_PROGRESS -> UPDATE_COMPLETE_CLEANUP_IN_PROGRESS -> UPDATE_COMPLETE, and cloudcontrol GetResource(TypeName="AWS::SQS::Queue", Identifier=<the queue url>) then reports Properties.MessageRetentionPeriod equal to the template's new value. No refusal, error code or rollback is part of AWS's behavior for this update.
Emulator: the update is accepted but reaches the terminal failure status UPDATE_ROLLBACK_COMPLETE — botocore's StackUpdateComplete waiter reports `Waiter StackUpdateComplete failed: Waiter encountered a terminal failure state: For expression "Stacks[].StackStatus" we matched expected path: "UPDATE_ROLLBACK_COMPLETE" at least once`. The create of the same template (MessageRetentionPeriod 345600) reached CREATE_COMPLETE and the pre-update GetResource read of 345600 passed, so the defect is specific to applying the changed value during the update; the queue is left at the pre-update retention. Hypothesis (neither the model nor the recording states the emulator's reason, and the check errors at the waiter before any StackStatusReason is read): the AWS::SQS::Queue update handler does not support an in-place MessageRetentionPeriod change — it either refuses the property as not updatable or rejects the value — and CloudFormation turns that resource-level failure into a stack rollback.
Why: a client relying on AWS's behavior can never retune a queue's retention through IaC. Every `cloudformation update-stack` / `cdk deploy` / `sam deploy` that changes MessageRetentionPeriod comes back as a failed, rolled-back deployment, and any Cloud Control read that is supposed to observe the updated value sees the stale one. Worse, the rollback is reported as a legitimate terminal state, so pipelines treat a perfectly valid template as a broken change rather than as an emulator gap.
Fix: the AWS::SQS::Queue update handler invoked by the CloudFormation stack-update executor in the cloudcontrol service — the step that diffs the resource's properties and applies the mutable ones. A MessageRetentionPeriod change must be applied in place against the live queue (SetQueueAttributes) and the resource update must succeed, driving the stack to UPDATE_COMPLETE; only values outside the documented 60..1209600 range may fail the resource.
Traces: cloudcontrol.deploy-cfn-update-reflected-in-getresource (the create, the Outputs QueueUrl and the pre-update GetResource of 345600 all pass; the check errors on the stack_update_complete waiter, so the post-update GetResource == 1209600, the DELETE_COMPLETE assertion and the post-delete ResourceNotFoundException/"...was not found." message assertion that follow it must also pass once the update applies).

No open issue carries this check's replay key and the suite report does not mark it owned. Open issues about other CloudFormation update/rollback causes exist (e.g. 20261112T113237Z-cloudformation-2-net-new-causes-changing-aws-logs.md on AWS::Logs::LogStream replacement, 20261111T105111Z-cloudformation-kinesis-an-in-place-property-update-of.md on AWS::Kinesis::Stream in-place updates refused with NotImplemented prose); none of them names AWS::SQS::Queue or this check. If the Kinesis-style "in-place property update refused" cause shares a code path with this one, re-replay this check after that fix to confirm.

## Evidence
All quotes are from the replayed check (suite simparity-behavioral-aws-v1). Generated stack and queue names, hosts, ports and timestamps are per-run values and are not part of any requirement.

Check cloudcontrol.deploy-cfn-update-reflected-in-getresource [error]:
  Template (both revisions identical except the one integer): {"Resources": {"Queue": {"Type": "AWS::SQS::Queue", "Properties": {"QueueName": <name>, "MessageRetentionPeriod": <seconds>}}}, "Outputs": {"QueueUrl": {"Value": {"Ref": "Queue"}}}}
  Step 1 (passes): CreateStack with MessageRetentionPeriod=345600 -> the stack_create_complete waiter settles, DescribeStacks Outputs yields QueueUrl.
  Step 2 (passes): cc.get_resource(TypeName="AWS::SQS::Queue", Identifier=<QueueUrl>) -> Properties.MessageRetentionPeriod == 345600.
  Step 3 (DIVERGES): cfn.update_stack(StackName=<stack>, TemplateBody=<same template with MessageRetentionPeriod=1209600>) then cfn.get_waiter("stack_update_complete").wait(...).
    Expected (AWS): the stack reaches StackStatus "UPDATE_COMPLETE".
    Actual (emulator): the stack reaches StackStatus "UPDATE_ROLLBACK_COMPLETE"; raised exception:
      botocore.exceptions.WaiterError: Waiter StackUpdateComplete failed: Waiter encountered a terminal failure state: For expression "Stacks[].StackStatus" we matched expected path: "UPDATE_ROLLBACK_COMPLETE" at least once
  Not reached, and therefore not separately filed: props_after["MessageRetentionPeriod"] == 1209600; DeleteStack -> DELETE_COMPLETE; GetResource after deletion refused ResourceNotFoundException with message "AWS::SQS::Queue Handler returned status FAILED: Resource of type 'AWS::SQS::Queue' with identifier '<queue url>' was not found. (HandlerErrorCode: NotFound, RequestToken: <token>)" — in that message the only variable parts are the queue URL (from the request) and the RequestToken (minted per call); the rest is fixed text.
  Model citation for the legality of the change: sqs SetQueueAttributesRequest.Attributes — MessageRetentionPeriod "Valid values: An integer representing seconds, from 60 (1 minute) to 1,209,600 (14 days). Default: 345,600 (4 days)."

## Steps to Reproduce
Boot the emulator and point botocore at it: cfn = client("cloudformation"), cc = client("cloudcontrol").

1. template(n) = json.dumps({"Resources": {"Queue": {"Type": "AWS::SQS::Queue", "Properties": {"QueueName": q, "MessageRetentionPeriod": n}}}, "Outputs": {"QueueUrl": {"Value": {"Ref": "Queue"}}}}).
2. cfn.create_stack(StackName=s, TemplateBody=template(345600)); wait for stack_create_complete; read the QueueUrl output.
3. cc.get_resource(TypeName="AWS::SQS::Queue", Identifier=<QueueUrl>) -> Properties.MessageRetentionPeriod == 345600 (passes today).
4. cfn.update_stack(StackName=s, TemplateBody=template(1209600)); poll describe_stacks(StackName=s)["Stacks"][0]["StackStatus"] to a terminal value.
   Expected: "UPDATE_COMPLETE". Observed: "UPDATE_ROLLBACK_COMPLETE" (the stack_update_complete waiter raises WaiterError on that terminal failure state). Also inspect describe_stack_events for the Queue resource's UPDATE_FAILED ResourceStatusReason, which names the refusal the handler made.
5. After the fix, confirm the rest of the check: cc.get_resource(...) reports MessageRetentionPeriod == 1209600 and sqs.get_queue_attributes(QueueUrl=..., AttributeNames=["MessageRetentionPeriod"]) agrees; then cfn.delete_stack(StackName=s) reaches DELETE_COMPLETE and cc.get_resource on the same identifier is refused ResourceNotFoundException with the handler NotFound sentence above.
