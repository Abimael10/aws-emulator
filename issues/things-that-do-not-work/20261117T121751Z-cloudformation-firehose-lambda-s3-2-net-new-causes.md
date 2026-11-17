Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__firehose__lambda__s3.deploy-cfn-lambda-processor-missing-lambdaarn-rejected cloudformation__firehose__lambda__s3.deploy-cfn-lambda-processing-config-transforms-records-and-survives-stack-deletion
Issue-Key: cloudformation__firehose__lambda__s3.deploy-cfn-lambda-processor-missing-lambdaarn-rejected|_parse_delivery_stream_rejection#33|AssertionError|assert match, f"stack event reason did not match the CFN/Firehose wrapper shape: {reason!r}"
Issue-Key: cloudformation__firehose__lambda__s3.deploy-cfn-lambda-processing-config-transforms-records-and-survives-stack-deletion|_wait_for_object_count#38|Timeout|return keys
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# cloudformation__firehose__lambda__s3: 2 net-new causes — a failed DeliveryStream resource's ResourceStatusReason omits the service-exception envelope, and the Lambda-processor stack scenario never reaches a verdict

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
TWO net-new root causes in the CloudFormation-deployed Firehose+Lambda path, grounded in cloudformation__firehose__lambda__s3 checks the replay marks neither ALREADY FILED nor OWNED. Relationship to open issues: 20261111T111216Z-cloudformation-firehose-s3-in-place-update-of-aws-kinesisfirehose-deliverystream-is.md covers the UPDATE path of AWS::KinesisFirehose::DeliveryStream and 20261101T223651Z-firehose-kinesis-s3-cloudformation-deployed-kinesis-firehose-s3.md covers Kinesis-sourced CFN delivery; neither carries the failure-reason envelope below nor the Lambda-processor create path, and the replay does not mark these checks as owned by them.

=== Cause 1 of 2: the CREATE_FAILED event's ResourceStatusReason drops the Firehose service-exception envelope ===
Failing check: cloudformation__firehose__lambda__s3.deploy-cfn-lambda-processor-missing-lambdaarn-rejected.
AWS: when a resource handler surfaces a service exception, CloudFormation records it in DescribeStackEvents' ResourceStatusReason in a fixed envelope — Resource handler returned message: "<service message> (Service: <ServiceId>, Status Code: <http status>, Request ID: <uuid>, Extended Request ID: <token>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: <code>). The fixed text is that whole scaffold, including the parenthesised "(Service: …, Status Code: …, Request ID: …, Extended Request ID: …)" and "(SDK Attempt Count: …)" segments the SDK appends to the exception's str(); the variable parts are the underlying service message (here Firehose's "LambdaArn is required when Lambda processor is used."), the ServiceId ("Firehose"), the HTTP status, the per-call request ids/token and the attempt count. The HandlerErrorCode for this class is InvalidRequest and the stack ends ROLLBACK_COMPLETE with no stream left behind. (Recorded behavior for the exact segment order; the model states only the Firehose message.)
Emulator: the reason is emitted as 'Resource handler returned message: "LambdaArn is required when Lambda processor is used." (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)' — the quoted message carries the service sentence alone, with the "(Service: Firehose, Status Code: …, Request ID: …, Extended Request ID: …) (SDK Attempt Count: …)" envelope absent entirely.
Why: the reason string is the only machine-readable diagnosis a CFN client gets for a failed resource. Tooling that parses it to recover the originating service, the HTTP status or the request id for a support case (and every test that pins the envelope) fails against the emulator, and an operator loses the status code and request id that make the failure traceable.
Fix: the CloudFormation resource-handler error path that renders ResourceStatusReason must wrap the service exception in the SDK envelope (Service/Status Code/Request ID/Extended Request ID + SDK Attempt Count) before the RequestToken/HandlerErrorCode suffix.
Traces: the check id above.

=== Cause 2 of 2: the stack-deployed Lambda-processor delivery scenario makes no observable progress and is cut off at the harness cap ===
Failing check: cloudformation__firehose__lambda__s3.deploy-cfn-lambda-processing-config-transforms-records-and-survives-stack-deletion.
AWS: a stack carrying AWS::IAM::Role + AWS::Lambda::Function + AWS::KinesisFirehose::DeliveryStream whose ExtendedS3DestinationConfiguration declares ProcessingConfiguration Processors [Lambda(LambdaArn, BufferIntervalInSeconds, BufferSizeInMBs, NumberOfRetries)] reaches CREATE_COMPLETE; records put into the deployed stream are passed through the Lambda processor (each payload returned with result Ok and the "-TRANSFORMED" suffix appended), the transformed bytes land in the destination bucket, and deleting the stack removes the stream so DescribeDeliveryStream then answers ResourceNotFoundException "Firehose under account <accountId> not found." (recorded behavior; the check's own budgets are CREATE 600 s, settle 180 s, delivery 300 s).
Emulator: the check reaches no verdict at all — it is cut off by the harness with "Check timed out after 60.0s", so no stage of the scenario (stack create, processor invocation, transformed delivery, post-delete absence) is ever observed to complete. Hypothesis: one of the stages blocks, consistent with the other CFN-deployed Firehose delivery issues already open against this emulator.
Why: the CloudFormation+Lambda-transform shape is the standard way a Firehose pipeline is deployed. A client cannot verify its stack converges, that its transform runs, or that teardown removes the stream, and the whole scenario is unusable rather than merely wrong.
Fix: the CloudFormation provisioning of AWS::KinesisFirehose::DeliveryStream with a Lambda ProcessingConfiguration, and the Firehose processing/delivery path it hands to, must make observable progress — each stage must settle so the scenario terminates rather than hanging.
Traces: the check id above.

## Evidence
Replayed suite: simparity-behavioral-aws-v1 — cloudformation__firehose__lambda__s3 0/2 (0%), both failing checks marked neither ALREADY FILED nor OWNED.

Cause 1 — cloudformation__firehose__lambda__s3.deploy-cfn-lambda-processor-missing-lambdaarn-rejected
Expected (AWS), the envelope the check parses: ^Resource handler returned message: "(?P<message>.*?) \(Service: Firehose, Status Code: \d+, Request ID: [0-9a-f-]+, Extended Request ID: \S+\) \(SDK Attempt Count: \d+\)" \(RequestToken: [0-9a-f-]+, HandlerErrorCode: (?P<handler_code>\w+)\)$ with message == "LambdaArn is required when Lambda processor is used." and handler_code == "InvalidRequest".
Actual (emulator): "AssertionError: stack event reason did not match the CFN/Firehose wrapper shape: 'Resource handler returned message: \"LambdaArn is required when Lambda processor is used.\" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)'" — the stack did roll back and the Firehose sentence and handler code are right; only the service-exception envelope is missing.

Cause 2 — cloudformation__firehose__lambda__s3.deploy-cfn-lambda-processing-config-transforms-records-and-survives-stack-deletion
Expected (AWS): stack CREATE_COMPLETE, the put records delivered to S3 carrying the processor's "-TRANSFORMED" suffix, and DescribeDeliveryStream answering ResourceNotFoundException after the stack is deleted.
Actual (emulator): "Error: Check timed out after 60.0s" — no stage completed within the harness budget, so the scenario produced no verdict.

## Steps to Reproduce
Common setup: boot the emulator, point botocore at it; s3.create_bucket(Bucket=B).

Cause 1:
1. cfn.create_stack with a template whose Resources are ProcessorRole (AWS::IAM::Role for lambda.amazonaws.com), ProcessorFunction (AWS::Lambda::Function), a Firehose delivery role, and DeliveryStream (AWS::KinesisFirehose::DeliveryStream) whose ExtendedS3DestinationConfiguration declares ProcessingConfiguration Processors [{Type: Lambda, Parameters: [BufferIntervalInSeconds, BufferSizeInMBs]}] — deliberately OMITTING the LambdaArn parameter; Capabilities=['CAPABILITY_IAM'].
2. Poll describe_stacks until terminal — expect ROLLBACK_COMPLETE (the emulator matches here).
3. describe_stack_events(StackName=…) and take the ResourceStatusReason of the DeliveryStream event whose ResourceStatus ends in FAILED.
4. Expect 'Resource handler returned message: "LambdaArn is required when Lambda processor is used. (Service: Firehose, Status Code: 400, Request ID: <uuid>, Extended Request ID: <token>) (SDK Attempt Count: 1)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)'; observe the same string with the whole "(Service: … ) (SDK Attempt Count: …)" envelope missing.

Cause 2:
1. Deploy the same template WITH the LambdaArn processor parameter pointing at {'Fn::GetAtt': ['ProcessorFunction','Arn']} and BufferSizeInMBs '1'.
2. Wait for CREATE_COMPLETE, wait for the stream to settle ACTIVE, put a record, and poll the destination bucket for an object whose payload ends with b'-TRANSFORMED'.
3. Delete the stack and expect describe_delivery_stream to answer ResourceNotFoundException "Firehose under account <accountId> not found."
4. Observe the scenario make no observable progress and be cut off at the harness budget instead of reaching any of these states.

Finally re-replay the two check ids in trace_ids.
