Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.fleet-metric-invalid-aggregation-rejected iot.fleet-metric-query-string-preserved iot.telemetry-fleet-metric-invalid-aggregation-rejected
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.fleet-metric-invalid-aggregation-rejected|run#11|AssertionError|assert not_found_message == expected_not_found, (
Issue-Key: iot.fleet-metric-query-string-preserved|run#16|AssertionError|assert message == (
Issue-Key: iot.telemetry-fleet-metric-invalid-aggregation-rejected|run#15|AssertionError|assert not_found_code_message == f"An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: FleetMetric {ctx.account}:{bad_
Issue-Commit: 04b7737d40b7fda90f20de8a2ba64e1a389f2093

# iot DescribeFleetMetric's ResourceNotFoundException Message is only the tail of the recorded text — the leading "An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: " segment is absent (3 checks)

> _Note: this issue was identified against the code base at commit `04b7737d40b7fda90f20de8a2ba64e1a389f2093`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeFleetMetric

## Description
REOPENS 20261114T082236Z-iot-refusal-message-fidelity-7-causes-addthingtothinggroup-createstream.md: its fix left the call refused. That issue is marked fixed and carried these three exact check ids as its Cause 7; the six other causes in it no longer appear in the failing list, but DescribeFleetMetric's not-found Message is unchanged and the same three checks still fail on it, byte for byte, in this replay. This is not a new defect — it is the unclosed remainder of that issue, and its open question (does the leading segment belong to the Message member?) was never resolved, only closed.

ONE cause must be closed before submitting.

Cause 1 of 1: DescribeFleetMetric for a fleet metric that does not exist is refused with the right status and the right error code, but the Message member carries only the tail of the text the recording asserts.
AWS: Smithy (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json) — DescribeFleetMetric = GET /fleet-metric/{metricName}, metricName @required @httpLabel @length 1..128 pattern ^[a-zA-Z0-9_\-\.]+$; modeled errors include ResourceNotFoundException @httpError 404. The model says nothing about the message text, so for the text this is recorded behavior only: three independent behavioral checks read botocore's response["Error"]["Message"] for this refusal and require it to equal "An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: FleetMetric <account-id>:<metricName> not found." Fixed text: "An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: FleetMetric ", the ":" separator, and " not found."; variable parts: the caller's account id and the metricName from the {metricName} URI label. The status is 404 and the code ResourceNotFoundException, which already match. Note the asymmetry that argues this is the recording and not a harness artifact: in the same check iot.fleet-metric-query-string-preserved, UpdateFleetMetric on the very same deleted metric is asserted — through the same _error() helper, reading the same Error.Message field — to be "FleetMetric <metricName> doesn't exist", with NO such prefix and no account qualifier. One helper, two operations, two different recorded shapes of message, so the prefix was recorded for DescribeFleetMetric specifically.
Emulator: 404 ResourceNotFoundException with Message "FleetMetric <account-id>:<metricName> not found." — exactly the tail of the recorded text, missing the whole leading "An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: " segment. Every other leg of all three checks passes: CreateFleetMetric's InvalidRequestException "Invalid values [average] for Statistics aggregation on String field. Supported values are [count]" refusal, the character-for-character queryString round trip, the aggregationField/aggregationType read-back and the absence of the rejected metric from ListFleetMetrics.
Why: a client that branches on, or asserts against, this refusal's message never matches the text the recording shows, and a parity test keyed to AWS's sentence fails on a response whose status and code are right. Hypothesis (unresolved, carried over from the earlier issue): that leading segment is also how botocore renders str(ClientError), so a fixer MUST confirm against the recording whether it belongs to the serialized Message member before prepending it — if the recording shows the Message member is only the tail, then the divergence is in the recorded expectation and the fixer should say so explicitly and close this with that determination, rather than emitting an operation name inside an error message AWS does not duplicate. Either way the three checks must stop failing or the determination must be recorded; last time they were closed with neither.
Fix: the DescribeFleetMetric handler's not-found refusal path in the iot crate (services/service-com-amazonaws-iot-awsiotservice/src/ops/describe_fleet_metric.rs) and the error message it formats — not the shared "<Type> <account>:<name> not found." registry formatter, which other operations in this service legitimately use.
Traces: iot.fleet-metric-invalid-aggregation-rejected, iot.fleet-metric-query-string-preserved, iot.telemetry-fleet-metric-invalid-aggregation-rejected.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout; service heading '### iot'. All three failures are [mismatch] and carry no ALREADY FILED / OWNED marker.

Cause 1 (3 checks, one assertion site each, identical symptom):
- iot.fleet-metric-invalid-aggregation-rejected, check_iot_fleet_metric_invalid_aggregation_rejected.py:49 —
  "AssertionError: expected message 'An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: FleetMetric 000000000000:sim2real-fleetmetric-bad-8d91e5549e31 not found.', got 'FleetMetric 000000000000:sim2real-fleetmetric-bad-8d91e5549e31 not found.'"
- iot.telemetry-fleet-metric-invalid-aggregation-rejected, check_iot_telemetry_fleet_metric_invalid_aggregation_rejected.py:63 —
  "AssertionError: expected the exact ResourceNotFoundException message, got 'FleetMetric 000000000000:sim2real-badmetrice7cb3da0e007 not found.'"
- iot.fleet-metric-query-string-preserved, check_iot_fleet_metric_query_string_preserved.py:60 —
  "AssertionError: FleetMetric 000000000000:sim2realfm2188037e65b9 not found."

Expected (all three): Error.Code == "ResourceNotFoundException" (matches) and Error.Message == "An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: FleetMetric <account>:<metricName> not found."
Actual (all three): Error.Code == "ResourceNotFoundException", Error.Message == "FleetMetric <account>:<metricName> not found."

Corroborating asymmetry inside iot.fleet-metric-query-string-preserved (lines 64-75): the same _error() helper, reading exc.response["Error"]["Message"], is asserted for UpdateFleetMetric on the same deleted metric to equal "FleetMetric <metricName> doesn't exist" — unprefixed and unqualified — so the prefix is not something the helper adds.

All preceding legs passed in each check, including CreateFleetMetric's InvalidRequestException code and message, the queryString/aggregationField/aggregationType read-back, and ListFleetMetrics not containing the rejected metric.

The account id 000000000000 and the metric names sim2real-fleetmetric-bad-8d91e5549e31 / sim2real-badmetrice7cb3da0e007 / sim2realfm2188037e65b9 are one run's values filling the template; the requirement is the fixed text plus the account id and the request's metricName.

## Steps to Reproduce
1. git_clone(); 2. run_behavioral_suite(); 3. re-replay the three checks in trace_ids.
Direct repro against the booted emulator with a botocore iot client:
- update_indexing_configuration(thingIndexingConfiguration={"thingIndexingMode": "REGISTRY"}) and poll describe_index(indexName="AWS_Things") until indexStatus == "ACTIVE".
- create_fleet_metric(metricName=M, queryString="thingName:*", aggregationType={"name": "Statistics", "values": ["sum"]}, aggregationField="registry.version", period=300, indexName="AWS_Things"), then delete_fleet_metric(metricName=M).
- describe_fleet_metric(metricName=M) and read response["Error"]["Code"] and response["Error"]["Message"] off the ClientError: Code is ResourceNotFoundException (correct), Message is "FleetMetric <account>:M not found." where the recording requires "An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: FleetMetric <account>:M not found."
- For the same metric, update_fleet_metric(metricName=M, indexName="AWS_Things", period=600) — its recorded Message is the unprefixed "FleetMetric M doesn't exist", confirming the prefix is specific to the describe path and not added by the client.
- Before changing the handler, inspect the recording for the DescribeFleetMetric 404 and determine whether the leading segment is part of the serialized Message member or botocore's str(ClientError) rendering; record that determination with the fix.
