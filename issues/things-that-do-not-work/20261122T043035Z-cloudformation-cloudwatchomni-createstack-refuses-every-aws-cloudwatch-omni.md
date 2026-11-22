Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudwatchomni__iam.deploy-cloudwatch-omni-estate-provisioned-and-torn-down
Issue-Key: cloudformation__cloudwatchomni|CreateStack|ValidationError
Issue-Commit: 622c214bd0a84106899ec4445dc3396f8892f16d

# cloudformation__cloudwatchomni: CreateStack refuses every AWS::CloudWatch Omni resource type as an unrecognized resource type

> _Note: this issue was identified against the code base at commit `622c214bd0a84106899ec4445dc3396f8892f16d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new root cause: CloudFormation's resource-type registry does not carry the CloudWatch Omni resource types, so a template declaring them is rejected at template-format validation and nothing is ever provisioned. Failing check: cloudformation__cloudwatchomni__iam.deploy-cloudwatch-omni-estate-provisioned-and-torn-down

AWS:      Smithy (cloudformation): CreateStack models the awsQuery common ValidationError; a template whose resource types are all registered is accepted and the stack settles CREATE_COMPLETE. AWS registers the CloudWatch Omni types AWS::CloudWatch::Domain, AWS::CloudWatch::Space, AWS::CloudWatch::AccessProfile, AWS::CloudWatch::AccessGrant, AWS::CloudWatch::Integration and AWS::CloudWatch::OmniDashboard, and the recording shows the whole estate provisioning through CloudFormation and then being torn down by the stack delete. 'Template format error: Unrecognized resource types: [...]' is the refusal AWS reserves for types that genuinely are not in the registry; it is not what AWS answers for these six.
Emulator: CreateStack answers HTTP 400 ValidationError with Message `Template format error: Unrecognized resource types: [AWS::CloudWatch::Domain, AWS::CloudWatch::Space, AWS::CloudWatch::AccessProfile, AWS::CloudWatch::AccessGrant, AWS::CloudWatch::Integration, AWS::CloudWatch::OmniDashboard]`, so the stack is never created and no Omni resource is ever reachable through CloudFormation.
Why:      every client that provisions CloudWatch Omni through infrastructure-as-code (CloudFormation, CDK, SAM, Serverless) fails at the very first call with a template-format error, and there is no path at all to create, read back or delete these resources via a stack. The failure is a hard refusal of the whole template, so even the unrelated resources in the same stack are never created.
Fix:      the CloudFormation resource-type registry (the table of recognized AWS::* types) plus the create/read/update/delete resource handlers for the six AWS::CloudWatch Omni types, wired to the cloudwatchomni service's state.
Traces:   cloudformation__cloudwatchomni__iam.deploy-cloudwatch-omni-estate-provisioned-and-torn-down — the check never gets past CreateStack.

## Evidence
Check cloudformation__cloudwatchomni__iam.deploy-cloudwatch-omni-estate-provisioned-and-torn-down [error]

botocore.exceptions.ClientError: An error occurred (ValidationError) when calling the CreateStack operation: Template format error: Unrecognized resource types: [AWS::CloudWatch::Domain, AWS::CloudWatch::Space, AWS::CloudWatch::AccessProfile, AWS::CloudWatch::AccessGrant, AWS::CloudWatch::Integration, AWS::CloudWatch::OmniDashboard]

expected: CreateStack accepted, the stack settles CREATE_COMPLETE, each Omni resource reads back through the cloudwatchomni API, and the stack delete removes them (DELETE_COMPLETE).
actual:   HTTP 400 ValidationError at CreateStack; no stack, no resources.

## Steps to Reproduce
1. CreateStack with a template declaring AWS::CloudWatch::Domain, AWS::CloudWatch::Space, AWS::CloudWatch::AccessProfile, AWS::CloudWatch::AccessGrant, AWS::CloudWatch::Integration and AWS::CloudWatch::OmniDashboard (plus the IAM role they reference).
2. The call is refused immediately with ValidationError 'Template format error: Unrecognized resource types: [...]' listing all six types.
3. AWS accepts the template, reaches CREATE_COMPLETE, and each resource is readable through the cloudwatchomni API until the stack is deleted.
