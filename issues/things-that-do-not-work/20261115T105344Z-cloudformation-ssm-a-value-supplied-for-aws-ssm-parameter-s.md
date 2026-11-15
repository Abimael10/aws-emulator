Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ssm.cfn-ssm-parameter-readonly-arn-ignored-and-ref-getatt-match-ssm
Issue-Key: cloudformation__ssm.cfn-ssm-parameter-readonly-arn-ignored-and-ref-getatt-match-ssm|run#9|AssertionError|assert status == "CREATE_COMPLETE", (
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# A value supplied for AWS::SSM::Parameter's read-only Arn property fails the resource create instead of being ignored, rolling the stack back

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new cause.

Cause 1 of 1: a readOnlyProperty set in the template blocks the create of AWS::SSM::Parameter.

AWS: recorded behavior only — a template that sets AWS::SSM::Parameter's only read-only property, `Arn`, to a value AWS never minted deploys exactly as if the property were absent. The stack reaches CREATE_COMPLETE; SSM's own GetParameter returns the parameter with ARN `arn:aws:ssm:<region>:<account-id>:parameter/<parameter-name>` (built from the request's Name and the calling account/region, never from the template's value), Type and Value as declared, and the declared Tags; `Ref` on the resource is the parameter's Name and `Fn::GetAtt Arn` is that same SSM-minted ARN. Deleting the stack removes the parameter, and GetParameter then answers ParameterNotFound. No modelled refusal applies: the read-only value is dropped, not validated.

Emulator: the create is accepted by CreateStack (a StackId is returned) and then the Param resource create fails and the stack settles `ROLLBACK_COMPLETE`, so none of the read-backs above can be performed and the parameter never exists.

Why: every tool that round-trips a resource's description back into a template — `cloudformation get-template`/generated templates, drift workflows, CDK/Cloud Control imports, and any "describe then redeploy" script — emits read-only attributes alongside the writable ones. Against the emulator each such template fails the resource and rolls the stack back, where AWS deploys it; the client sees a total deployment failure for a property AWS ignores.

Fix: the AWS::SSM::Parameter resource handler's create path (and the template-property validation in front of it) — properties the resource type marks read-only must be stripped before the underlying PutParameter rather than failing the resource; the Arn returned by Ref/GetAtt must be the one SSM mints. The equivalent read-only-ignored behavior for AWS::Logs::LogGroup and AWS::SQS::Queue is already reported in open issues 20261109T143515Z-cloudformation-logs-10-net-new-causes-in-template.md and 20261109T185037Z-cloudformation-sqs-6-net-new-causes-aws-sqs-queue-read.md; neither covers AWS::SSM::Parameter, and the two open cloudformation__ssm issues (20261113T232314Z-cloudformation-ssm-2-net-new-causes-a-cfn.md, 20261109T142303Z-cloudformation-ssm-5-net-new-causes-no-resource.md) carry unrelated causes.

Traces: cloudformation__ssm.cfn-ssm-parameter-readonly-arn-ignored-and-ref-getatt-match-ssm

## Evidence
cloudformation__ssm.cfn-ssm-parameter-readonly-arn-ignored-and-ref-getatt-match-ssm — AssertionError: a readOnlyProperty value in the template must not block the create; got 'ROLLBACK_COMPLETE'

Template resource: AWS::SSM::Parameter {Name: <name>, Type: String, Value: <value>, Tags: {sim2real: <check>}, Arn: arn:aws:ssm:<region>:111122223333:parameter/not-the-real-parameter} with Outputs {Ref Param, Fn::GetAtt [Param, Arn]}.
Expected (AWS): stack CREATE_COMPLETE; GetParameter(<name>).ARN == arn:aws:ssm:<region>:<account-id>:parameter/<name>; (Type, Value) == (String, <value>); own tags == the declared Tags; Ref == <name>; GetAtt Arn == the SSM ARN; after DeleteStack, GetParameter -> ParameterNotFound.
Actual (emulator): stack ROLLBACK_COMPLETE — the parameter is never created, so none of the read-backs are reachable.

## Steps to Reproduce
1. CreateStack with a template whose only resource is AWS::SSM::Parameter {Name, Type: String, Value, Tags} plus the read-only property `Arn` set to any ARN the emulator did not mint (e.g. arn:aws:ssm:<region>:111122223333:parameter/not-the-real-parameter), and Outputs exporting {Ref: Param} and {Fn::GetAtt: [Param, Arn]}.
2. Poll DescribeStacks until terminal — the stack settles ROLLBACK_COMPLETE instead of CREATE_COMPLETE.
3. DescribeStackEvents and read the Param CREATE_FAILED row's ResourceStatusReason to see the refusal the handler produced for the read-only property.
4. Re-replay cloudformation__ssm.cfn-ssm-parameter-readonly-arn-ignored-and-ref-getatt-match-ssm.
