Issue-Source: trace-sweep-divergence
Issue-Trace: sagemakertrainingsessionruntime-execute-sampling-operation-generates-text
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sagemakertrainingsessionruntime-execute-sampling-operation-generates-text
Issue-Shape-Id: com.amazonaws.sagemakertrainingsessionruntime#LegoRouterService
Issue-Key: sagemaker|DescribeTrainingSession|2xx->2xx|$.TrainingSessionDetail.BillableTokenUsage
Issue-Key: sagemaker|DescribeTrainingSession|2xx->2xx|$.TrainingSessionDetail.CreationTime
Issue-Key: sagemaker|DescribeTrainingSession|2xx->2xx|$.TrainingSessionDetail.LastModifiedTime
Issue-Key: sagemaker|DescribeTrainingSession|2xx->2xx|$.TrainingSessionDetail.LastReceivedSequenceNumber
Issue-Key: sagemaker|DescribeTrainingSession|2xx->2xx|$.TrainingSessionDetail.TrainingSessionStatus
Issue-Commit: 5e1ce66aaf40efbb8bcb903be4505a65ffac087e

# sagemaker DescribeTrainingSession diverges: training-session detail omits usage/sequence members and the session status never leaves Running

> _Note: this issue was identified against the code base at commit `5e1ce66aaf40efbb8bcb903be4505a65ffac087e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTrainingSession

## Description
Both causes are in the sagemaker DescribeTrainingSession read path for training sessions (the control-plane resource the sagemakertrainingsessionruntime data plane samples against). The emulator answers 200 with a TrainingSessionDetail that is both incomplete and lifecycle-frozen. 2 causes.

Cause 1 of 2: DescribeTrainingSession's TrainingSessionDetail omits BillableTokenUsage and LastReceivedSequenceNumber (fixture sagemakertrainingsessionruntime-execute-sampling-operation-generates-text, step task#6).
AWS: Smithy (services/service-com-amazonaws-sagemaker-sagemaker) DescribeTrainingSession -> DescribeTrainingSessionResponse.TrainingSessionDetail (required) of shape TrainingSessionDetail, which models BillableTokenUsage -> BillableTokenUsage {PrefillTokenCount: long, SampleTokenCount: long, TrainingTokenCount: long} ("The billable token usage for the training session") and LastReceivedSequenceNumber -> JavaInteger ("The sequence number of the last training operation received by the session"). The recorded response for a live session carries both: BillableTokenUsage present as a structure with all three counters, and LastReceivedSequenceNumber present as an integer. A session that has received no traffic yet reports the zero-valued form (counters 0, sequence number 0) rather than omitting the members — i.e. these are accounted values the service always reports for an existing session, not optional echoes of request input. Status 200, no error.
Emulator: answers 200 but TrainingSessionDetail.BillableTokenUsage is absent and TrainingSessionDetail.LastReceivedSequenceNumber is absent. Replay detail: "$.TrainingSessionDetail.BillableTokenUsage: missing (expected {'PrefillTokenCount': 0, 'SampleTokenCount': 0, 'TrainingTokenCount': 0}, got None)" and "$.TrainingSessionDetail.LastReceivedSequenceNumber: missing (expected 0, got None)". The same cluster also reports CreationTime/LastModifiedTime as value_mismatch against a shape-only assertion ('<shaped-not-asserted>' vs 1794982241.1778455 / 1794982244.1778455); the recording asserts only their shape, so the requirement is that both members serialize as the protocol's timestamp form for this service rather than any particular instant. Hypothesis: the emulator emits them as fractional epoch seconds where the recording's shape is the service's integral epoch-seconds timestamp — the instants themselves are per-run values and are not a requirement.
Why: a client reading token accounting (cost/quota attribution) or resuming a training stream from the last received sequence number gets None where AWS always gives it a number. Code that indexes BillableTokenUsage.TrainingTokenCount or compares LastReceivedSequenceNumber to its own cursor raises a missing-key/NoneType error, or silently treats consumption as unknown and a fresh session as un-resumable. No usage-based reporting or at-least-once resume logic can be exercised against the emulator.
Fix: the DescribeTrainingSession handler's projection of a stored training session into TrainingSessionDetail, plus the training-session state that must carry a token-usage accumulator and a last-received-sequence-number counter (initialised to zero at create and advanced as training/sampling operations are accepted) for the projection to read; the timestamp sub-point belongs in that response's serializer.
Traces: sagemakertrainingsessionruntime-execute-sampling-operation-generates-text#task#6.

Cause 2 of 2: a stopped training session never reaches the Stopped status — DescribeTrainingSession keeps reporting Running (fixture sagemakertrainingsessionruntime-execute-sampling-operation-generates-text, step teardown#11).
AWS: Smithy StopTrainingSession takes TrainingSessionName and returns an empty output; the session then advances through TrainingSessionStatus, whose modelled values are Created, Blocked, Running, Failed, Scheduled, Stopping, Stopped, Expired, Deleting, DeletionFailed. The recording drives teardown by polling DescribeTrainingSession until TrainingSessionDetail.TrainingSessionStatus is Stopped, so AWS terminates a stopped session within the fixture's polling window (Stopping is a transient state on the way to the terminal Stopped). Status 200 throughout; no error is involved.
Emulator: answers 200 with TrainingSessionDetail.TrainingSessionStatus = "Running" indefinitely. Replay detail: "$.TrainingSessionDetail.TrainingSessionStatus: poll: not in ['Stopped'] after 15 attempt(s) (expected None, got 'Running')" — fifteen consecutive polls all read Running, so the stop request produced no state transition at all rather than a slow one.
Why: a client that stops a session and waits for it to quiesce before deleting it, releasing its capacity, or asserting final token usage waits forever and then times out; every teardown and every stop-then-recreate flow hangs. Because the session is still reported Running, a client also believes it is still being billed and still accepting training operations.
Fix: the StopTrainingSession handler and the training-session lifecycle in the service's state — stopping a session must move its status to Stopping and then settle it in the terminal Stopped (setting EndTime), with the DescribeTrainingSession read reflecting the new status.
Traces: sagemakertrainingsessionruntime-execute-sampling-operation-generates-text#teardown#11.

Not covered by existing open issues: 20261109T123745Z-sagemaker-describetrainingjob-and-describeendpoint-omit-response-members-the and 20261115T093805Z-sagemaker-describetrainingjob-projection-drops-the-algorithm-metric-definitions concern DescribeTrainingJob/DescribeEndpoint, a different resource and operation; the open sagemakertrainingsessionruntime issues (…-resource-resolution-path-diverges-unknown-session-accepted, …-sampling-operations-never-produce-a-result-getsamplingoperationresult) are data-plane findings on a different service token and do not carry these DescribeTrainingSession replay keys.

## Evidence
Cause 1 — fixture sagemakertrainingsessionruntime-execute-sampling-operation-generates-text step task#6, DescribeTrainingSession, expected status 200, actual status 200:
  $.TrainingSessionDetail.BillableTokenUsage: missing (expected {'PrefillTokenCount': 0, 'SampleTokenCount': 0, 'TrainingTokenCount': 0}, got None)
  $.TrainingSessionDetail.LastReceivedSequenceNumber: missing (expected 0, got None)
  $.TrainingSessionDetail.CreationTime: value_mismatch (expected '<shaped-not-asserted>', got 1794982241.1778455)
  $.TrainingSessionDetail.LastModifiedTime: value_mismatch (expected '<shaped-not-asserted>', got 1794982244.1778455)
Model: TrainingSessionDetail declares BillableTokenUsage (structure: PrefillTokenCount/SampleTokenCount/TrainingTokenCount, all long) and LastReceivedSequenceNumber (JavaInteger); CreationTime/LastModifiedTime are Timestamp.

Cause 2 — fixture sagemakertrainingsessionruntime-execute-sampling-operation-generates-text step teardown#11, DescribeTrainingSession, expected status 200, actual status 200:
  $.TrainingSessionDetail.TrainingSessionStatus: poll: not in ['Stopped'] after 15 attempt(s) (expected None, got 'Running')
Expected: status settles in the terminal Stopped (TrainingSessionStatus enum includes Stopping and Stopped). Actual: Running on all 15 polls.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay fixture sagemakertrainingsessionruntime-execute-sampling-operation-generates-text.
3. Cause 1 — at step task#6 the fixture calls sagemaker DescribeTrainingSession for the session it created. Inspect the 200 response body: TrainingSessionDetail has no BillableTokenUsage and no LastReceivedSequenceNumber member (AWS reports both, zero-valued for a session with no accounted traffic), and CreationTime/LastModifiedTime are serialized as fractional epoch seconds.
4. Cause 2 — let the fixture run to teardown#11: it calls StopTrainingSession and then polls DescribeTrainingSession for TrainingSessionStatus == 'Stopped'. Every poll (15 attempts) returns 'Running', so the step never satisfies and teardown times out.
