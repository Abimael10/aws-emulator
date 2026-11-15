Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.template-wait-condition-failure-signal-fails-create cloudformation.deep-cloudformation-3-cfn-flight-stackset-tolerance-one-absorbs-region1-collision-and-deploys-region2
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.template-wait-condition-failure-signal-fails-create|_settle#9|Timeout|time.sleep(3.0)
Issue-Key: cloudformation.deep-cloudformation-3-cfn-flight-stackset-tolerance-one-absorbs-region1-collision-and-deploys-region2|run#25|AssertionError|assert conflicts == [(
Issue-Commit: d3cdd55e8b72332e5c71fd34997111c995fed5da

# cloudformation: 2 net-new causes — a FAILURE wait-condition signal never fails the create, and DescribeEvents reports no VALIDATION_ERROR for a stack-set instance that failed on a name conflict

> _Note: this issue was identified against the code base at commit `d3cdd55e8b72332e5c71fd34997111c995fed5da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEvents

## Description
Two net-new root causes in CloudFormation.

Cause 1 of 2: a wait condition that receives a FAILURE signal never fails the stack create, so the whole trajectory never reaches a terminal status.
AWS: recorded behavior only (wait conditions are not modelled in Smithy). A stack with an AWS::CloudFormation::WaitConditionHandle and an AWS::CloudFormation::WaitCondition of Count 2 that is PUT one SUCCESS and one FAILURE signal to the handle's presigned URL fails the create: the Wait resource emits CREATE_FAILED with ResourceStatusReason exactly "WaitCondition received failed message: '<Reason from the signal>' for uniqueId: <UniqueId from the signal>" (the fixed text is "WaitCondition received failed message: '" ... "' for uniqueId: "; the reason text and the unique id come from the signal body), the stack emits ROLLBACK_IN_PROGRESS with reason 'The following resource(s) failed to create: [Wait]. Rollback requested by user.' and settles in ROLLBACK_COMPLETE — well inside a minute for a two-resource stack.
Emulator: the check reaches no verdict at all — it is cut off with 'Check timed out after 60.0s'. Nothing in the trajectory (handle creation to CREATE_COMPLETE, the two signal PUTs, then DescribeStacks polling for a non-*_IN_PROGRESS status) completes, so either the WaitConditionHandle never reaches CREATE_COMPLETE or the signalled wait condition never resolves and the stack stays CREATE_IN_PROGRESS.
Why: every bootstrapping pattern that gates a stack on an out-of-band signal (cfn-signal from user data, a custom orchestrator) hangs forever instead of failing fast, and the documented failure reason a client parses is never produced.
Fix: the wait-condition state machine in the CloudFormation provisioner — the handle's presigned signal endpoint and the WaitCondition resource that consumes Count signals and maps a FAILURE to CREATE_FAILED plus stack rollback.
Failing check: cloudformation.template-wait-condition-failure-signal-fails-create.

Cause 2 of 2: DescribeEvents returns no VALIDATION_ERROR operation event for a stack-set instance whose resource create failed on a name conflict.
AWS: recorded behavior only for the event payload. In a SELF_MANAGED two-region SEQUENTIAL fan-out with FailureToleranceCount=1 where region 1 holds a pre-existing SSM parameter of the template's name, the region-1 stack fails and CloudFormation's DescribeEvents (Filters={'FailedEvents': True}) returns an OperationEvents entry with EventType 'VALIDATION_ERROR', ValidationName 'NAME_CONFLICT_VALIDATION' and ValidationStatusReason "Resource of type 'AWS::SSM::Parameter' with identifier '<parameter name>' already exists." (the fixed text is "Resource of type '" + the resource type + "' with identifier '" + the colliding identifier + "' already exists.").
Emulator: everything up to the event read matches — the operation ends SUCCEEDED with FailedStackInstancesCount 1, the region-1 instance is OUTDATED/FAILED and carries a StackId — but DescribeEvents on that stack returns an empty list of VALIDATION_ERROR events, so the reason the instance failed is not reported anywhere.
Why: a client diagnosing a partially failed stack-set operation has no way to learn WHY the instance failed; DescribeEvents answers 200 with nothing, which is indistinguishable from a healthy stack.
Fix: the DescribeEvents handler and the event/validation-error state it reads in the CloudFormation service — a pre-provision name-conflict refusal must be recorded as a VALIDATION_ERROR operation event with ValidationName and ValidationStatusReason.
Failing check: cloudformation.deep-cloudformation-3-cfn-flight-stackset-tolerance-one-absorbs-region1-collision-and-deploys-region2.

## Evidence
Cause 1 — expected: the stack settles ROLLBACK_COMPLETE with the Wait resource CREATE_FAILED and reason "WaitCondition received failed message: '<reason>' for uniqueId: <id>".
Actual (cloudformation.template-wait-condition-failure-signal-fails-create): Error: Check timed out after 60.0s — the trajectory produced no terminal stack status at all.

Cause 2 — expected DescribeEvents(StackName=<region-1 instance stack id>, Filters={'FailedEvents': True}) to contain exactly [('NAME_CONFLICT_VALIDATION', "Resource of type 'AWS::SSM::Parameter' with identifier '<param name>' already exists.")].
Actual (cloudformation.deep-cloudformation-3-cfn-flight-stackset-tolerance-one-absorbs-region1-collision-and-deploys-region2): AssertionError: region-1 stack validation errors were [], expected exactly the planted name conflict.

## Steps to Reproduce
1. Replay the behavioral suite and re-run both checks in trace_ids.
2. Cause 1: CreateStack a template with AWS::CloudFormation::WaitConditionHandle 'Handle' and AWS::CloudFormation::WaitCondition 'Wait' (Handle=Ref Handle, Timeout 600, Count 2). Read the handle's PhysicalResourceId (the presigned URL) via DescribeStackResource. PUT {"Status":"SUCCESS",...} then {"Status":"FAILURE","Reason":"disk full on worker two","UniqueId":"bad-x",...} to it. Poll DescribeStacks for a terminal status and read DescribeStackEvents.
3. Cause 2: put an SSM parameter P in region 1. CreateStackSet (SELF_MANAGED) whose template creates AWS::SSM::Parameter named P, then CreateStackInstances for [region1, region2] with OperationPreferences RegionConcurrencyType=SEQUENTIAL, MaxConcurrentCount=1, FailureToleranceCount=1. After the operation reaches SUCCEEDED, take the region-1 instance's StackId and call DescribeEvents(StackName=<that stack id>, Filters={'FailedEvents': True}).
