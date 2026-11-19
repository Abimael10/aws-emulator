Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__dynamodb__iam__sqs.cdk-bootstrap-custom-permissions-boundary-blocks-a-deploy-outside-it
Issue-Key: cloudformation__dynamodb__iam__sqs.cdk-bootstrap-custom-permissions-boundary-blocks-a-deploy-outside-it|run#34|AssertionError|assert rc == 1, (
Issue-Commit: 93aea946e1be9d371a5c94c0146ea55c9b1afa4f

# cloudformation__dynamodb__iam__sqs: the permissions boundary on the CDK cfn-exec role is not enforced while CloudFormation provisions, so a deploy the boundary denies succeeds

> _Note: this issue was identified against the code base at commit `93aea946e1be9d371a5c94c0146ea55c9b1afa4f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: CloudFormation provisions resources as the cfn-exec role without evaluating that role's permissions boundary (or its identity policies), so a deploy the boundary forbids reaches CREATE_COMPLETE.
AWS: recorded behavior only. After `cdk bootstrap --custom-permissions-boundary <policy>`, the toolkit's cfn-exec role carries that policy as its PermissionsBoundary. A `cdk deploy` of an app whose only resource is outside the boundary (an AWS::SQS::Queue, with the boundary allowing only the other service) fails: CloudFormation's create of that resource is refused AccessDenied by the boundary, the resource reaches CREATE_FAILED with a resource-handler reason of the form `Resource handler returned message: "<AccessDenied message> (Service: Sqs, Status Code: 403, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: AccessDenied)`, the stack rolls back and the CDK CLI exits 1. The boundary is a ceiling: an action it does not allow is denied even when the role's identity policy allows it.
Emulator: the same deploy succeeds. The stack events show `CREATE_IN_PROGRESS AWS::SQS::Queue Queue (Queue4A7E3555)` then `CREATE_COMPLETE AWS::SQS::Queue` and `CREATE_COMPLETE AWS::CloudFormation::Stack`, the queue URL is published as a stack output, and `cdk deploy` exits 0 (the check asserts exit 1). No authorization is applied to the resource create performed on behalf of the exec role.
Why: a permissions boundary is a security control. A client that bounds its deploy role to a service subset gets no enforcement at all: stacks that must be refused are provisioned, so the guard rail reads as effective while it is inert, and a negative test of a least-privilege deployment can never be written against the emulator.
Fix: the authorization path of CloudFormation's provisioning engine — each resource create/update/delete performed under the stack's RoleARN (here the cdk cfn-exec role) must be evaluated against that principal's identity policies AND its permissions boundary, and a denial must surface as the resource's AccessDenied CREATE_FAILED reason and roll the stack back.
Related but not covering: open issue 20261109T163411Z-cloudformation-dynamodb-iam-sqs-cdk-bootstrap-custom-permissions-boundary-leaves-the-synthesized.md reports the synthesized cfn-exec role carrying NO PermissionsBoundary. In this run the boundary is attached and visible to IAM (the sibling toolkit checks read it back on the role), so what remains is that nothing enforces it during provisioning — a distinct cause.

## Evidence
Check cloudformation__dynamodb__iam__sqs.cdk-bootstrap-custom-permissions-boundary-blocks-a-deploy-outside-it:
  expected: `cdk deploy` of the queue app exits 1 (the queue create refused AccessDenied by the exec role's permissions boundary, stack rolled back)
  actual:   AssertionError: `cdk deploy` of a queue the boundary denies exited 0, expected 1 (refused). Output tail shows:
            'CREATE_IN_PROGRESS  AWS::SQS::Queue  Queue (Queue4A7E3555) Resource creation Initiated'
            'CREATE_COMPLETE    AWS::SQS::Queue  Queue (Queue4A7E3555)'
            'CREATE_COMPLETE    AWS::CloudFormation::Stack  sim2real-cdkbndqs-<id>'
            '✅  sim2real-cdkbndqs-<id>' ... 'Outputs: ...QueueUrl = https://sqs.<region>.amazonaws.com/<account>/sim2real-cdkbndq-<id>'

## Steps to Reproduce
1. Create an IAM managed policy that allows only the non-SQS service of the app (e.g. dynamodb) and run `cdk bootstrap --qualifier <q> --custom-permissions-boundary <policy-name>`; confirm the cfn-exec role's PermissionsBoundary is that policy.
2. Synthesize a CDK app whose only resource is an AWS::SQS::Queue and run `cdk deploy` against that qualifier.
3. Emulator: the queue reaches CREATE_COMPLETE, the stack CREATE_COMPLETE and the CLI exits 0. AWS: the queue create is refused AccessDenied by the boundary, the stack rolls back and the CLI exits 1.
