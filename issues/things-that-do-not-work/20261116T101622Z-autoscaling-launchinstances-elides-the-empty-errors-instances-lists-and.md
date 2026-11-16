Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.launch-instances-idempotent-token-returns-same-result autoscaling.launch-instances-mismatched-token-params-fails
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.launch-instances-idempotent-token-returns-same-result|run#11|KeyError|assert resp1["Errors"] == [], f"unexpected launch errors: {resp1['Errors']}"
Issue-Key: autoscaling.launch-instances-mismatched-token-params-fails|run#11|KeyError|assert resp1["Errors"] == []
Issue-Commit: e65b11926418f2246d75a3bf5dec882b7ebf85de

# LaunchInstances elides the empty Errors/Instances lists and words IdempotentParameterMismatch differently from AWS

> _Note: this issue was identified against the code base at commit `e65b11926418f2246d75a3bf5dec882b7ebf85de`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
LaunchInstances

## Description
Two net-new root causes in Auto Scaling's `LaunchInstances` handler (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/ops/launch_instances.rs). Both are reached by the same two behavioral checks.

Cause 1 of 2: a successful LaunchInstances response omits the `Errors` list instead of serializing it as an empty list, so SDK clients see no `Errors` key at all.
  AWS: Smithy: LaunchInstances output members `Errors` (LaunchInstancesErrors, list) and `Instances` (InstanceCollections, list); the operation doc says it "Returns instance IDs and other details if launch is successful or error details if launch is unsuccessful". The model itself does not state whether an empty list is emitted, so: recorded behavior only — the recording shows a fully successful launch answering HTTP 200 with the `Errors` list PRESENT and empty, which is why botocore surfaces `response["Errors"] == []`. For the awsQuery wire that means the member element is emitted (an empty list wrapper) rather than dropped, for every list member the operation answers.
  Emulator: `launch_instances_output` builds the output with `errors: (!outcome.errors.is_empty()).then_some(outcome.errors)` and `instances: (!outcome.instances.is_empty()).then_some(outcome.instances)`, i.e. an empty list is turned into an absent member. The replayed response therefore carries `AutoScalingGroupName` and `ClientToken` but no `Errors`, and both checks die with `KeyError: 'Errors'` on the first read of the response.
  Why: a client that follows AWS's contract and reads `resp["Errors"]` (the documented way to find the units a launch could not place) raises KeyError / NPE against the emulator on the HAPPY path — every fully successful launch. The same elision hides `Instances` for a launch that placed nothing, so a client cannot distinguish "no instances" from "member missing".
  Fix: the operation's output projection/serializer — emit each modeled list member of LaunchInstancesResult as present-and-empty rather than mapping an empty Vec to None.
  Traces: autoscaling.launch-instances-idempotent-token-returns-same-result, autoscaling.launch-instances-mismatched-token-params-fails (both fail at the first `resp["Errors"]` read).

Cause 2 of 2: the IdempotentParameterMismatch refusal message does not match AWS's wording.
  AWS: Smithy: `IdempotentParameterMismatchError` — errorKind client, httpError 400, awsQueryError code `IdempotentParameterMismatch`. Recorded behavior: reusing a ClientToken with a different `RequestedCapacity` is refused HTTP 400 / `IdempotentParameterMismatch` with the message `Different request parameters found in the last request used with the provided token name` — fixed text in its entirety; it interpolates no value from the request or from state (no token, no capacity, no group name).
  Emulator: the handler returns the same shape/code but with its own sentence, `IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE = "The parameters in the current request do not match the parameters from a previous request with the same client token"` (paraphrased from the shape's documentation rather than the service's message).
  Why: a client (or a test, or a log-matching runbook) that keys on the refusal text AWS emits does not recognise the emulator's refusal, even though the status and code agree.
  Fix: the operation's validation/error construction for the stored-token fingerprint mismatch — carry AWS's fixed sentence.
  Traces: autoscaling.launch-instances-mismatched-token-params-fails (asserts code `IdempotentParameterMismatch` and that exact message; it is currently blocked earlier by Cause 1, so this cause surfaces once Cause 1 is fixed — re-replay it after fixing both).

## Evidence
Check `autoscaling.launch-instances-idempotent-token-returns-same-result` [error]:
  File "check_autoscaling_launch_instances_idempotent_token_returns_same_result.py", line 45, in run
    assert resp1["Errors"] == [], f"unexpected launch errors: {resp1['Errors']}"
  KeyError: 'Errors'

Check `autoscaling.launch-instances-mismatched-token-params-fails` [error]:
  File "check_autoscaling_launch_instances_mismatched_token_params_fails.py", line 43, in run
    assert resp1["Errors"] == []
  KeyError: 'Errors'

Expected (AWS): LaunchInstances on a group with MinSize 0 / MaxSize 10 / DesiredCapacity 0, RequestedCapacity 1, a fresh ClientToken and one SubnetIds entry answers HTTP 200 with `Errors` present and empty and `Instances` carrying the launched instance ids.
Actual (emulator): HTTP 200 whose body has no `Errors` member at all — `errors: (!outcome.errors.is_empty()).then_some(outcome.errors)` in `launch_instances_output` (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/ops/launch_instances.rs:430-441) drops the member when the list is empty; `instances` is dropped the same way.

Expected (AWS, cause 2): reusing that ClientToken with RequestedCapacity 2 -> HTTP 400, Code `IdempotentParameterMismatch` (Smithy awsQueryError), Message exactly `Different request parameters found in the last request used with the provided token name`.
Actual (emulator, cause 2): same code, Message `The parameters in the current request do not match the parameters from a previous request with the same client token` (launch_instances.rs:478-480).

## Steps to Reproduce
1. CreateAutoScalingGroup with MinSize=0, MaxSize=10, DesiredCapacity=0, a LaunchTemplate and VPCZoneIdentifier set to one subnet of the shared network.
2. Call LaunchInstances(AutoScalingGroupName=<group>, RequestedCapacity=1, ClientToken=<fresh token>, SubnetIds=[<that subnet>]).
   -> AWS: HTTP 200 with `Errors` present and empty; emulator: 200 with no `Errors` member (botocore `KeyError: 'Errors'`).
3. Call LaunchInstances again with the SAME ClientToken but RequestedCapacity=2.
   -> AWS: HTTP 400 `IdempotentParameterMismatch`, message `Different request parameters found in the last request used with the provided token name`; emulator returns the same code with a different sentence.
4. Re-replay checks autoscaling.launch-instances-idempotent-token-returns-same-result and autoscaling.launch-instances-mismatched-token-params-fails.
