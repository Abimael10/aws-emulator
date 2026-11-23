Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__pipes__sqs.deploy-pipes-filter-pattern-config-positive
Issue-Key: lambda__pipes__sqs.deploy-pipes-filter-pattern-config-positive|run#11|Timeout|resp = sqs.receive_message(QueueUrl=result_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 81f864176a353ca0b12e45138ef5482587aa1344

# lambda__pipes__sqs: a CloudFormation-deployed AWS::Pipes::Pipe (SQS source + FilterCriteria + Lambda target) never delivers the matching message, so the template-declared FilterCriteria check reaches no verdict

> _Note: this issue was identified against the code base at commit `81f864176a353ca0b12e45138ef5482587aa1344`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePipe

## Description
One net-new root cause, grounded in one failing behavioral check.

Cause 1 of 1: a pipe provisioned by CloudFormation as `AWS::Pipes::Pipe` with an SQS source, `SourceParameters.FilterCriteria` and a Lambda target never produces any delivery at its target, so neither the keep-the-matching-message leg nor the drop-the-non-matching-message leg of FilterCriteria can be graded; the whole scenario exhausts the check budget with no observable progress.

AWS: `CreatePipe` accepts `SourceParameters.FilterCriteria` (Smithy: com.amazonaws.pipes#FilterCriteria.Filters -> FilterList @length(min 0, max 5), each com.amazonaws.pipes#Filter.Pattern an EventPattern @length(max 4096), sensitive) together with `SqsQueueParameters.BatchSize` and `TargetParameters.LambdaFunctionParameters.InvocationType`. Documented behavior (Pipes user guide, "Filtering Amazon SQS messages"): once the pipe is RUNNING, it polls the source queue; a message whose body matches the event pattern (here `{"body":{"type":["keep"]}}`) is delivered to the Lambda target as an SQS-style record carrying `body`, and a message that does NOT match is dropped and deleted from the source queue without ever reaching the target. Recorded behavior: on real AWS the matching message reaches the target (and the forwarder's SendMessage lands on the result queue) within about a minute of the stack reaching CREATE_COMPLETE, and the non-matching marker never appears.

Emulator: the check is terminated with `Error: Check timed out after 60.0s` with no step of the chain observable. Nothing the scenario depends on settles inside the budget: the stack (two AWS::SQS::Queue, two AWS::IAM::Role, one AWS::Lambda::Function, one AWS::Pipes::Pipe, with Outputs SourceQueueUrl/ResultQueueUrl) plus the pipe's own source polling never produce a single message on the result queue, so `marker_keep` is never seen. The matching message is therefore indistinguishable from a dropped one, and the differential (drop vs keep) is unobservable.

Why: a client that declares FilterCriteria in its template gets a stack it believes is wired and a target that is never invoked. Messages sit in (or vanish from) the source queue, the Lambda runs zero times, and the application silently processes nothing; worse, because non-delivery is also the correct outcome for a non-matching message, a consumer cannot distinguish "filtered" from "broken" and will mis-diagnose its own filter pattern.

Fix: the Pipes source poller/dispatch for an SQS source — the component that, for a RUNNING pipe, receives from the source queue, evaluates `SourceParameters.FilterCriteria.Filters[].Pattern` against the message, deletes non-matching messages from the source, and invokes the `Target` Lambda with the SQS record batch — plus the CloudFormation `AWS::Pipes::Pipe` resource handler's propagation of SourceParameters/TargetParameters into that pipe state.

Related but NOT covering this check (the replay marked it neither ALREADY FILED nor OWNED): 20261121T105340Z-lambda-pipes-sqs-an-eventbridge-pipe-with-an.md (SQS-source pipe never delivers to a Lambda target, API-created), 20261117T142140Z-pipes-sqs-the-filtercriteria-non-delivery-check-never.md and 20261103T175414Z-pipes-updatepipe-re-installing-filtercriteria-on-a-running.md.

Traces: lambda__pipes__sqs.deploy-pipes-filter-pattern-config-positive (verdict: mismatch, `Check timed out after 60.0s`).

## Evidence
Expected (AWS): with a pipe whose SourceParameters = {"SqsQueueParameters":{"BatchSize":1},"FilterCriteria":{"Filters":[{"Pattern":"{\"body\":{\"type\":[\"keep\"]}}"}]}} and Target = the forwarder Lambda (InvocationType REQUEST_RESPONSE), sending {"type":"drop","marker":<drop>} and {"type":"keep","marker":<keep>} to the source queue results in exactly one message on the result queue, the one carrying the keep marker; the drop marker never appears.

Actual (emulator): over the 90 s collection window the result queue yields no messages at all (seen_markers stays empty), so the assertion "matching message was never delivered to the target" is the outcome; the harness terminates the check first with `Error: Check timed out after 60.0s`. Neither the keep nor the drop leg is observable, i.e. the pipe performs no source polling / target invocation.

## Steps to Reproduce
1. cloudformation.create_stack with a template containing: SourceQueue and ResultQueue (AWS::SQS::Queue), LambdaExecRole and PipeRole (AWS::IAM::Role with the pipes.amazonaws.com / lambda.amazonaws.com trust and sqs:ReceiveMessage/DeleteMessage/GetQueueAttributes on SourceQueue plus lambda:InvokeFunction on the function), ForwarderFunction (AWS::Lambda::Function, python3.12, inline ZipFile handler that SendMessage's each record's `body` to RESULT_QUEUE_URL), and Pipe (AWS::Pipes::Pipe) with Source = SourceQueue Arn, SourceParameters = {"SqsQueueParameters":{"BatchSize":1},"FilterCriteria":{"Filters":[{"Pattern":"{\"body\":{\"type\":[\"keep\"]}}"}]}}, Target = ForwarderFunction Arn, TargetParameters = {"LambdaFunctionParameters":{"InvocationType":"REQUEST_RESPONSE"}}; Capabilities CAPABILITY_IAM/CAPABILITY_NAMED_IAM; Outputs SourceQueueUrl/ResultQueueUrl.
2. Wait for CREATE_COMPLETE and read the two output queue URLs.
3. sqs.send_message(SourceQueueUrl, '{"type":"drop","marker":"<drop>"}') and sqs.send_message(SourceQueueUrl, '{"type":"keep","marker":"<keep>"}').
4. Poll sqs.receive_message(ResultQueueUrl, MaxNumberOfMessages=10, WaitTimeSeconds=5) for 90 s -> expect exactly the keep marker; emulator yields nothing.
