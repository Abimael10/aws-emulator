Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__stepfunctions.deploy-pass-state-with-next-and-end-together-rejected-at-stack-create
Issue-Key: cloudformation__stepfunctions.deploy-pass-state-with-next-and-end-together-rejected-at-stack-create|run#12|AssertionError|assert match is not None, f"MyStateMachine's CREATE_FAILED reason is not a resource-handler message: {reason!r}"
Issue-Commit: 93aea946e1be9d371a5c94c0146ea55c9b1afa4f

# cloudformation__stepfunctions: the CREATE_FAILED reason for a rejected AWS::StepFunctions::StateMachine omits the (Service, Status Code, Request ID) envelope of the wrapped service error

> _Note: this issue was identified against the code base at commit `93aea946e1be9d371a5c94c0146ea55c9b1afa4f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
Cause 1 of 1: the resource-handler failure reason CloudFormation records for a Step Functions state machine the service rejects is missing the service-error envelope.
AWS: recorded behavior only (the message is CloudFormation's own rendering of the wrapped service error). When AWS::StepFunctions::StateMachine is refused at create, the resource's CREATE_FAILED ResourceStatusReason has the form
  Resource handler returned message: "<service message> (Service: <sdkId>, Status Code: <http status>, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: <code>)
For this template the parts are: service message `Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: There should only be one of the following fields: [Next, End] at /States/<state name>'` (the offending state name comes from the submitted definition), Service `AWSStepFunctions`, Status Code `400`, HandlerErrorCode `InvalidRequest`; Request ID, SDK Attempt Count and RequestToken are per-run values. The fixed text is everything outside those placeholders, including the `(Service: ..., Status Code: ..., Request ID: ...)` group.
Emulator: the reason is recorded as
  Resource handler returned message: "Invalid State Machine Definition: 'SCHEMA_VALIDATION_FAILED: There should only be one of the following fields: [Next, End] at /States/Only'" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)
— the service message and HandlerErrorCode are right, but the whole `(Service: AWSStepFunctions, Status Code: 400, Request ID: <id>)` segment (and the SDK Attempt Count group) is absent, so the reason does not parse as a resource-handler message at all.
Why: clients and tooling (CDK, CLI wrappers, these checks) parse the reason to recover which service refused the resource and with what HTTP status before deciding whether a deploy failure is retryable or a client error; with the envelope missing they can only treat the text as opaque, and the failure cannot be attributed to Step Functions.
Fix: the serializer that renders a service error into ResourceStatusReason for the Step Functions resource handler in CloudFormation's provisioning engine — emit the full `(Service: <sdkId>, Status Code: <status>, Request ID: <id>)` group (plus the SDK Attempt Count group) inside the quoted handler message.

## Evidence
Check cloudformation__stepfunctions.deploy-pass-state-with-next-and-end-together-rejected-at-stack-create:
  expected (regex over the CREATE_FAILED reason of MyStateMachine):
    ^Resource handler returned message: "(?P<message>.*) \(Service: (?P<service>[A-Za-z0-9]+), Status Code: (?P<status>\d+), Request ID: [^)]*\)( \(SDK Attempt Count: \d+\))?" \(RequestToken: [^,]*, HandlerErrorCode: (?P<handler>[A-Za-z]+)\)$
  actual reason:
    'Resource handler returned message: "Invalid State Machine Definition: \'SCHEMA_VALIDATION_FAILED: There should only be one of the following fields: [Next, End] at /States/Only\'" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)'
  -> AssertionError: MyStateMachine's CREATE_FAILED reason is not a resource-handler message

## Steps to Reproduce
CreateStack with an AWS::IAM::Role for states.amazonaws.com and an AWS::StepFunctions::StateMachine whose definition has a single Pass state declaring both "Next" and "End" (so Step Functions refuses it with SCHEMA_VALIDATION_FAILED). Wait for the terminal stack status and read DescribeStackEvents for the state machine's CREATE_FAILED ResourceStatusReason: the emulator's reason has no (Service: AWSStepFunctions, Status Code: 400, Request ID: <id>) group; AWS's does.
