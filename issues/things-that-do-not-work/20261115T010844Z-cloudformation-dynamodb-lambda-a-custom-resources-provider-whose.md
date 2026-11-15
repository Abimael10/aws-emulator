Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__dynamodb__lambda.cdk-custom-resource-provider-iscomplete-timeout-fails-the-stack
Issue-Key: cloudformation__dynamodb__lambda.cdk-custom-resource-provider-iscomplete-timeout-fails-the-stack|run#13|AssertionError|assert reason == expected_reason, (
Issue-Commit: 3cd41981ee722cd642660e763820306da1bf304f

# cloudformation__dynamodb__lambda: a custom_resources.Provider whose isComplete handler never reports done fails the stack with a JavaScript stack trace instead of the Provider's own 'Operation timed out' reason

> _Note: this issue was identified against the code base at commit `3cd41981ee722cd642660e763820306da1bf304f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
Cause 1 of 1: when a CDK `custom_resources.Provider` custom resource exhausts its isComplete waiter budget, the CREATE_FAILED event's ResourceStatusReason carries the framework's internal JavaScript stack trace rather than the Provider's own timeout reason.

AWS: the Provider framework answers CloudFormation with status FAILED and reason `Operation timed out` when its isComplete handler never reports done within the configured total timeout. CloudFormation wraps that reason verbatim into the custom resource's CREATE_FAILED event, together with the request it was answering, producing exactly:
`Received response status [FAILED] from custom resource. Message returned: Operation timed out (RequestId: <the custom-resource request id>)`
The fixed text is `Received response status [FAILED] from custom resource. Message returned: ` … ` (RequestId: `…`)`; the only variable parts are the reason the provider returned and the request id of the create request. Smithy: CloudFormation models StackEvent.ResourceStatusReason as a free-form string, so the wording is recorded behavior, not a model constraint; the status code of the DescribeStackEvents call itself is 200 in both cases.

Emulator: the event's reason is
`Received response status [FAILED] from custom resource. Message returned: at from (<input>:246:46)\n    at parseJsonPayload (/var/task/util.js:1:904)\n    at invokeUserFunction (/var/task/framework.js:1:4091) (RequestId: <id>)`
— the wrapper is right, but the wrapped reason is a raw JS stack trace from the framework's own payload parsing instead of the `Operation timed out` the waiter should have produced. That the trace points at parseJsonPayload/invokeUserFunction indicates the framework's isComplete response is being mis-handled rather than timed out.

Why: the stack still fails, but the reason a client (or an operator reading stack events) sees is an internal implementation trace that names no cause. Any automation that matches the documented `Operation timed out` reason to distinguish a waiter timeout from a handler error cannot do so, and the distinction between "isComplete never finished" and "onEvent returned garbage" is lost.

The open issue 20261105T204906Z-cloudformation-dynamodb-lambda-a-custom-resources-provider-custom.md concerns the same Provider construct but does not carry this check's replay key.

Fix: the CloudFormation custom-resource provider framework's waiter — produce the FAILED response with reason `Operation timed out` when the isComplete budget expires, and keep internal parse failures out of the ResourceStatusReason surfaced on the stack event.

Traces: cloudformation__dynamodb__lambda.cdk-custom-resource-provider-iscomplete-timeout-fails-the-stack.

## Evidence
Expected CREATE_FAILED ResourceStatusReason (fixed text plus the request id from the create request):
`Received response status [FAILED] from custom resource. Message returned: Operation timed out (RequestId: <request-id>)`

Actual ResourceStatusReason:
`Received response status [FAILED] from custom resource. Message returned: at from (<input>:246:46)\n    at parseJsonPayload (/var/task/util.js:1:904)\n    at invokeUserFunction (/var/task/framework.js:1:4091) (RequestId: <request-id>)`

The request id differs per run and is not part of the requirement; the diverging part is the wrapped reason, which must be the Provider's `Operation timed out`.

## Steps to Reproduce
1. Deploy a CDK app with a `custom_resources.Provider` custom resource whose isComplete handler always reports `IsComplete: false`, with a totalTimeout of ~40 s.
2. Wait for the stack to reach a failed state and read the custom resource's CREATE_FAILED event via DescribeStackEvents.
3. Compare ResourceStatusReason against `Received response status [FAILED] from custom resource. Message returned: Operation timed out (RequestId: <id>)`.
Replay check cloudformation__dynamodb__lambda.cdk-custom-resource-provider-iscomplete-timeout-fails-the-stack to confirm the fix.
