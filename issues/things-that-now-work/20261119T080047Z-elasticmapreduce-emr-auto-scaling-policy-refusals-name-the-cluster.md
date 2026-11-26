Issue-Source: trace-sweep-divergence
Issue-Trace: elasticmapreduce-cluster-scaling-and-policy-errors
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticmapreduce-cluster-scaling-and-policy-errors
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: elasticmapreduce|PutAutoScalingPolicy|4xx->4xx|$.message
Issue-Key: elasticmapreduce|RemoveAutoScalingPolicy|4xx->4xx|$.message
Issue-Commit: 8465be9c60d4839857b29976b5cf2d08632066fc

# EMR auto-scaling policy refusals name the cluster id instead of the unresolved instance group id

> _Note: this issue was identified against the code base at commit `8465be9c60d4839857b29976b5cf2d08632066fc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutAutoScalingPolicy

## Description
This issue bundles 2 net-new root causes in the EMR crate (service token `elasticmapreduce`) that share ONE fault: the instance-group-scoped auto-scaling operations refuse an unresolvable reference by reporting the WRONG identifier. Real EMR resolves the instance group named by the request and, when it cannot, refuses with a sentence naming that InstanceGroupId; the emulator instead refuses with a sentence naming the request's ClusterId. The HTTP status (400) and the exception shape match in both causes — only the identifier and the sentence template diverge, so the client is told the wrong thing is wrong.

In both causes the FIXED text of the message is the requirement; the single-quoted id is a variable part taken from the request. The concrete ids in the recordings (`ig-1PT4EYFX3F2NM`, `j-3SD91U2P1PANK`) are per-run request values and are NOT part of the template.

Cause 1 of 2: PutAutoScalingPolicy reports the cluster id rather than the instance group id.
AWS: Smithy (com.amazonaws.emr#ElasticMapReduce): PutAutoScalingPolicy input ClusterId (required, @length 0..256), InstanceGroupId (required, @length 0..256), AutoScalingPolicy (required); the model declares NO errors for this operation, so the 400 refusal text is recorded behavior only. The recording shows the request refused with HTTP 400 and message `Instance group id '<InstanceGroupId>' is not valid.` — fixed text `Instance group id '` and `' is not valid.`, variable part: the InstanceGroupId copied from the request. AWS reports the instance-group reference it could not resolve, which is the member that scopes the policy.
Emulator: HTTP 400 with `Cluster id '<ClusterId>' is not valid.` — a different sentence template, naming the request's ClusterId instead of its InstanceGroupId.
Why: a caller attaching an auto-scaling policy is told its cluster reference is bad when the cluster reference is the part it got right, so it retries against the same (correct) cluster or re-resolves the cluster id instead of fixing the instance group it actually mistyped; the returned message also no longer contains the instance group id, so a client or runbook that extracts the offending id from the message to re-resolve it (the message is the only human-visible discriminator here, since no typed error member carries the id) extracts the wrong one and matches on the wrong template.
Fix: the reference-resolution/validation branch of the PutAutoScalingPolicy handler in the EMR crate's operation handlers — it must validate and report the InstanceGroupId reference, rendering `Instance group id '<id>' is not valid.` with the request's InstanceGroupId, rather than falling through to the shared cluster-not-found sentence.
Traces: elasticmapreduce-cluster-scaling-and-policy-errors#task#5.

Cause 2 of 2: RemoveAutoScalingPolicy reports the cluster id rather than the instance group id.
AWS: Smithy: RemoveAutoScalingPolicy input ClusterId (required, @length 0..256), InstanceGroupId (required, @length 0..256), empty output; again no modeled errors, so the refusal text is recorded behavior only. The recording shows HTTP 400 with message `Instance group id '<InstanceGroupId>' is not valid.` — same fixed text and same variable part as cause 1, confirming the template is shared by the instance-group-scoped auto-scaling operations.
Emulator: HTTP 400 with `Cluster id '<ClusterId>' is not valid.`, i.e. the same wrong-identifier substitution as the Put path, so a client cleaning up a policy gets the same misdirection on the detach path as on the attach path.
Why: removal is idempotent-ish housekeeping; being told the CLUSTER id is invalid reads as the whole cluster being gone, which can make a teardown script abort or skip the remaining per-instance-group cleanup, where AWS's message tells it precisely which instance group reference to drop.
Fix: the same reference-resolution/validation branch for RemoveAutoScalingPolicy in the EMR crate — both operations should report the unresolved instance group, so the fix belongs in the shared instance-group lookup used by the auto-scaling handlers, not in the cluster lookup.
Traces: elasticmapreduce-cluster-scaling-and-policy-errors#task#6.

Related but NOT covering: open issue 20261118T033253Z-elasticmapreduce-emr-authors-its-own-response-text-and-shape.md and 20261112T132121Z-elasticmapreduce-emr-request-validation-and-refusal-paths-diverge-placement.md describe the same family of authored/reworded EMR refusal text, but neither mentions the auto-scaling operations and neither carries a replay key for these two steps.

## Evidence
Replay of wire corpus simparity-wire-aws-v1 against the booted emulator (summary: 390 steps, 361 passed, 29 mismatched, score 92.6%, emulator_up true, bootstrap_signal false). Both clusters below are net-new: empty `stub_path` and empty `owned_by`.

Cause 1 — fixture `elasticmapreduce-cluster-scaling-and-policy-errors`, step `task#5`, PutAutoScalingPolicy, expected status 400 / actual 400:
  `$.message: value_mismatch (expected "Instance group id 'ig-1PT4EYFX3F2NM' is not valid.", got "Cluster id 'j-3SD91U2P1PANK' is not valid.")`
  Fixed text (the requirement): `Instance group id '` ... `' is not valid.`; variable part: the InstanceGroupId from the request. `ig-1PT4EYFX3F2NM` and `j-3SD91U2P1PANK` are per-run request values, not requirements.

Cause 2 — fixture `elasticmapreduce-cluster-scaling-and-policy-errors`, step `task#6`, RemoveAutoScalingPolicy, expected status 400 / actual 400:
  `$.message: value_mismatch (expected "Instance group id 'ig-1PT4EYFX3F2NM' is not valid.", got "Cluster id 'j-3SD91U2P1PANK' is not valid.")`
  Same template and same substitution defect as cause 1.

Status code (400) and the error shape agree with the recording in both steps; the only diverging key is `$.message`.

## Steps to Reproduce
Boot the emulator and replay these steps from wire trace set simparity-wire-aws-v1 (service token `elasticmapreduce`):
1. Fixture `elasticmapreduce-cluster-scaling-and-policy-errors`, step `task#5`: call PutAutoScalingPolicy with a ClusterId and an InstanceGroupId that the emulator cannot resolve to a stored instance group, plus any valid AutoScalingPolicy. Assert HTTP 400 and that the message is `Instance group id '<the requested InstanceGroupId>' is not valid.` — it must interpolate the request's InstanceGroupId, not the ClusterId, and must not use the `Cluster id '...' is not valid.` template.
2. Fixture `elasticmapreduce-cluster-scaling-and-policy-errors`, step `task#6`: call RemoveAutoScalingPolicy with the same ClusterId/InstanceGroupId pair. Assert HTTP 400 and the identical `Instance group id '<the requested InstanceGroupId>' is not valid.` message.
3. Re-replay the whole fixture to confirm both steps pass with the status unchanged at 400.
