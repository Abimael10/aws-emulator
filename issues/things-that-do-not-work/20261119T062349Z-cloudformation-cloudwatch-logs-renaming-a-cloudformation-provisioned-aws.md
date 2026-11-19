Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudwatch__logs.deploy-logs-3-invalid-filter-pattern-rejected-rolls-back
Issue-Key: cloudformation__cloudwatch__logs.deploy-logs-3-invalid-filter-pattern-rejected-rolls-back|run#65|AssertionError|assert ren_status == "UPDATE_COMPLETE", f"rename update ended {ren_status!r}"
Issue-Commit: 93aea946e1be9d371a5c94c0146ea55c9b1afa4f

# cloudformation__cloudwatch__logs: renaming a CloudFormation-provisioned AWS::Logs::LogGroup rolls the stack update back instead of replacing the group

> _Note: this issue was identified against the code base at commit `93aea946e1be9d371a5c94c0146ea55c9b1afa4f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Cause 1 of 1: a replacing update of AWS::Logs::LogGroup (LogGroupName changed) fails and rolls back.
AWS: recorded behavior only — LogGroupName is a create-only property of AWS::Logs::LogGroup, so changing it is a replacement: CloudFormation creates the new log group, re-creates the dependent AWS::Logs::LogStream and AWS::Logs::MetricFilter against it, deletes the old group, and the stack reaches UPDATE_COMPLETE. After it, the resource's PhysicalResourceId is the new group name, DescribeLogGroups reports the renamed group with the unchanged retentionInDays (7), and the old group is gone.
Emulator: the same UpdateStack (same template, only the LogGroupName parameter changed) ends UPDATE_ROLLBACK_COMPLETE, so the rename never happens; the stack is left on the pre-update group. The in-place leg of the same check (RetentionInDays 3 -> 7, physical id retained, log data kept) passes, so only the replacement path of these log resources diverges.
Why: a client cannot rename a log group through the stack it owns — the update is reported as failed/rolled back even though the template change is legal, and automation that renames log resources (and the dependent metric filters/streams) can never converge.
Fix: the replacement path of the AWS::Logs::LogGroup resource handler and the re-creation ordering of its dependent AWS::Logs::LogStream / AWS::Logs::MetricFilter in CloudFormation's update engine (create new, re-point dependents, delete old).

## Evidence
Check cloudformation__cloudwatch__logs.deploy-logs-3-invalid-filter-pattern-rejected-rolls-back, step 4 (replacement):
  cfn.update_stack(StackName=<stack>, UsePreviousTemplate=True, Parameters=[... LogGroupName=<renamed group> ...])
  expected: stack status 'UPDATE_COMPLETE', LogGroup PhysicalResourceId == <renamed group>, DescribeLogGroups(<renamed group>).retentionInDays == 7, old group absent
  actual:   AssertionError: rename update ended 'UPDATE_ROLLBACK_COMPLETE'
The earlier legs of the same check (invalid metric-filter pattern rejected and rolled back; in-place RetentionInDays update keeping the physical id and the two log events) passed.

## Steps to Reproduce
Deploy a stack whose resources are, strictly ordered, AWS::Logs::LogGroup (LogGroupName from a parameter, RetentionInDays 3) -> AWS::Logs::LogStream -> AWS::Logs::MetricFilter (valid pattern '{ $.latency > 0 }'). Wait for CREATE_COMPLETE. UpdateStack with RetentionInDays 7 (passes, in place). Then UpdateStack again changing only the LogGroupName parameter to a new name and poll DescribeStacks: the emulator reaches UPDATE_ROLLBACK_COMPLETE; AWS reaches UPDATE_COMPLETE with the new group present (retentionInDays 7) and the old one deleted.
