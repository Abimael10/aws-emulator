Issue-Source: trace-sweep-divergence
Issue-Trace: asg-err-metrics-granularity
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: asg-err-metrics-granularity autoscaling-group-configuration-and-topology-negative autoscaling-group-dependent-operations-on-nonexistent-group autoscaling-group-and-policy-dependent-operations-on-nonexistent-group autoscaling-instance-level-operations-negative
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|EnableMetricsCollection|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|LaunchInstances|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|DisableMetricsCollection|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 93ff44361a038ac1da9d65dc0eee101932df58d5

# autoscaling refusal messages diverge from AWS's fixed text on metrics-collection and group-not-found paths (3 causes)

> _Note: this issue was identified against the code base at commit `93ff44361a038ac1da9d65dc0eee101932df58d5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
EnableMetricsCollection

## Description
Three net-new causes share one root fault: the autoscaling query-protocol error path returns the right HTTP status (400) and the right error Code, but the Error.Message is emulator-authored prose instead of the fixed sentence AWS emits for that refusal class. In two of the three the group name that AWS interpolates into the message is either re-framed or dropped entirely (serialized as the literal 'null'), so the message carries no usable identity of the offending resource.

These are the autoscaling error-message refusals left over after the already-open message-fidelity issues; none of the three steps below is covered by an existing issue's replay key. Adjacent open issues that describe the same FAMILY but not these operations/steps: 20261114T180324Z-autoscaling-group-scoped-not-found-refusals-re-word.md, 20261110T100847Z-autoscaling-refusal-fidelity-wrong-codes-and-emulator-authored.md, 20261116T101622Z-autoscaling-launchinstances-elides-the-empty-errors-instances-lists-and.md. Each cause below must be closed on its own fixture/step.

=== Cause 1 of 3: EnableMetricsCollection invalid Granularity message is re-worded ===
Fixture asg-err-metrics-granularity, step task#8.
AWS: recorded behavior only — the Smithy model (AutoScaling_2011_01_01, awsQuery) declares EnableMetricsCollection with required input member Granularity (XmlStringMaxLen255, @length 1..255) and models only ResourceContentionFault, so the refusal message for an unsupported granularity is not in the model. The recording shows HTTP 400 with Error.Message 'Valid metrics granularity type is: [1Minute].' Every part of that sentence is FIXED text (including the singular 'granularity type is'); the only variable part is the bracketed set of granularity values the service supports, which is the enumeration of accepted Granularity values, not a per-run value.
Emulator: HTTP 400 with Error.Message 'Valid metric granularity values are: [1Minute].' — 'metric' instead of 'metrics', and 'granularity values are' instead of 'granularity type is'.
Why: a client that matches on the refusal sentence (or asserts on it in a test/fixture, or surfaces it to an operator) cannot recognize the AWS granularity refusal. The bracketed list happens to agree, so the divergence is purely in the sentence AWS fixes.
Fix: the EnableMetricsCollection handler's Granularity validation in the autoscaling crate — the message template it builds for the unsupported-granularity refusal.
Traces: asg-err-metrics-granularity#task#8; also autoscaling-group-configuration-and-topology-negative#task#5, autoscaling-group-dependent-operations-on-nonexistent-group#task#3.

=== Cause 2 of 3: LaunchInstances group-not-found message is re-framed ===
Fixture autoscaling-group-and-policy-dependent-operations-on-nonexistent-group, step task#9.
AWS: recorded behavior only — the model declares LaunchInstances with required AutoScalingGroupName and models only IdempotentCallInProgressFault, IdempotentParameterMismatchError and ResourceContentionFault, so the not-found refusal message is not modeled. The recording shows HTTP 400 with Error.Message of the form 'AutoScalingGroup not found - No such Auto Scaling group: <AutoScalingGroupName>'. Fixed text: 'AutoScalingGroup not found - No such Auto Scaling group: '. Variable part: the group name taken verbatim from the request's AutoScalingGroupName (the recorded run happens to carry sim2real-probe-autoscaling-asg-f393fafd; that value is not part of the requirement).
Emulator: HTTP 400 with Error.Message 'AutoScalingGroup name not found - AutoScalingGroup <name> not found' — a different fixed prefix ('AutoScalingGroup name not found - ') and a different trailing clause ('AutoScalingGroup <name> not found' rather than 'No such Auto Scaling group: <name>').
Why: the refusal no longer matches AWS's not-found sentence, so a client keying off the message cannot distinguish 'no such group' from the other ValidationError refusals this operation can return, and tooling that parses the group name out of the AWS clause reads the wrong token.
Fix: the group-lookup refusal in the LaunchInstances handler in the autoscaling crate — the shared not-found message template it formats.
Traces: autoscaling-group-and-policy-dependent-operations-on-nonexistent-group#task#9; also autoscaling-instance-level-operations-negative#task#6.

=== Cause 3 of 3: DisableMetricsCollection group-not-found message drops the group name and emits 'null' ===
Fixture autoscaling-group-configuration-and-topology-negative, step task#6.
AWS: recorded behavior only — the model declares DisableMetricsCollection with required AutoScalingGroupName (XmlStringMaxLen255, @length 1..255) and models only ResourceContentionFault. The recording shows HTTP 400 with Error.Message of the form 'Group <AutoScalingGroupName> not found'. Fixed text: 'Group ' and ' not found'. Variable part: the group name from the request's AutoScalingGroupName (the recorded run carries sim2real-probe-autoscaling-missing-asg; that value is not the requirement).
Emulator: HTTP 400 with Error.Message 'AutoScalingGroup name not found - null'. Both the fixed text and the interpolation diverge: the sentence is an emulator-authored prefix, and the requested group name is not interpolated at all — the message serializes the absent value as the literal string 'null'.
Why: the refusal names no resource. A client cannot tell WHICH group was missing, and the literal 'null' is not a value any AWS message produces, so log scraping and message assertions both break. This is distinct from cause 2: a different operation and a different AWS sentence ('Group <name> not found', not the 'No such Auto Scaling group:' clause).
Fix: the group-lookup refusal in the DisableMetricsCollection handler in the autoscaling crate — it must resolve the requested AutoScalingGroupName into the message rather than formatting an unset option.
Traces: autoscaling-group-configuration-and-topology-negative#task#6; also autoscaling-group-dependent-operations-on-nonexistent-group#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1 (89 fixtures, 1044 steps, score 76.4%) reported these three clusters with no owning issue and no blocking stub:

Cause 1 — autoscaling EnableMetricsCollection, status 400 -> 400, key $.ErrorResponse.Error.Message:
  expected: 'Valid metrics granularity type is: [1Minute].'
  actual:   'Valid metric granularity values are: [1Minute].'
  cluster: asg-err-metrics-granularity#task#8, autoscaling-group-configuration-and-topology-negative#task#5, autoscaling-group-dependent-operations-on-nonexistent-group#task#3 (3 steps)

Cause 2 — autoscaling LaunchInstances, status 400 -> 400, key $.ErrorResponse.Error.Message:
  expected: 'AutoScalingGroup not found - No such Auto Scaling group: <group-name-from-request>'
  actual:   'AutoScalingGroup name not found - AutoScalingGroup <group-name-from-request> not found'
  cluster: autoscaling-group-and-policy-dependent-operations-on-nonexistent-group#task#9, autoscaling-instance-level-operations-negative#task#6 (2 steps)

Cause 3 — autoscaling DisableMetricsCollection, status 400 -> 400, key $.ErrorResponse.Error.Message:
  expected: 'Group <group-name-from-request> not found'
  actual:   'AutoScalingGroup name not found - null'  (request group name never interpolated)
  cluster: autoscaling-group-configuration-and-topology-negative#task#6, autoscaling-group-dependent-operations-on-nonexistent-group#task#4 (2 steps)

In all three the HTTP status (400) and the error Code agree with the recording; only $.ErrorResponse.Error.Message diverges. Model grounding: services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json — service AutoScaling_2011_01_01, protocol awsQuery; EnableMetricsCollection/DisableMetricsCollection/LaunchInstances each require AutoScalingGroupName and model no ValidationError, so the refusal wording is recorded behavior only.

## Steps to Reproduce
1. git clone the project repo and build/boot the emulator from the checkout.
2. Replay trace set simparity-wire-aws-v1 against it.
3. Cause 1: replay fixture asg-err-metrics-granularity step task#8 — call autoscaling EnableMetricsCollection on an existing group with a Granularity value the service does not support and compare $.ErrorResponse.Error.Message against 'Valid metrics granularity type is: [1Minute].'
4. Cause 2: replay fixture autoscaling-group-and-policy-dependent-operations-on-nonexistent-group step task#9 — call autoscaling LaunchInstances with an AutoScalingGroupName that does not exist and compare $.ErrorResponse.Error.Message against 'AutoScalingGroup not found - No such Auto Scaling group: <that group name>'.
5. Cause 3: replay fixture autoscaling-group-configuration-and-topology-negative step task#6 — call autoscaling DisableMetricsCollection with an AutoScalingGroupName that does not exist and compare $.ErrorResponse.Error.Message against 'Group <that group name> not found'; the emulator currently answers 'AutoScalingGroup name not found - null'.
6. All three must pass before closing: each cause has its own fixture#step above and the other cluster members listed under Evidence.
