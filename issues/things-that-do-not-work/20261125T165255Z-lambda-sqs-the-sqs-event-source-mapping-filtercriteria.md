Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sqs.esm-filter-criteria-non-matching-not-invoked
Issue-Key: lambda__sqs.esm-filter-criteria-non-matching-not-invoked|_collect#14|Timeout|resp = sqs.receive_message(
Issue-Commit: 6e4695f2e2e0380b6e4530904193a77f767a96ef

# lambda__sqs: the SQS event source mapping FilterCriteria negative scenario reaches no verdict — no message is ever delivered, so the matching control leg never lands

> _Note: this issue was identified against the code base at commit `6e4695f2e2e0380b6e4530904193a77f767a96ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
Cause 1 of 1: an Enabled SQS event source mapping carrying FilterCriteria never invokes the target function at all, so the scenario's control leg (a matching message must be delivered before a non-matching one can be shown not to invoke) never lands and the check spends its whole 60 s budget without reaching a verdict. Failing check: lambda__sqs.esm-filter-criteria-non-matching-not-invoked.

AWS: Smithy (lambda) models CreateEventSourceMapping POST /2015-03-31/event-source-mappings -> 202 with EventSourceMappingConfiguration (UUID, State, FilterCriteria, FilterCriteriaError). For an SQS source an Enabled mapping makes Lambda's poller read the queue and evaluate FilterCriteria against each message: a matching message invokes the function and is deleted from the queue, a non-matching message is discarded without any invocation. In AWS the matching delivery is observed within seconds of the mapping becoming Enabled. Recorded behavior only for the exact delivery latency.

Emulator: the mapping is accepted and reads back with its FilterCriteria, but no invocation ever happens — the matching message is not delivered either — and the check ends with "Check timed out after 60.0s". Hypothesis: the emulator keeps the mapping as control-plane state but runs no SQS poller, so a filter that correctly suppresses a non-matching message is indistinguishable from a mapping that delivers nothing at all; the negative assertion can never be made meaningful.

Why: a client that relies on ESM FilterCriteria to pre-filter an SQS queue cannot tell whether its filter works or whether delivery is simply absent; in the emulator no queued message is ever processed while the control plane reports a healthy Enabled mapping.

Fix: the Lambda event-source-mapping poller for SQS sources — it must poll the queue for an Enabled mapping, evaluate FilterCriteria per message, invoke the function with the matching records (deleting them) and drop non-matching messages without invoking.

Related open issues 20261121T105340Z-lambda-s3-sqs-an-enabled-sqs-event-source.md and 20261118T062834Z-lambda-pipes-sqs-an-enabled-sqs-event-source.md describe the same missing SQS delivery for other service groups; neither carries this check's replay key.

Traces: lambda__sqs.esm-filter-criteria-non-matching-not-invoked.

## Evidence
Expected (AWS): with an Enabled SQS event source mapping whose FilterCriteria selects on a message-body field, a matching SendMessage invokes the function within seconds (and the message leaves the queue), while a non-matching SendMessage produces no invocation.

Actual (emulator): no invocation occurs for the matching message either; the delivery wait never satisfies and the check ends with "Error: Check timed out after 60.0s". The 60 s figure is the harness budget; the requirement is that an Enabled SQS mapping delivers a matching message.

## Steps to Reproduce
1. Create an SQS queue and a Lambda function that records the SQS records event it receives.
2. CreateEventSourceMapping with EventSourceArn=<queue arn>, FunctionName=<fn>, Enabled=true and FilterCriteria selecting on a body field; wait until it reads back Enabled.
3. SendMessage with a body that matches the filter.
4. Poll for the function's invocation record.
5. Observe no invocation ever occurs, so the non-matching leg cannot be distinguished from absent delivery.
