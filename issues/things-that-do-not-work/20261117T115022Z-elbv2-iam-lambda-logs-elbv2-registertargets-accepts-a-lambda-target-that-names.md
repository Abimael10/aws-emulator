Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elbv2__iam__lambda__logs.lambda-target-group-rejects-explicit-port
Issue-Key: elbv2__iam__lambda__logs.lambda-target-group-rejects-explicit-port|_expect_error#11|AssertionError|raise AssertionError(f"expected ({code}, {message!r}) but the call succeeded: {result!r}")
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# ELBv2 RegisterTargets accepts a Lambda target that names an explicit Port instead of refusing it with ValidationError

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterTargets

## Description
Cause 1 of 1: a lambda-type target group accepts (and stores) a target description carrying a Port.
AWS: Smithy elasticloadbalancingv2 models RegisterTargets with TargetDescription, where Port is meaningful only for instance/ip targets; for a target group whose TargetType is lambda the service refuses any target naming a Port with the query-protocol error code ValidationError and the message 'Port cannot be specified for Lambda function targets' (fixed text, no variable parts). The same refusal applies to DeregisterTargets and DescribeTargetHealth, and nothing is stored by the refused call; the identical calls with no Port succeed.
Emulator: RegisterTargets answers HTTP 200 with an empty body — the Port is accepted and the registration is stored.
Why: this is an overly-permissive accept: a client that registers a Lambda target with a Port is told it worked, so a configuration AWS rejects is carried silently into the target group, and the three refusals the scenario checks (register/deregister/describe-health) never happen.
Fix: the RegisterTargets/DeregisterTargets/DescribeTargetHealth handlers' target validation for lambda-type target groups (refuse a TargetDescription with Port, ValidationError).
Failing check: elbv2__iam__lambda__logs.lambda-target-group-rejects-explicit-port

## Evidence
[mismatch] elbv2__iam__lambda__logs.lambda-target-group-rejects-explicit-port — AssertionError: expected (ValidationError, 'Port cannot be specified for Lambda function targets') but the call succeeded: {'ResponseMetadata': {... 'HTTPStatusCode': 200 ...}}.

## Steps to Reproduce
1. Create a Lambda function and a TargetType=lambda target group; grant the ALB invoke permission.
2. Call RegisterTargets(TargetGroupArn=<tg>, Targets=[{'Id': <function arn>, 'Port': 80}]) — expect ValidationError 'Port cannot be specified for Lambda function targets'; repeat for DeregisterTargets and DescribeTargetHealth, then confirm the same calls without Port succeed.
