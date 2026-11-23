Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__cloudformation.deploy-asg-warmpool-poolstate-invalid-enum-rejected
Issue-Key: autoscaling__cloudformation.deploy-asg-warmpool-poolstate-invalid-enum-rejected|run#21|AssertionError|assert got == _REJECTED, f"handler reason {got!r}, expected {_REJECTED!r}"
Issue-Commit: 26b85ab073b263b4ee174e2a21b18d29a50ca66d

# autoscaling__cloudformation: the CREATE_FAILED handler reason for a refused AWS::AutoScaling::WarmPool drops AWS's fixed 'Invalid request provided: ' prefix

> _Note: this issue was identified against the code base at commit `26b85ab073b263b4ee174e2a21b18d29a50ca66d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new root cause on the CloudFormation resource-handler error relay for AWS::AutoScaling::WarmPool.

Cause 1 of 1: the relayed resource-handler message omits the fixed text 'Invalid request provided: ' that AWS prepends when a registry handler maps a downstream SDK refusal to HandlerErrorCode InvalidRequest.
Failing check: autoscaling__cloudformation.deploy-asg-warmpool-poolstate-invalid-enum-rejected
AWS: Smithy (com.amazonaws.autoscaling#PutWarmPool): PoolState targets the WarmPoolState enum, whose only values are Stopped, Running, Hibernated, so a template PoolState of 'Terminated' is refused by AutoScaling with HTTP 400 and the awsQuery common ValidationError. CloudFormation records the WarmPool resource's CREATE_FAILED ResourceStatusReason as 'Resource handler returned message: "<message>" (RequestToken: <uuid>, HandlerErrorCode: <code>)'. Recorded behavior for <message> on this resource: the fixed text 'Invalid request provided: ', then the refusing service's own sentence '1 validation error detected: Value '<rejected value>' at 'poolState' failed to satisfy constraint: Member must satisfy enum value set: [Stopped, Hibernated, Running]', then the fixed SDK envelope ' (Service: AutoScaling, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)'. Fixed text: 'Invalid request provided: ', '1 validation error detected: Value ', " at 'poolState' failed to satisfy constraint: Member must satisfy enum value set: [Stopped, Hibernated, Running]", ' (Service: AutoScaling, Status Code: 400, Request ID: ', ') (SDK Attempt Count: ', ')'. Variables: the rejected PoolState value comes from the template; the request id and attempt count are per-run values that must merely be present (the check masks them) and are never compared literally. HandlerErrorCode is 'InvalidRequest'; the stack ends ROLLBACK_COMPLETE and no warm pool or ASG survives.
Emulator: the refusal, the rollback, the SDK envelope and the HandlerErrorCode are all correct, but the relayed message starts at the service sentence — '1 validation error detected: Value 'Terminated' at 'poolState' failed to satisfy constraint: Member must satisfy enum value set: [Stopped, Hibernated, Running] (Service: AutoScaling, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)' — with no leading 'Invalid request provided: '.
Why: a client (or CI) that parses the resource-handler reason to classify the failure cannot tell an InvalidRequest relay from a raw service message, and any assertion or log pattern written against AWS's reason text does not match, even though the failure itself was reported correctly.
Fix: the CloudFormation resource-provider error relay that formats a downstream AutoScaling SDK error into the handler message for the AWS::AutoScaling::WarmPool handler — prepend AWS's fixed 'Invalid request provided: ' when the mapped HandlerErrorCode is InvalidRequest; the PoolState enum validation itself is already correct. The same envelope family is described for other resource types in the open issues 20261118T201606Z-cloudformation-cloudfront-the-create-failed-handler-reason-for.md and 20261113T130916Z-autoscaling-cloudformation-a-create-complete-stack-declaring-aws.md; neither carries this check's replay key, so this failure is not covered by them.
Traces: autoscaling__cloudformation.deploy-asg-warmpool-poolstate-invalid-enum-rejected (assert at check line 250: 'handler reason ... expected ...').

## Evidence
Check [mismatch] autoscaling__cloudformation.deploy-asg-warmpool-poolstate-invalid-enum-rejected
  AssertionError: handler reason ("1 validation error detected: Value 'Terminated' at 'poolState' failed to satisfy constraint: Member must satisfy enum value set: [Stopped, Hibernated, Running] (Service: AutoScaling, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)", 'InvalidRequest'), expected ("Invalid request provided: 1 validation error detected: Value 'Terminated' at 'poolState' failed to satisfy constraint: Member must satisfy enum value set: [Stopped, Hibernated, Running] (Service: AutoScaling, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)", 'InvalidRequest')

The check parses the WarmPool resource's CREATE_FAILED ResourceStatusReason with '^Resource handler returned message: "(?P<message>.*)" \\(RequestToken: [0-9a-f-]+, HandlerErrorCode: (?P<code>[A-Za-z]+)\\)$' and compares (masked message, code). Everything else already matches: the positive control stack with PoolState 'Stopped' reached CREATE_COMPLETE with DescribeWarmPool reading back ('Stopped', 0, 0), the bad stack reached ROLLBACK_COMPLETE, no early PROPERTY_VALIDATION rows were emitted, and HandlerErrorCode is 'InvalidRequest'. The only divergence is the missing fixed head 'Invalid request provided: '. Request ID and SDK Attempt Count are masked to <id>/<n> by the check and are not part of the requirement.

## Steps to Reproduce
1. CreateStack with a template declaring AWS::EC2::LaunchTemplate, AWS::AutoScaling::AutoScalingGroup (MinSize 0, MaxSize 1, DesiredCapacity 0, VPCZoneIdentifier=<subnet>) and AWS::AutoScaling::WarmPool {AutoScalingGroupName: {Ref: ASG}, MinSize: 0, MaxGroupPreparedCapacity: 0, PoolState: 'Terminated'} (a value outside the WarmPoolState enum). Keep the same template with PoolState 'Stopped' as the positive control.
2. Poll DescribeStacks by StackId until terminal. The bad stack reaches ROLLBACK_COMPLETE (correct).
3. DescribeStackEvents and read the WarmPool resource's single CREATE_FAILED ResourceStatusReason; parse it as 'Resource handler returned message: "<message>" (RequestToken: <uuid>, HandlerErrorCode: <code>)'.
4. Observed <message> begins '1 validation error detected: Value ...'. Expected <message> begins 'Invalid request provided: 1 validation error detected: Value ...', with the rest (service sentence and ' (Service: AutoScaling, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)') unchanged.
5. Re-replay autoscaling__cloudformation.deploy-asg-warmpool-poolstate-invalid-enum-rejected to confirm.
