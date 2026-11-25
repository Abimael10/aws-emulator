Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__logs__sqs.esm-scaling-config-floor-and-overflow-then-clear
Issue-Key: lambda|UpdateEventSourceMapping|ResourceInUseException
Issue-Commit: 003cb6bb321c7219a4ce9a0aad9437f3b0c08a6f

# iam__lambda__logs__sqs: UpdateEventSourceMapping refuses every update with ResourceInUseException, so a settled mapping's ScalingConfig can never be raised or cleared (1 cause)

> _Note: this issue was identified against the code base at commit `003cb6bb321c7219a4ce9a0aad9437f3b0c08a6f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateEventSourceMapping

## Description
One net-new root cause, grounded in 1 failing behavioral check that no open issue carries.

Cause 1 of 1: UpdateEventSourceMapping answers ResourceInUseException for a mapping that is not in a transient state, so no property of a settled event source mapping can ever be updated.

AWS: UpdateEventSourceMapping (Smithy: PUT /2015-03-31/event-source-mappings/{UUID}, success code 202, output shape EventSourceMappingConfiguration) applies the submitted members to an existing mapping and answers 202 echoing the new configuration. Its ScalingConfig member is ScalingConfig{MaximumConcurrency @range min 2 max 1000}; ScalingConfig is documented "(Amazon SQS only) The scaling configuration for the event source. To remove the configuration, pass an empty value", so ScalingConfig={} clears MaximumConcurrency and the member is then absent from the response and from GetEventSourceMapping. The operation does declare ResourceInUseException (modeled httpError 400), but the model scopes it precisely: "The operation conflicts with the resource's availability. For example, you tried to update an event source mapping in the CREATING state, or you tried to delete an event source mapping currently UPDATING." It is therefore due only while the mapping sits in a transient state (Creating/Enabling/Disabling/Updating/Deleting) — never for a mapping that has settled. A mapping created with Enabled=False settles in State "Disabled" and is updatable; AWS's message for the genuine transient case is the fixed text "Cannot update the event source mapping because it is in use." (no variable parts), which is exactly the text the emulator emits for the non-transient case.

Emulator: every UpdateEventSourceMapping on a freshly created, settled mapping is refused with HTTP 400 ResourceInUseException and the message "Cannot update the event source mapping because it is in use." In the failing check the mapping was created seconds earlier with Enabled=False and ScalingConfig.MaximumConcurrency=2 (that create was accepted and echoed correctly, and the sibling create with MaximumConcurrency=1001 was correctly refused as ValidationException), then the first UpdateEventSourceMapping — raising MaximumConcurrency to 777 — raised ResourceInUseException instead of answering 202. The refusal is unconditional with respect to the mapping's state: the handler never reaches the clear path (ScalingConfig={}) either.

Why: a client that relies on AWS's behavior cannot tune or remove a mapping's scaling after creation. ResourceInUseException is the documented signal "retry in a moment, the resource is mid-transition", so SDK and IaC retry loops treat it as transient and spin until their own deadline, then report a stuck mapping; CloudFormation/CDK/Serverless updates to an AWS::Lambda::EventSourceMapping never converge, and the only way to change MaximumConcurrency becomes delete-and-recreate, which loses the mapping UUID and its ARN.

Fix: the state precondition in the UpdateEventSourceMapping handler of the lambda service crate — it must refuse with ResourceInUseException only while the stored mapping is in a transient state (Creating/Enabling/Disabling/Updating/Deleting) and otherwise apply the submitted members, persist them, and answer 202 with the updated EventSourceMappingConfiguration; the ScalingConfig path must also honour an empty ScalingConfig as a removal so the member disappears from both the response and the subsequent GetEventSourceMapping read.

Traces: iam__lambda__logs__sqs.esm-scaling-config-floor-and-overflow-then-clear.

## Evidence
Failing check (suite heading `### iam__lambda__logs__sqs`, verdict [error], not marked ALREADY FILED or OWNED): iam__lambda__logs__sqs.esm-scaling-config-floor-and-overflow-then-clear.

Replay output:
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
  botocore.errorfactory.ResourceInUseException: An error occurred (ResourceInUseException) when calling the UpdateEventSourceMapping operation: Cannot update the event source mapping because it is in use.

Sequence the check had already completed successfully before that call:
  1. CreateEventSourceMapping(FunctionName=<fn>, EventSourceArn=<sqs queue arn>, Enabled=False, ScalingConfig={MaximumConcurrency: 2}) -> accepted, response echoed ScalingConfig.MaximumConcurrency == 2 (floor value accepted).
  2. CreateEventSourceMapping(... ScalingConfig={MaximumConcurrency: 1001}) -> correctly refused with ValidationException and the exact constraint message "1 validation error detected: Value '1001' at 'scalingConfig.maximumConcurrency' failed to satisfy constraint: Member must have value less than or equal to 1000".
  3. ListEventSourceMappings(FunctionName=<fn>) -> returned only the floor mapping's UUID, as expected.

Expected vs actual on the next call:
  Expected: UpdateEventSourceMapping(UUID=<floor uuid>, ScalingConfig={MaximumConcurrency: 777}) -> HTTP 202, response ScalingConfig.MaximumConcurrency == 777, and a following GetEventSourceMapping(UUID) reporting 777; then UpdateEventSourceMapping(UUID, ScalingConfig={}) -> HTTP 202 with no MaximumConcurrency, and GetEventSourceMapping reporting no MaximumConcurrency.
  Actual: HTTP 400, Error.Code "ResourceInUseException", Error.Message "Cannot update the event source mapping because it is in use." The clear leg is never reached.

Message anatomy: "Cannot update the event source mapping because it is in use." is entirely fixed text with no variable parts — it is AWS's wording for the transient-state case only, and the defect is that it is served for a settled (Disabled) mapping where AWS answers 202, not the wording itself.

Model citations: Smithy UpdateEventSourceMapping http {method: PUT, uri: /2015-03-31/event-source-mappings/{UUID}, code: 202}; errors include ResourceInUseException (httpError 400) and ResourceConflictException (409); ScalingConfig.MaximumConcurrency @range min 2 max 1000; ResourceInUseException doc "The operation conflicts with the resource's availability. For example, you tried to update an event source mapping in the CREATING state, or you tried to delete an event source mapping currently UPDATING."; ScalingConfig doc "(Amazon SQS only) The scaling configuration for the event source. To remove the configuration, pass an empty value."

No open issue carries this check or this cause: the open Lambda ESM issues concern mappings never delivering/invoking (e.g. 20261029T072224Z-lambda-s3-sqs-an-sqs-event-source-mapping.md) or CreateEventSourceMapping pre-flight refusal wording (20261106T225443Z-lambda-6-net-new-causes-layer-permission-preconditions.md); neither touches UpdateEventSourceMapping's state precondition.

## Steps to Reproduce
1. Boot the emulator and replay the behavioral suite; re-run the check iam__lambda__logs__sqs.esm-scaling-config-floor-and-overflow-then-clear.

By hand:
1. sqs CreateQueue, then GetQueueAttributes to read QueueArn.
2. iam CreateRole with a lambda.amazonaws.com trust policy and an inline policy granting sqs:ReceiveMessage/DeleteMessage/GetQueueAttributes on that queue plus logs:CreateLogGroup/CreateLogStream/PutLogEvents.
3. lambda CreateFunction (python3.12, any handler) with that role; wait for State=Active.
4. lambda CreateEventSourceMapping(FunctionName=<fn>, EventSourceArn=<queue arn>, Enabled=False, ScalingConfig={"MaximumConcurrency": 2}) and note the UUID.
5. lambda UpdateEventSourceMapping(UUID=<uuid>, ScalingConfig={"MaximumConcurrency": 777}).
   Observed: HTTP 400 ResourceInUseException "Cannot update the event source mapping because it is in use."
   Expected: HTTP 202 with ScalingConfig.MaximumConcurrency == 777, and GetEventSourceMapping(UUID) reporting 777.
6. lambda UpdateEventSourceMapping(UUID=<uuid>, ScalingConfig={}).
   Expected: HTTP 202 with ScalingConfig.MaximumConcurrency absent, and GetEventSourceMapping(UUID) reporting it absent.
