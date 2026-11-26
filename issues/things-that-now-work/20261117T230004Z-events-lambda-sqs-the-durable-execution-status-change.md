Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__lambda__sqs.xsvc-lambda-1-durable-status-change-running-then-succeeded-reaches-sqs
Issue-Key: events__lambda__sqs.xsvc-lambda-1-durable-status-change-running-then-succeeded-reaches-sqs|run#20|AssertionError|assert ev["resources"] == [qualified], f"resources were {ev['resources']!r}, want {[qualified]!r}"
Issue-Commit: 952a95f6f44f35dec8503ef518e3b550df5d6797

# events__lambda__sqs: the Durable Execution Status Change event's resources array carries the durable-execution ARN instead of the qualified function ARN

> _Note: this issue was identified against the code base at commit `952a95f6f44f35dec8503ef518e3b550df5d6797`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartDurableExecution

## Description
One root cause.

=== Cause 1 of 1: the aws.lambda "Durable Execution Status Change" event envelope lists the wrong ARN in `resources` ===
AWS: recorded behavior only (the EventBridge envelope a service emits is not in the Smithy model). An EventBridge event's `resources` array names the resource(s) the event is ABOUT at the granularity the service publishes; for the aws.lambda "Durable Execution Status Change" event that is the qualified function ARN — arn:aws:lambda:<region>:<account>:function:<function-name>:$LATEST (the function ARN plus the invoked qualifier, the same value the event carries in detail.functionArn). The execution's own identity is carried in the detail, as detail.durableExecutionArn / detail.durableExecutionName, and not in `resources`. Both the RUNNING and the terminal (SUCCEEDED) event share that same single-element `resources` array.
Emulator: `resources` instead holds the durable-execution ARN — the qualified function ARN with the execution path appended, of the form arn:aws:lambda:<region>:<account>:function:<function-name>:$LATEST/durable-execution/<execution-name>/<execution-id>. The rest of the envelope is correct (source aws.lambda, detail-type "Durable Execution Status Change", account, region all matched, and the events are filterable by detail.durableExecutionArn), so only the `resources` member is built from the wrong ARN.
Why: `resources` is a first-class event-pattern field. A rule written as {"resources": ["arn:aws:lambda:<region>:<account>:function:<name>:$LATEST"]} — the normal way to subscribe to one function's durable-execution status changes — matches nothing on the emulator, so targets never fire; conversely any consumer that reads resources[0] as the function ARN (to look the function up, to tag or to route) gets a string that is not a function ARN and that no Lambda API accepts.
Fix: the lambda durable-execution status-change event emitter (the component that builds the EventBridge envelope for "Durable Execution Status Change") — it must put the qualified function ARN in `resources` and leave the execution ARN to detail.durableExecutionArn.
Traces: events__lambda__sqs.xsvc-lambda-1-durable-status-change-running-then-succeeded-reaches-sqs

## Evidence
Failing check events__lambda__sqs.xsvc-lambda-1-durable-status-change-running-then-succeeded-reaches-sqs (mismatch):

AssertionError: resources were ['arn:aws:lambda:us-west-2:000000000000:function:sim2real-xsvc-durev-1b32bcddb266:$LATEST/durable-execution/ok-f0c3277205dc/29ea9409325241ddbf9897f859b508f9'], want ['arn:aws:lambda:us-west-2:000000000000:function:sim2real-xsvc-durev-1b32bcddb266:$LATEST']

Expected (AWS): resources == ["<function ARN>:$LATEST"] on BOTH the RUNNING and the SUCCEEDED event. The fixed part of the template is the qualified function ARN shape arn:aws:lambda:<region>:<account>:function:<name>:$LATEST; the region, account id, function name, execution name and execution id above are this run's values. The emulator's extra "/durable-execution/<execution-name>/<execution-id>" suffix is the divergence.

Asserts that passed before this one on the same events: source == 'aws.lambda', detail-type == 'Durable Execution Status Change', account == caller account, region == request region, and both a RUNNING and a SUCCEEDED event were delivered to the SQS target and were selectable by detail.durableExecutionArn.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite.
2. Create an SQS queue and an EventBridge rule on the default bus matching {"source": ["aws.lambda"], "detail-type": ["Durable Execution Status Change"]} with that queue as target.
3. Create a durable-execution-enabled Lambda function and start one execution that completes.
4. Drain the queue and inspect the delivered RUNNING and SUCCEEDED envelopes' `resources` array.
5. Observe resources == ["<qualified function ARN>/durable-execution/<execution-name>/<execution-id>"] instead of ["<qualified function ARN>"]; a rule keyed on resources == [the qualified function ARN] therefore matches nothing.
6. Equivalently: replay check events__lambda__sqs.xsvc-lambda-1-durable-status-change-running-then-succeeded-reaches-sqs.
