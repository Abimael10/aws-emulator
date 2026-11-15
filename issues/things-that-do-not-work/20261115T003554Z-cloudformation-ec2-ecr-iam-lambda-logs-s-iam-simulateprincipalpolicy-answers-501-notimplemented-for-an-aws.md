Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__ecr__iam__lambda__logs__s3__ssm.cdk-bootstrap-session-tags-unlock-lookup-deploy-and-asset-publishing
Issue-Key: iam|SimulatePrincipalPolicy|NotImplemented
Issue-Commit: a49cdab94fc41177458b6e53730040285f53d8d6

# IAM SimulatePrincipalPolicy answers 501 NotImplemented for an AWS-managed policy that is not in the emulator's catalog, so CDK bootstrap with session tags cannot be diagnosed or completed

> _Note: this issue was identified against the code base at commit `a49cdab94fc41177458b6e53730040285f53d8d6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SimulatePrincipalPolicy

## Description
One net-new root cause.

Cause 1 of 1: SimulatePrincipalPolicy refuses the whole call with HTTP 501 / error code NotImplemented whenever any attached AWS-managed policy ARN is absent from the emulator's internal catalog of managed policies.

AWS: Smithy (iam) models SimulatePrincipalPolicy with PolicySourceArn, ActionNames and ContextEntries, returning SimulatePolicyResponse (EvaluationResults). Its modeled errors are NoSuchEntityException, InvalidInputException and PolicyEvaluationException — there is no NotImplemented error and 501 is not a modeled status. AWS resolves every AWS-managed policy attached to the principal (here arn:aws:iam::aws:policy/AWSCloudFormationReadOnlyAccess) and returns an EvaluationResults entry per requested action with EvalDecision 'allowed' or 'implicitDeny'/'explicitDeny'. If a named policy genuinely did not exist AWS would answer NoSuchEntityException (HTTP 404), never 501.

Emulator: the call fails with botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the SimulatePrincipalPolicy operation: "AWS-managed policy arn:aws:iam::aws:policy/AWSCloudFormationReadOnlyAccess is not in this emulator's catalog of AWS-managed policies, so its document cannot be reported; the policy is not claimed to be absent". The ARN in that message is the one supplied by the request/state; the fixed part is the emulator-authored sentence and the NotImplemented code, neither of which AWS can produce for this operation.

Why: the whole simulation is refused rather than degraded, so a client cannot evaluate ANY action for a principal that happens to carry one unlisted AWS-managed policy. For CDK this is fatal: `cdk bootstrap`/`cdk deploy` with session tags uses the simulation to decide whether the bootstrap role may perform lookups, deploys and asset publishing, and a 501 is not a decision it can act on. A refusal code AWS never returns also defeats client retry/branch logic, which keys on NoSuchEntityException and InvalidInputException.

Fix: the SimulatePrincipalPolicy handler and the IAM managed-policy catalog it resolves against — the handler must resolve the principal's attached AWS-managed policies (extending the catalog, or evaluating the policies it can resolve) and return EvaluationResults, and must never answer NotImplemented/501 for a modeled operation.

Traces: cloudformation__ec2__ecr__iam__lambda__logs__s3__ssm.cdk-bootstrap-session-tags-unlock-lookup-deploy-and-asset-publishing.

## Evidence
Expected (AWS): HTTP 200 with SimulatePolicyResponse.EvaluationResults, one entry per ActionName, each carrying EvalActionName, EvalResourceName and EvalDecision; the only modeled failures are NoSuchEntityException (404), InvalidInputException (400) and PolicyEvaluationException (500).

Actual (emulator): botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the SimulatePrincipalPolicy operation: AWS-managed policy arn:aws:iam::aws:policy/AWSCloudFormationReadOnlyAccess is not in this emulator's catalog of AWS-managed policies, so its document cannot be reported; the policy is not claimed to be absent — HTTP 501, an unmodeled code, aborting the CDK bootstrap/lookup/deploy/asset-publishing flow.

## Steps to Reproduce
1. Create an IAM role and attach the AWS-managed policy arn:aws:iam::aws:policy/AWSCloudFormationReadOnlyAccess to it.
2. Call iam.SimulatePrincipalPolicy(PolicySourceArn=<that role ARN>, ActionNames=[...], ContextEntries=[... session tags ...]).
3. AWS returns EvaluationResults for every requested action. The emulator answers HTTP 501 with code NotImplemented and the catalog message above.
4. Equivalently, run the suite check: it performs the CDK bootstrap-with-session-tags flow and aborts at this call.
