Issue-Source: trace-sweep-divergence
Issue-Trace: iot-fleet-indexing-error-paths
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-fleet-indexing-error-paths iot-fleet-metric-lifecycle
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|DescribeFleetMetric|4xx->4xx|$.message
Issue-Commit: 9944627f2412edf656bcae33c2e25abe8d8b32c9

# iot DescribeFleetMetric not-found Message omits the leading "An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: " segment the recorded wire body carries

> _Note: this issue was identified against the code base at commit `9944627f2412edf656bcae33c2e25abe8d8b32c9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeFleetMetric

## Description
ONE cause must be closed before submitting.

Cause 1 of 1: DescribeFleetMetric for a fleet metric that does not exist is refused with the correct HTTP status and the correct error code, but the serialized `message` member of the error body carries only the TAIL of the text the recorded wire response carries — the whole leading segment is absent.

AWS: Smithy (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json) — DescribeFleetMetric = GET /fleet-metric/{metricName}; metricName is @required @httpLabel, FleetMetricName @length 1..128 pattern ^[a-zA-Z0-9_\-\.]+$; the operation's modeled errors include ResourceNotFoundException with @httpError 404. The model says nothing about the message text, so for the TEXT this is recorded behavior only. The recorded response for this refusal is status 404 with x-amzn-errortype ResourceNotFoundException and a JSON body whose `message` member is: "An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: FleetMetric <account-id>:<metricName> not found." Fixed text of that template: "An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: FleetMetric ", the ":" separator between account and name, and the trailing " not found.". Variable parts, taken from the request and the emulator's state: the caller's account id and the metricName from the {metricName} URI label. The status (404) and the error code (ResourceNotFoundException) already match and are NOT part of this divergence.

Emulator: answers 404 / ResourceNotFoundException with `message` = "FleetMetric <account-id>:<metricName> not found." — exactly the tail of the recorded text, with the entire leading "An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: " segment missing. The divergence is byte-identical across four replayed steps in two independent fixtures (two fixtures x two steps each), so it is the handler's message template and not a per-run value: the account id and the metric name agree character-for-character on both sides of every one of those four comparisons, and ONLY the leading segment differs.

Why: a client that branches on, logs or asserts against this refusal's message never matches the text AWS is recorded as returning, even though it got the right status and the right code. Any parity harness keyed to AWS's sentence for this error class fails against a response that is otherwise correct, and a caller that string-matches the message to distinguish a missing fleet metric from another 404 on this path sees different text than production.

Fix: the DescribeFleetMetric handler's not-found refusal path in the iot crate and the error message it formats there — not the service-wide "<Type> <account>:<name> not found." registry formatter, which other iot operations legitimately share and which must keep its current text for them. This is a message-template fix in that one refusal path; the status code, the error code and the body shape are already correct and must not change.

Note on an existing issue: open issue 20261123T222507Z-iot-describefleetmetric-s-resourcenotfoundexception-message-is-only-the.md describes the same symptom, but it was filed from the BEHAVIORAL corpus (simparity-behavioral-aws-v1) and carries no wire replay key, so it does not cover these wire fixture steps and a fixer replaying it never exercises them. That issue also leaves an explicit open question — whether the leading segment belongs to the serialized Message member or is only botocore's str(ClientError) rendering — and flags it as unresolved. THIS replay answers it from the other direction: the divergence is observed against the recorded HTTP response body's `message` member, with no SDK in the path, so the expectation under test is the on-the-wire member itself. A fixer should close this wire issue and that behavioral issue together with one change, and record that determination.

Traces: iot-fleet-indexing-error-paths#task#1 and #task#4; iot-fleet-metric-lifecycle#task#4 and #task#7.

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the emulator built from this checkout (emulator_up=true, 1120 steps, 891 passed). This cluster carries no stub_path and an EMPTY owned_by, so no filed issue holds its replay key.

Divergence cluster service=iot|DescribeFleetMetric|status=404->404|keys=$.message — verdict mismatch, expected_status 404, actual_status 404 (status and x-amzn-errortype agree; only $.message diverges).

Representative: fixture iot-fleet-indexing-error-paths, step task#1.
Affected (4 steps, 2 fixtures, identical symptom):
- iot-fleet-indexing-error-paths#task#1
- iot-fleet-indexing-error-paths#task#4
- iot-fleet-metric-lifecycle#task#4
- iot-fleet-metric-lifecycle#task#7

Replay detail, verbatim:
  $.message: value_mismatch (expected 'An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: FleetMetric 000000000000:sim2real-probe-iot-fleetmetric-missing-0cf32869 not found.', got 'FleetMetric 000000000000:sim2real-probe-iot-fleetmetric-missing-0cf32869 not found.')

Expected: 404, x-amzn-errortype ResourceNotFoundException, body.message == "An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: FleetMetric <account>:<metricName> not found."
Actual:   404, x-amzn-errortype ResourceNotFoundException, body.message == "FleetMetric <account>:<metricName> not found."

The account id 000000000000 and the metric name sim2real-probe-iot-fleetmetric-missing-0cf32869 are one run's values filling the template and are IDENTICAL on both sides — they are not the divergence. The requirement is the fixed text plus the caller's account id and the request's metricName; the divergence is the missing leading segment only.

## Steps to Reproduce
1. git_clone()
2. replay_wire_corpus()
3. Inspect the cluster keyed service=iot|DescribeFleetMetric|status=404->404|keys=$.message; re-replay fixture iot-fleet-indexing-error-paths step task#1 (and task#4), and fixture iot-fleet-metric-lifecycle steps task#4 and task#7.

Direct repro against the booted emulator with an iot client:
- GET /fleet-metric/<metricName> for a metricName that was never created (or was created and then deleted via DeleteFleetMetric).
- Observe HTTP 404 with header x-amzn-errortype: ResourceNotFoundException (both correct) and a JSON body whose `message` member is "FleetMetric <account>:<metricName> not found.".
- The recorded response's `message` member for the same refusal is "An error occurred (ResourceNotFoundException) when calling the DescribeFleetMetric operation: FleetMetric <account>:<metricName> not found." — compare the two strings and note that the only difference is the absent leading segment.
- Confirm the fix by re-replaying all four affected steps above; the status, the error code and the body shape must be unchanged.
