Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__s3__stepfunctions.cfn-state-machine-definition-s3-location-missing-key-rejected cloudformation__s3__stepfunctions.cfn-state-machine-definition-s3-location-substitutions-round-trip-and-execution
Issue-Key: cloudformation__s3__stepfunctions.cfn-state-machine-definition-s3-location-missing-key-rejected|run#23|AssertionError|assert got == _MISSING_KEY_REASON, (
Issue-Key: cloudformation__s3__stepfunctions.cfn-state-machine-definition-s3-location-substitutions-round-trip-and-execution|run#19|WaiterError|cfn.get_waiter("stack_create_complete").wait(
Issue-Commit: 898fb31eb69e07df3cb7a6861bbbd7f63697428c

# cloudformation__s3__stepfunctions: AWS::StepFunctions::StateMachine DefinitionS3Location is never read from S3 — the resource is refused with an emulator-authored InvalidRequest instead of fetching the object

> _Note: this issue was identified against the code base at commit `898fb31eb69e07df3cb7a6861bbbd7f63697428c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: a state machine whose definition lives in S3 (DefinitionS3Location, with or without DefinitionSubstitutions) is never provisioned: CloudFormation refuses the resource with its own message rather than fetching the object, so both the negative path (missing key) and the positive path (object present, substitutions applied, execution runs) fail.

AWS: recorded behavior only for the exact text, taken from the recorded response. AWS::StepFunctions::StateMachine's DefinitionS3Location names Bucket/Key/Version; CloudFormation issues an S3 GetObject for it. When the key is absent, the resource goes CREATE_FAILED and its reason is S3's own failure relayed verbatim — ResourceStatusReason 'The specified key does not exist. (Service: S3, Status Code: 404, Request ID: <id>, Extended Request ID: <id>) (SDK Attempt Count: <n>)' with HandlerErrorCode 'GeneralServiceException'. Fixed text: 'The specified key does not exist. (Service: S3, Status Code: 404, Request ID: ', ', Extended Request ID: ', ') (SDK Attempt Count: ', ')'; the request id, extended request id and attempt count are per-call values from the S3 response, not literals. When the key IS present, the resource must be created, DefinitionSubstitutions applied to the fetched document, and the state machine must execute.

Emulator: the missing-key case answers CREATE_FAILED with reason ('this emulator does not apply the DefinitionS3Location property of AWS::StepFunctions::StateMachine', HandlerErrorCode 'InvalidRequest') — the property is declared unsupported rather than honoured. The positive case therefore cannot create at all: 'botocore.exceptions.WaiterError: Waiter StackCreateComplete failed: ... we matched expected path: "ROLLBACK_COMPLETE"'.

Why: every stack that keeps its ASL definition in S3 (the standard pattern for definitions over the inline size limit, and for CI-published definitions) fails to deploy. A client also cannot distinguish a genuine S3 404 from an unimplemented property, because the failure reason names the emulator instead of S3, so the usual remediation (upload the object) does not change the outcome.

Fix: the AWS::StepFunctions::StateMachine resource provisioner — it must resolve DefinitionS3Location via S3 GetObject, apply DefinitionSubstitutions to the fetched document, and relay an S3 failure as the resource's reason with HandlerErrorCode GeneralServiceException.

Traces: cloudformation__s3__stepfunctions.cfn-state-machine-definition-s3-location-missing-key-rejected, cloudformation__s3__stepfunctions.cfn-state-machine-definition-s3-location-substitutions-round-trip-and-execution.

## Evidence
Expected (missing key): MyStateMachine CREATE_FAILED with reason ('The specified key does not exist. (Service: S3, Status Code: 404, Request ID: <id>, Extended Request ID: <id>) (SDK Attempt Count: <n>)', 'GeneralServiceException').
Actual: ('this emulator does not apply the DefinitionS3Location property of AWS::StepFunctions::StateMachine', 'InvalidRequest').
Expected (object present): StackCreateComplete, substitutions applied, a successful execution.
Actual: 'botocore.exceptions.WaiterError: Waiter StackCreateComplete failed: Waiter encountered a terminal failure state: For expression "Stacks[].StackStatus" we matched expected path: "ROLLBACK_COMPLETE" at least once'.

## Steps to Reproduce
1. PutObject an ASL definition document (containing ${Substitution} placeholders) into a bucket.
2. CreateStack with an AWS::StepFunctions::StateMachine whose DefinitionS3Location names that Bucket/Key, plus DefinitionSubstitutions; wait on StackCreateComplete and start an execution.
3. Repeat with a Key that does not exist and read the resource's CREATE_FAILED ResourceStatusReason and HandlerErrorCode.
Observed: step 2 ends ROLLBACK_COMPLETE; step 3's reason names the emulator with HandlerErrorCode InvalidRequest instead of relaying S3's 404.
