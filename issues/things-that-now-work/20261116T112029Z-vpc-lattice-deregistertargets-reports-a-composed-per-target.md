Issue-Source: trace-sweep-divergence
Issue-Trace: vpclattice-target-group-register-deregister-targets
Issue-Step: task#11
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: vpclattice-target-group-register-deregister-targets
Issue-Shape-Id: com.amazonaws.vpclattice#MercuryControlPlane
Issue-Key: vpc-lattice|DeregisterTargets|2xx->2xx|$.unsuccessful[*].failureMessage
Issue-Commit: b69d07d1292c6efabbbd9d453bbd6c69decab13d

# vpc-lattice DeregisterTargets reports a composed per-target failureMessage instead of AWS's fixed 'Target failed to deregister.'

> _Note: this issue was identified against the code base at commit `b69d07d1292c6efabbbd9d453bbd6c69decab13d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeregisterTargets

## Description
Cause 1 of 1: DeregisterTargets puts the wrong text in the per-target `unsuccessful[].failureMessage` when a target cannot be deregistered (here a LAMBDA target the target group holds no registration of). The HTTP status (200), the response shape (`successful`/`unsuccessful`) and the entry's `id`/`port`/`failureCode` all match; only the message text diverges.

AWS: Smithy (services/service-com-amazonaws-vpclattice-mercurycontrolplane/smithy-model/com-amazonaws-vpclattice-mercurycontrolplane.json) models DeregisterTargets as POST /targetgroups/{targetGroupIdentifier}/deregistertargets returning HTTP 200 with members `successful` (TargetList) and `unsuccessful` (TargetFailureList); TargetFailure carries `id`, `port`, `failureCode` ('The failure code.') and `failureMessage` ('The failure message.'). The model does not state the message text, so for that text this is recorded behavior only: the recording shows AWS answering 200 and reporting the target it could not deregister under `unsuccessful` with failureMessage exactly 'Target failed to deregister.' That string is ENTIRELY fixed text for this failure class — it interpolates nothing from the request or from service state (no target id, no ARN, no target-group id), so the same text is expected for any target of any target-group type that the operation cannot deregister.

Emulator: the same step answers 200 but with failureMessage 'Lambda <target-arn> is not registered with target group <tg-id>.' — a message the emulator composes from the group type, the submitted target id and the target-group id (crate::state::not_registered_message, used at services/service-com-amazonaws-vpclattice-mercurycontrolplane/src/ops/deregister_targets.rs around the `unsuccessful.push(TargetFailure { .. })` branch). Its fixed parts ('Lambda ', ' is not registered with target group ', '.') and its two interpolated values are all absent from AWS's wording; in the recorded step the interpolated values were the Lambda function ARN and the target group id, which are request/state values and not part of any requirement.

Why: a client that surfaces, logs, matches or asserts on the per-target failure message — the only human-readable explanation VPC Lattice gives for a partially failed batch — sees text AWS never emits. Tooling and tests that compare the message, and golden-output/snapshot comparisons of a deregister response, diverge against the emulator even though the status and codes line up; the longer composed message also leaks an identifier shape AWS does not put in this field.

Fix: the DeregisterTargets handler's per-target failure construction (services/service-com-amazonaws-vpclattice-mercurycontrolplane/src/ops/deregister_targets.rs) together with the shared message builder it calls in services/service-com-amazonaws-vpclattice-mercurycontrolplane/src/state.rs: emit AWS's fixed failure text for a target that cannot be deregistered rather than a per-target composed 'not registered with target group' sentence. Note the open issue 20261109T141403Z-lambda-vpc-lattice-vpc-lattice-registertargets-answers-notimplemented.md names the sibling RegisterTargets operation, and 20261101T145430Z-vpc-lattice-listener-rule-update-path-wrong-batchupdaterule.md names a different per-entry failure message (BatchUpdateRule); neither covers this DeregisterTargets message.

Traces: vpclattice-target-group-register-deregister-targets#task#11.

## Evidence
Fixture vpclattice-target-group-register-deregister-targets, step task#11 (DeregisterTargets), verdict mismatch, expected status 200 / actual status 200.
Diverging key $.unsuccessful[0].failureMessage:
  Expected (AWS, recorded): "Target failed to deregister."
  Actual (emulator): "Lambda arn:aws:lambda:us-west-2:000000000000:function:sim2real-probe-lat-tgrd-fn-517051b3 is not registered with target group tg-020212a28f8a9a0e3."
Fixed vs variable: AWS's message is all fixed text ('Target failed to deregister.') with no interpolation. The emulator's message is fixed text 'Lambda ' + ' is not registered with target group ' + '.' with the Lambda function ARN (from the request) and the target group id (from state) interpolated — the specific ARN and tg- id above are one run's values, not requirements.
Model: TargetFailure.failureMessage is 'The failure message.' (String); the model fixes no text, so the recorded AWS string is the requirement for this failure class.

## Steps to Reproduce
1. Point a vpc-lattice client at the emulator.
2. Create a LAMBDA target group and register a Lambda function ARN with it (RegisterTargets), then deregister it (DeregisterTargets) so the group no longer holds the registration.
3. Call DeregisterTargets again for the same target: POST /targetgroups/<tg-id>/deregistertargets with targets=[{"id": "<lambda-function-arn>"}].
4. Observe HTTP 200 with unsuccessful[0].failureMessage = 'Lambda <arn> is not registered with target group <tg-id>.' instead of AWS's 'Target failed to deregister.'
5. Replay fixture vpclattice-target-group-register-deregister-targets, step task#11, to confirm.
