Issue-Source: trace-sweep-divergence
Issue-Trace: autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals
Issue-Step: task#16
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals autoscaling-asgdx-traj-standby-park-and-return autoscaling-asghc-traj-describe-groups-and-instances-lookup-mistakes autoscaling-pagination-invalid-next-token-errors autoscaling-policy-schedule-hook-notification-tag-lifecycle autoscaling-tags-on-nonexistent-group
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|DescribeAutoScalingInstances|2xx->2xx|$.DescribeAutoScalingInstancesResponse.DescribeAutoScalingInstancesResult.AutoScalingInstances.member.HealthStatus
Issue-Key: autoscaling|DescribeAutoScalingInstances|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|CreateOrUpdateTags|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|DeleteTags|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: addb8a7148576d7d08097a9e3c6488eb5c13818f

# autoscaling describe and tag paths serve emulator-authored strings for HealthStatus, the maxRecords limit and group-not-found

> _Note: this issue was identified against the code base at commit `addb8a7148576d7d08097a9e3c6488eb5c13818f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAutoScalingInstances

## Description
4 causes. On these describe and tag paths the emulator emits strings it authored itself instead of the exact strings Auto Scaling emits: one projected enum-like value with the wrong casing, and three refusal messages whose fixed text differs from AWS's.

Cause 1 of 4: DescribeAutoScalingInstances projects HealthStatus as 'Healthy' instead of 'HEALTHY'.
AWS: Smithy models `AutoScalingInstanceDetails.HealthStatus` as a required XmlStringMaxLen32 and documents the states in prose only ("Healthy means that the instance is healthy and should remain in service. Unhealthy means ..."), so there is no enum to read off the model — recorded behavior only. The recording shows DescribeAutoScalingInstances answering the upper-case token `HEALTHY` for a healthy group member (DescribeAutoScalingGroups' per-instance projection is the mixed-case `Healthy`; the two surfaces differ, and this operation's is upper-case).
Emulator: answers 200 with `HealthStatus` = `Healthy` on this operation, i.e. it reuses the DescribeAutoScalingGroups casing for both.
Why: a client that string-compares the field (the normal way to find unhealthy members, since the member is an unconstrained string) classifies every instance as unknown rather than healthy, so health-gated automation never sees a healthy fleet.
Fix: the DescribeAutoScalingInstances output projection in the autoscaling service crate — the health token must be rendered per-operation, not shared verbatim with the group projection.
Traces: autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals#task#16 (also autoscaling-asgdx-traj-standby-park-and-return#task#10 and #task#15).

Cause 2 of 4: the over-limit MaxRecords refusal is a generic framework constraint message instead of Auto Scaling's sentence.
AWS: Smithy models `MaxRecords` as a plain integer with NO @range trait, so the bound is enforced by the service, not by shape validation; the recording shows AWS refusing MaxRecords above the page cap with HTTP 400 ValidationError and the fixed message `The number of records returned is limited to 50` (the number is the operation's page cap).
Emulator: answers HTTP 400 but with the generic shape-validation sentence `1 validation error detected: Value '51' at 'maxRecords' failed to satisfy constraint: Member must have value less than or equal to 50` — a message AWS does not produce here, since nothing in the model declares that constraint.
Why: a client or test that matches on AWS's limit sentence (or that distinguishes a service limit refusal from a request-shape validation refusal) mis-handles the refusal, and the message points at a modeled constraint that does not exist.
Fix: the DescribeAutoScalingInstances handler's MaxRecords validation (the pagination-bound check in the autoscaling service crate) must raise the service's own ValidationError text instead of going through generic range validation.
Traces: autoscaling-asghc-traj-describe-groups-and-instances-lookup-mistakes#task#10 (also autoscaling-pagination-invalid-next-token-errors#task#2).

Cause 3 of 4: CreateOrUpdateTags' group-not-found refusal re-words AWS's clause.
AWS: recorded behavior — HTTP 400, error code ValidationError, message `AutoScalingGroup not found - AutoScaling Group <group-name> not found`. Fixed text: the prefix `AutoScalingGroup not found - AutoScaling Group ` and the trailing ` not found`; the only variable part is the group name taken from the request's Tags entry ResourceId.
Emulator: answers HTTP 400 with `AutoScalingGroup name not found - AutoScalingGroup <group-name> not found` — it inserts `name` into the prefix and spells the second occurrence as one word, so neither half of the fixed text matches.
Why: a client matching the refusal text to distinguish a missing group from another ValidationError (the code is the same for both) does not match, and retries or fails on the wrong branch.
Fix: the CreateOrUpdateTags handler's not-found refusal in the autoscaling service crate's tag path — it must use Auto Scaling's clause verbatim.
Traces: autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#1 (also autoscaling-tags-on-nonexistent-group#task#1).

Cause 4 of 4: DeleteTags' group-not-found refusal re-words the same clause.
AWS: recorded behavior — HTTP 400, ValidationError, message `AutoScalingGroup not found - AutoScaling Group <group-name> not found`, same fixed text and same single variable part as Cause 3.
Emulator: answers HTTP 400 with `AutoScalingGroup name not found - AutoScalingGroup <group-name> not found`.
Why: same as Cause 3 — the delete path's missing-group refusal is not recognizable by its text.
Fix: the DeleteTags handler's not-found refusal; it shares the tag path's lookup helper with CreateOrUpdateTags, so the clause should be fixed once there.
Traces: autoscaling-policy-schedule-hook-notification-tag-lifecycle#task#3 (also autoscaling-tags-on-nonexistent-group#task#2).

Related but NOT covering these: open issues 20261116T221312Z-autoscaling-refusal-messages-diverge-from-aws-s-fixed.md and 20261114T180324Z-autoscaling-group-scoped-not-found-refusals-re-word.md describe the same family of defect on other operations but do not carry the replay keys above, so all four causes are net-new.

## Evidence
Cause 1 — fixture autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals, step task#16, DescribeAutoScalingInstances, 200 vs 200:
  $.DescribeAutoScalingInstancesResponse.DescribeAutoScalingInstancesResult.AutoScalingInstances.member.HealthStatus: expected 'HEALTHY', got 'Healthy' (cluster also covers autoscaling-asgdx-traj-standby-park-and-return#task#10, #task#15)
  Smithy: AutoScalingInstanceDetails.HealthStatus -> XmlStringMaxLen32 (required), prose-only states; no enum.

Cause 2 — fixture autoscaling-asghc-traj-describe-groups-and-instances-lookup-mistakes, step task#10, DescribeAutoScalingInstances, 400 vs 400:
  $.ErrorResponse.Error.Message: expected 'The number of records returned is limited to 50', got "1 validation error detected: Value '51' at 'maxRecords' failed to satisfy constraint: Member must have value less than or equal to 50" (cluster also covers autoscaling-pagination-invalid-next-token-errors#task#2)
  Smithy: com.amazonaws.autoscaling#MaxRecords is an integer with no @range trait.

Cause 3 — fixture autoscaling-policy-schedule-hook-notification-tag-lifecycle, step task#1, CreateOrUpdateTags, 400 vs 400:
  $.ErrorResponse.Error.Message: expected 'AutoScalingGroup not found - AutoScaling Group <name> not found', got 'AutoScalingGroup name not found - AutoScalingGroup <name> not found' (cluster also covers autoscaling-tags-on-nonexistent-group#task#1)

Cause 4 — fixture autoscaling-policy-schedule-hook-notification-tag-lifecycle, step task#3, DeleteTags, 400 vs 400:
  $.ErrorResponse.Error.Message: same expected vs actual as Cause 3 (cluster also covers autoscaling-tags-on-nonexistent-group#task#2)

## Steps to Reproduce
1. Replay fixture autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals, step task#16: read AutoScalingInstances.member.HealthStatus from the DescribeAutoScalingInstances response and compare its casing to the recording.
2. Replay fixture autoscaling-asghc-traj-describe-groups-and-instances-lookup-mistakes, step task#10: call DescribeAutoScalingInstances with MaxRecords above the page cap and compare ErrorResponse.Error.Message.
3. Replay fixture autoscaling-policy-schedule-hook-notification-tag-lifecycle, steps task#1 and task#3: CreateOrUpdateTags and DeleteTags against a nonexistent group name and compare ErrorResponse.Error.Message.
4. All steps are in the simparity-wire-aws-v1 corpus; replay_wire_corpus reports them as four mismatch clusters on autoscaling.
