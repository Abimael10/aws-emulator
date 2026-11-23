Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudwatch__s3__sns__sqs.cfn-changeset-include-nested-stacks-lists-child-resource-changes cloudformation__cloudwatch__s3__sns__sqs.cfn-rollback-trigger-alarm-reverts-update-and-restores-property
Issue-Key: cloudformation__cloudwatch__s3__sns__sqs.cfn-changeset-include-nested-stacks-lists-child-resource-changes|run#31|AssertionError|assert executed == "UPDATE_COMPLETE", f"the parent update ended in {executed!r}"
Issue-Key: cloudformation__cloudwatch__s3__sns__sqs.cfn-rollback-trigger-alarm-reverts-update-and-restores-property|run#27|AssertionError|assert control == "UPDATE_COMPLETE", (
Issue-Commit: 44d4304dbd56e0f97705f387ff3fc6793140eb84

# cloudformation__cloudwatch__s3__sns__sqs: 2 net-new causes — executing an IncludeNestedStacks parent change set ends UPDATE_ROLLBACK_FAILED, and an UpdateStack RollbackConfiguration with an empty RollbackTriggers list does not clear the previously configured trigger

> _Note: this issue was identified against the code base at commit `44d4304dbd56e0f97705f387ff3fc6793140eb84`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
TWO net-new root causes in CloudFormation's stack-update path, both exercised under the cloudformation__cloudwatch__s3__sns__sqs heading. A fixer must close BOTH causes before submitting.

Cause 1 of 2: executing a parent change set that was created with IncludeNestedStacks=True does not apply the previewed nested-stack change — the parent update fails AND its rollback fails, settling UPDATE_ROLLBACK_FAILED instead of UPDATE_COMPLETE. Failing check: cloudformation__cloudwatch__s3__sns__sqs.cfn-changeset-include-nested-stacks-lists-child-resource-changes.
AWS: Smithy (cloudformation) CreateChangeSetInput.IncludeNestedStacks — "Creates a change set for the all nested stacks specified in the template. The default behavior of this action is set to False. To include nested sets in a change set, specify True." The parent change set therefore carries, for each AWS::CloudFormation::Stack resource, a ResourceChange with a nested ChangeSetId, and ExecuteChangeSet applies exactly the previewed changes: the child stack is updated, the previewed Modify of the child's AWS::SQS::Queue VisibilityTimeout is applied in place (no replacement, same physical id), and the parent reaches StackStatus UPDATE_COMPLETE. Recorded behavior only for the status sequence: UPDATE_IN_PROGRESS -> UPDATE_COMPLETE_CLEANUP_IN_PROGRESS -> UPDATE_COMPLETE, with no UPDATE_ROLLBACK_* status and no failure reason. The previewed change here is a mutable SQS attribute: Smithy (sqs) SetQueueAttributesRequest.Attributes documents VisibilityTimeout as settable on a live queue (0..43200 seconds), so 30 -> 45 is a legal in-place modification.
Emulator: the change-set creation and the whole preview are correct (the nested ChangeSetId is surfaced, the nested change set is CREATE_COMPLETE, its single Change names LogicalResourceId 'ChildQueue' with Action 'Modify' — all of those assertions pass). ExecuteChangeSet is accepted and the parent stack then settles in the terminal failure status 'UPDATE_ROLLBACK_FAILED': the update of the child failed and the attempt to roll it back failed too, so the stack is left in a state from which only ContinueUpdateRollback can recover and the child queue keeps the pre-update VisibilityTimeout. Hypothesis (the emulator's reason is not in the check output, which asserts on the status): the executor cannot drive a nested AWS::CloudFormation::Stack child through an update, and the compensating rollback of the half-applied nested change hits the same gap.
Why: nested stacks plus IncludeNestedStacks is how every CDK/SAM-style parent-child estate previews and lands a child change. A client is shown an accurate preview and then gets a failed deployment, and because the terminal state is UPDATE_ROLLBACK_FAILED rather than a clean rollback the stack is wedged — subsequent UpdateStack calls are refused until the operator runs ContinueUpdateRollback, which is far worse than either success or a clean revert.
Fix: the CloudFormation change-set execution path for AWS::CloudFormation::Stack resources — the step that, for each nested ChangeSetId in the parent plan, executes the child change set and waits for the child stack, plus the update-rollback path that must be able to undo a partially applied nested change so the stack can never land in UPDATE_ROLLBACK_FAILED for a change it previewed as a plain Modify.
Traces: cloudformation__cloudwatch__s3__sns__sqs.cfn-changeset-include-nested-stacks-lists-child-resource-changes (the create, the child's VisibilityTimeout=30 read at SQS, the parent change set, the nested ChangeSetId and the previewed Modify all pass; after the fix the remaining assertions — the child queue reads '45' and its physical id is unchanged — must also pass).

Cause 2 of 2: an UpdateStack whose RollbackConfiguration carries an EMPTY RollbackTriggers list does not clear the trigger the previous update configured, so an unrelated alarm still in ALARM rolls the new update back — the stack settles UPDATE_ROLLBACK_COMPLETE instead of UPDATE_COMPLETE. Failing check: cloudformation__cloudwatch__s3__sns__sqs.cfn-rollback-trigger-alarm-reverts-update-and-restores-property.
AWS: Smithy (cloudformation) RollbackConfiguration.RollbackTriggers documents the rule verbatim: "By default, CloudFormation saves the rollback triggers specified for a stack and applies them to any subsequent update operations for the stack, unless you specify otherwise. If you do specify rollback triggers for this parameter, those triggers replace any list of triggers previously specified for the stack... To remove all currently specified triggers, specify an empty list for this parameter." So UpdateStack(RollbackConfiguration={"RollbackTriggers": []}) removes every trigger: the identical property change that the previous, trigger-monitored update rolled back must now complete, the stack reaches UPDATE_COMPLETE and the new value stays applied — regardless of the alarm still being in ALARM. (RollbackTrigger is modelled {Arn @required, Type @required}, RollbackTriggers @length(0..5), so an empty list is a legal value and means exactly 'no triggers'.)
Emulator: the trigger-monitored leg is right — the first update ends UPDATE_ROLLBACK_COMPLETE, the failure text names the alarm, the queue property and the stack's recorded Parameters are reverted (all those assertions pass). The control update that follows, identical except for RollbackConfiguration={"RollbackTriggers": []}, is ALSO rolled back: AssertionError "without a trigger the identical update must succeed, got 'UPDATE_ROLLBACK_COMPLETE'". Hypothesis: the empty list is treated as 'not specified' (falsy/omitted) rather than as 'clear', so the stack keeps the previous update's RollbackTriggers and re-evaluates the alarm, which is still in ALARM. Note the emulator CAN apply this in-place VisibilityTimeout change (open issue 20261119T002136Z-cloudformation-cloudwatch-sqs-a-rollback-trigger-alarm-that.md records the same template reaching UPDATE_COMPLETE), so the rollback is attributable to the retained trigger and not to the property change failing.
Why: an empty RollbackTriggers list is the documented, and only, way to stop monitoring an alarm for later updates. A client that armed a canary alarm once can never deploy again while that alarm is unhealthy: every subsequent update is reverted and reported as a legitimate rollback, so a pipeline sees a valid change as a broken one and the stack is permanently pinned to the pre-trigger configuration.
Fix: the CloudFormation UpdateStack handler's RollbackConfiguration merge — distinguish 'member absent' (inherit the stack's stored triggers) from 'member present with an empty list' (replace the stored triggers with none), persist the cleared configuration on the stack, and skip trigger evaluation entirely when the effective trigger list is empty.
Traces: cloudformation__cloudwatch__s3__sns__sqs.cfn-rollback-trigger-alarm-reverts-update-and-restores-property (the alarm setup, the create, the triggered rollback, its alarm-naming reason and the reverted property/parameter all pass; the check fails on the control update and does not reach the final assertion that the queue reads '45').

Neither check is marked ALREADY FILED or OWNED in the suite report, and no open issue carries their replay keys. Related but distinct open issues: 20261119T002136Z-cloudformation-cloudwatch-sqs-a-rollback-trigger-alarm-that.md (triggers are not monitored for MonitoringTimeInMinutes AFTER the resources deploy — the opposite direction of Cause 2 and a different heading) and 20261119T062349Z-cloudformation-2-net-new-causes-a-cascading-replacement.md (a cascading replacement rolls an update back); neither names IncludeNestedStacks execution or an empty RollbackTriggers list.

## Evidence
All quotes are from the replayed behavioral suite (simparity-behavioral-aws-v1). Stack/queue/alarm names, hosts, ports and timestamps are per-run values and are not part of any requirement.

Cause 1 — cloudformation__cloudwatch__s3__sns__sqs.cfn-changeset-include-nested-stacks-lists-child-resource-changes [mismatch]:
  Parent template: {"Parameters": {"ChildTemplateUrl": ..., "ChildQueueName": ...}, "Resources": {"Child": {"Type": "AWS::CloudFormation::Stack", "Properties": {"TemplateURL": {"Ref": "ChildTemplateUrl"}, "Parameters": {"QueueName": {"Ref": "ChildQueueName"}}}}}}
  Child template v1/v2: one AWS::SQS::Queue with VisibilityTimeout 30 (v1) / 45 (v2).
  Passing steps: CreateStack -> CREATE_COMPLETE; the child queue reports VisibilityTimeout '30' at SQS; CreateChangeSet(ChangeSetType="UPDATE", UsePreviousTemplate=True, IncludeNestedStacks=True, ChildTemplateUrl=v2) -> CREATE_COMPLETE; exactly one nested ChangeSetId on the AWS::CloudFormation::Stack change; the nested change set is CREATE_COMPLETE with Changes == ['ChildQueue'] and Action == 'Modify'.
  DIVERGING step: cfn.execute_change_set(ChangeSetName=<cs id>), then poll describe_stacks for a terminal update status.
    Expected (AWS): StackStatus "UPDATE_COMPLETE", then the child queue reads VisibilityTimeout '45' with an unchanged physical id.
    Actual (emulator): AssertionError: the parent update ended in 'UPDATE_ROLLBACK_FAILED'

Cause 2 — cloudformation__cloudwatch__s3__sns__sqs.cfn-rollback-trigger-alarm-reverts-update-and-restores-property [mismatch]:
  Setup: PutMetricAlarm(TreatMissingData="breaching") + SetAlarmState(ALARM); CreateStack of one AWS::SQS::Queue with VisibilityTimeout from a Timeout parameter (30) -> CREATE_COMPLETE.
  Passing steps: UpdateStack(Timeout=45, RollbackConfiguration={"RollbackTriggers": [{"Arn": <alarm arn>, "Type": "AWS::CloudWatch::Alarm"}], "MonitoringTimeInMinutes": 1}) -> UPDATE_ROLLBACK_COMPLETE; the failure text contains "alarm" and the alarm name; the queue reads VisibilityTimeout '30' again and DescribeStacks reports Timeout '30'.
  DIVERGING step: UpdateStack(Timeout=45, RollbackConfiguration={"RollbackTriggers": []}).
    Expected (AWS): StackStatus "UPDATE_COMPLETE" (the empty list removes every trigger per RollbackConfiguration.RollbackTriggers), and the queue then reads VisibilityTimeout '45'.
    Actual (emulator): AssertionError: without a trigger the identical update must succeed, got 'UPDATE_ROLLBACK_COMPLETE'; the first rollback would otherwise be explained by an invalid update rather than by the alarm
  Model citation: cloudformation RollbackConfiguration.RollbackTriggers — "To remove all currently specified triggers, specify an empty list for this parameter."

## Steps to Reproduce
Boot the emulator and point botocore at it: cfn = client("cloudformation"), cw = client("cloudwatch"), sqs = client("sqs"), s3 = client("s3").

Cause 1 (IncludeNestedStacks execution):
1. Upload two child templates to S3; each declares one AWS::SQS::Queue with QueueName from a parameter and VisibilityTimeout 30 (v1) and 45 (v2), plus an Output of Ref ChildQueue.
2. create_stack with a parent template whose only resource is an AWS::CloudFormation::Stack pointing at the v1 URL; wait for CREATE_COMPLETE; confirm get_queue_attributes reports VisibilityTimeout '30'.
3. create_change_set(StackName=<parent>, ChangeSetType="UPDATE", UsePreviousTemplate=True, IncludeNestedStacks=True, Parameters=[ChildTemplateUrl=<v2 url>, ChildQueueName=<same name>]); wait CREATE_COMPLETE and note the nested ChangeSetId and its previewed Modify of ChildQueue (all correct today).
4. execute_change_set(ChangeSetName=<id>) and poll describe_stacks(StackName=<parent>)["Stacks"][0]["StackStatus"] to a terminal value.
   Expected: "UPDATE_COMPLETE". Observed: "UPDATE_ROLLBACK_FAILED". Read describe_stack_events for the Child / ChildQueue rows to see the failure the executor recorded.
5. After the fix: the child queue reports VisibilityTimeout '45' and its PhysicalResourceId is unchanged.

Cause 2 (empty RollbackTriggers):
1. put_metric_alarm(AlarmName=A, ..., TreatMissingData="breaching") and set_alarm_state(A, "ALARM"); read its AlarmArn.
2. create_stack with one AWS::SQS::Queue whose VisibilityTimeout is a Number parameter (30); wait CREATE_COMPLETE.
3. update_stack(Timeout=45, RollbackConfiguration={"RollbackTriggers": [{"Arn": <A arn>, "Type": "AWS::CloudWatch::Alarm"}], "MonitoringTimeInMinutes": 1}); it settles UPDATE_ROLLBACK_COMPLETE and reverts to 30 (correct today).
4. update_stack with the SAME Timeout=45 and RollbackConfiguration={"RollbackTriggers": []}; poll for the terminal status.
   Expected: "UPDATE_COMPLETE" and get_queue_attributes VisibilityTimeout '45'. Observed: "UPDATE_ROLLBACK_COMPLETE" with the value still 30.
5. Also check describe_stacks(StackName=<stack>)["Stacks"][0]["RollbackConfiguration"] after step 4: it must report an empty (or absent) RollbackTriggers list rather than the trigger from step 3.
