Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__dynamodb__lambda.deploy-ddb-stream-lambda-cloudwatch-dashboard
Issue-Key: cloudwatch__dynamodb__lambda.deploy-ddb-stream-lambda-cloudwatch-dashboard|run#24|AssertionError|assert 0 <= maximum <= elapsed_ms, (
Issue-Commit: 4cf7211f237a5c930178a3131e9243c3c3eb5dd6

# AWS/Lambda IteratorAge for a DynamoDB-stream event source mapping is fabricated: the datapoint exceeds the wall-clock age of the stream record

> _Note: this issue was identified against the code base at commit `4cf7211f237a5c930178a3131e9243c3c3eb5dd6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
Cause 1 of 1: the AWS/Lambda IteratorAge series published for a DynamoDB-stream event source mapping is not derived from the processed record's age, so it reports a lag larger than the time that has physically elapsed since the record was written. Failing check: cloudwatch__dynamodb__lambda.deploy-ddb-stream-lambda-cloudwatch-dashboard.

AWS: IteratorAge (namespace AWS/Lambda, unit Milliseconds, dimension FunctionName) is the age of the last record in the batch the event source mapping just processed — the difference between the time Lambda read the batch and that record's ApproximateCreationDateTime. It is therefore bounded above by the wall clock elapsed since the record was written to the stream: 0 <= IteratorAge <= (now - first stream write). Recorded behavior only: metrics are not in the Smithy model, so there is no modeled constraint to cite; the invariant is the one AWS's definition of the metric implies and the one the recording relies on, and the recording shows a Maximum inside that window.

Emulator: for a stack that wires a DynamoDB stream to a Lambda via an event source mapping and a CloudWatch dashboard over IteratorAge, the published datapoint's Maximum is roughly three times the greatest possible value — 97.56 ms reported when only 33.05 ms of wall clock had elapsed since the first stream write. The magnitude is not a constant and not a clamp; it reads as a synthesized number rather than one computed from the record's ApproximateCreationDateTime.

Why: a client cannot use IteratorAge for what it exists for. A lag alarm (e.g. IteratorAge > N ms) fires or stays silent on an invented number, a dashboard widget shows a lag the stream never had, and the standard "consumer has caught up" test (IteratorAge near zero) can never be trusted. Because the value can exceed the elapsed time, no threshold derived from the application's own timing holds.

Fix: the metric emission in Lambda's event-source-mapping poller (the component that publishes AWS/Lambda IteratorAge for a stream mapping) — compute the datapoint from the processed record's ApproximateCreationDateTime against the batch's read time instead of synthesizing it, so the value is bounded by the record's real age.

Traces: cloudwatch__dynamodb__lambda.deploy-ddb-stream-lambda-cloudwatch-dashboard (IteratorAge plausibility leg).

## Evidence
Expected: 0 <= IteratorAge Maximum <= elapsed wall-clock milliseconds since the first write to the DynamoDB stream.

Actual (replayed check, failing assertion):
  AssertionError: IteratorAge Maximum=97.55897521972656ms outside the plausible range [0, 33.052ms] elapsed since the first stream write

The two numbers are per-run values; the requirement is the bound they violate (the reported Maximum must not exceed the elapsed time since the record was written). The series read is namespace AWS/Lambda, metric IteratorAge, statistic Maximum, for the function the event source mapping invokes.

## Steps to Reproduce
1. Deploy a template with an AWS::DynamoDB::Table (StreamSpecification NEW_IMAGE), a Lambda function, an AWS::Lambda::EventSourceMapping from the table stream to the function, and a CloudWatch dashboard over AWS/Lambda IteratorAge for that function.
2. Record the time, then write one item to the table so the stream delivers a record and the function is invoked.
3. Read the AWS/Lambda IteratorAge series for the function (statistic Maximum) and compare it with the milliseconds elapsed since step 2.
4. Observe a Maximum greater than the elapsed time (97.56 ms vs 33.05 ms in the replay) instead of a value inside [0, elapsed].
