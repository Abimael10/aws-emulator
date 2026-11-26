Issue-Source: trace-sweep-divergence
Issue-Trace: autoscaling-asgdx-traj-lifecycle-hook-on-live-group-mistakes
Issue-Step: task#15
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscaling-asgdx-traj-lifecycle-hook-on-live-group-mistakes autoscaling-instance-level-operations-negative autoscaling-lifecycle-hook-operations-on-nonexistent-group autoscaling-asghc-traj-health-check-type-and-grace-update-refusals
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|CompleteLifecycleAction|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|UpdateAutoScalingGroup|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: autoscaling|RecordLifecycleActionHeartbeat|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 991354efed443a569b426643157f0d67ea150cda

# Lifecycle-action and health-check update refusals carry the wrong error code and emulator-authored text (3 causes)

> _Note: this issue was identified against the code base at commit `991354efed443a569b426643157f0d67ea150cda`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CompleteLifecycleAction

## Description
Three net-new causes in the autoscaling Query-protocol refusal plane. Every one answers the correct HTTP status (400); what diverges is the <Code> the error is classified under, the fixed <Message> text AWS uses for that error class, and — on one operation — which of two checks runs first. Each cause must be closed separately; the three are bundled only because they live in the same refusal plane of one service.

Cause 1 of 3 — CompleteLifecycleAction: the "neither token nor instance id" refusal is emulator-authored prose.
AWS: Smithy (com.amazonaws.autoscaling#AutoScaling_2011_01_01): CompleteLifecycleAction.LifecycleHookName, .AutoScalingGroupName and .LifecycleActionResult are required; .LifecycleActionToken (LifecycleActionToken, @length 36..36) and .InstanceId (XmlStringMaxLen19) are BOTH optional, and the model states no "exactly one of" constraint — so the requirement that one of the two be supplied is a server-side validation whose refusal is recorded behavior only. The recording shows HTTP 400 with awsQueryError code ValidationError and the message 'No Lifecycle Action found because no token or instance ID is provided'. That sentence is entirely fixed text: it has NO variable parts — neither the hook name, the group name nor any request value is interpolated.
Emulator: HTTP 400 with the message 'Either the lifecycle action token or the instance ID must be specified.' — the check fires at the right point and the status and code are right, but the text is emulator-authored (different wording, different shape, trailing period) rather than AWS's fixed sentence.
Why: a client (or a recorded-golden comparison, or a retry/branching rule) that matches AWS's sentence does not recognize the refusal and treats it as an unknown failure; the Query error code alone does not distinguish this refusal from the other ValidationErrors the operation can raise, so the message IS the discriminator.
Fix: the CompleteLifecycleAction handler's input validation (the token/instance-id presence check) — emit AWS's fixed sentence verbatim, with no interpolation.
Traces: autoscaling-asgdx-traj-lifecycle-hook-on-live-group-mistakes#task#15; also autoscaling-instance-level-operations-negative#task#8, autoscaling-lifecycle-hook-operations-on-nonexistent-group#task#4.

Cause 2 of 3 — UpdateAutoScalingGroup: a malformed parameter value is classified ValidationError instead of MalformedInput.
AWS: Smithy: UpdateAutoScalingGroup's modeled errors are ResourceContentionFault, ScalingActivityInProgressFault and ServiceLinkedRoleFailure; HealthCheckType targets XmlStringMaxLen32 and HealthCheckGracePeriod targets the integer HealthCheckGracePeriod, so a value that cannot be coerced into the modeled type is rejected by the Query front-end before any operation-level validation runs. Recorded behavior for that case: HTTP 400 with ErrorResponse.Error.Code 'MalformedInput' (the protocol-level deserialization refusal), and the message the emulator already reproduces correctly. Hypothesis: the offending step supplies a HealthCheckGracePeriod/HealthCheckType value that is not parseable into its modeled type, which is why AWS answers at the protocol layer rather than from the handler.
Emulator: HTTP 400 with the same message but ErrorResponse.Error.Code 'ValidationError' — it funnels every 400 on this operation through the handler's generic validation classification, so a protocol-layer deserialization failure is reported as a semantic validation failure.
Why: Error.Code is the field SDKs and clients branch on (exception class selection, retry/no-retry classification, error-code assertions). A client that distinguishes a malformed request (MalformedInput, never retriable, a client-side serialization bug) from a semantically invalid one (ValidationError) sees the wrong class and cannot tell a bad encoding from a bad value.
Fix: the Query request-deserialization layer shared by the autoscaling operations (where a parameter value that fails to coerce into its modeled shape is turned into an error) — it must classify that failure as MalformedInput rather than falling through to the handler's ValidationError.
Traces: autoscaling-asghc-traj-health-check-type-and-grace-update-refusals#task#8.

Cause 3 of 3 — RecordLifecycleActionHeartbeat: the group lookup pre-empts the token/instance-id presence check.
AWS: Smithy: RecordLifecycleActionHeartbeat.LifecycleHookName and .AutoScalingGroupName are required, .LifecycleActionToken and .InstanceId are both optional. Recorded behavior: for a request that names a group that does not exist AND supplies neither a LifecycleActionToken nor an InstanceId, AWS answers HTTP 400 with the fixed sentence 'No Lifecycle Action found because no token or instance ID is provided' — i.e. the argument-presence check runs BEFORE the group lookup, and the refusal names the missing argument, not the missing group. Like cause 1's sentence, it has no variable parts.
Emulator: HTTP 400 whose message is 'AutoScalingGroup name not found - No such Auto Scaling group: <group-name>' — it resolves the group first and reports the group as nonexistent, never reaching the presence check (note that on CompleteLifecycleAction, cause 1, the emulator DOES run the presence check first, so the two sibling operations also disagree with each other).
Why: the caller is told the Auto Scaling group does not exist when the actual fault is an incomplete request, so remediation goes down the wrong path (recreate/await the group) instead of supplying the token or instance id; and a client keying off AWS's no-lifecycle-action sentence never fires. It also makes the operation's validation order observably different from its sibling.
Fix: RecordLifecycleActionHeartbeat's handler — order the token/instance-id presence validation ahead of the group lookup in state, and emit AWS's fixed sentence.
Traces: autoscaling-instance-level-operations-negative#task#9; also autoscaling-lifecycle-hook-operations-on-nonexistent-group#task#3.

Related but non-covering open issues (none carries these steps' replay keys): 20261116T221312Z-autoscaling-refusal-messages-diverge-from-aws-s-fixed.md and 20261117T053015Z-autoscaling-refusal-messages-and-validation-precedence-diverge-on.md cover the metrics-collection, group-not-found, scheduled-action, policy and traffic-source refusals; 20261111T132217Z-autoscaling-lifecycle-hooks-are-stored-but-never-executed.md covers hooks never being executed, not these refusal texts. The group names and parameter values quoted here are one recording's request values filling the templates — only the fixed text, the error codes and the templated positions are the requirement.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout:
Cause 1 — fixture autoscaling-asgdx-traj-lifecycle-hook-on-live-group-mistakes, step task#15, CompleteLifecycleAction, status expected 400 / got 400; $.ErrorResponse.Error.Message value_mismatch: expected 'No Lifecycle Action found because no token or instance ID is provided', got 'Either the lifecycle action token or the instance ID must be specified.' (cluster of 3 steps: also autoscaling-instance-level-operations-negative#task#8, autoscaling-lifecycle-hook-operations-on-nonexistent-group#task#4).
Cause 2 — fixture autoscaling-asghc-traj-health-check-type-and-grace-update-refusals, step task#8, UpdateAutoScalingGroup, status expected 400 / got 400; $.ErrorResponse.Error.Code value_mismatch: expected 'MalformedInput', got 'ValidationError' (message matched; 1 step in cluster).
Cause 3 — fixture autoscaling-instance-level-operations-negative, step task#9, RecordLifecycleActionHeartbeat, status expected 400 / got 400; $.ErrorResponse.Error.Message value_mismatch: expected 'No Lifecycle Action found because no token or instance ID is provided', got 'AutoScalingGroup name not found - No such Auto Scaling group: sim2real-probe-autoscaling-missing-asg' (cluster of 2 steps: also autoscaling-lifecycle-hook-operations-on-nonexistent-group#task#3).
The group name in cause 3's actual message is that recording's request value; the expected messages in causes 1 and 3 are wholly fixed text with no interpolated values.

## Steps to Reproduce
1. Replay the wire corpus (trace set simparity-wire-aws-v1) against the emulator.
2. Cause 1: fixture autoscaling-asgdx-traj-lifecycle-hook-on-live-group-mistakes, step task#15 — CompleteLifecycleAction naming an existing group and hook with LifecycleActionResult set but neither LifecycleActionToken nor InstanceId; read ErrorResponse.Error.Message.
3. Cause 2: fixture autoscaling-asghc-traj-health-check-type-and-grace-update-refusals, step task#8 — UpdateAutoScalingGroup whose health-check parameter value cannot be coerced into its modeled type; read ErrorResponse.Error.Code.
4. Cause 3: fixture autoscaling-instance-level-operations-negative, step task#9 — RecordLifecycleActionHeartbeat naming a group that does not exist and supplying neither LifecycleActionToken nor InstanceId; read ErrorResponse.Error.Message.
In each case compare the HTTP status, ErrorResponse.Error.Code and ErrorResponse.Error.Message against the fixture's expected response.
