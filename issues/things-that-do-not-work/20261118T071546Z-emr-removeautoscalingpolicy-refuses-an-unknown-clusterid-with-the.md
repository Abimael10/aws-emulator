Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.remove-autoscaling-policy-without-existing-policy-rejected
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr.remove-autoscaling-policy-without-existing-policy-rejected|run#22|AssertionError|assert message2 == f"Cluster id '{bogus_cluster_id}' is not valid.", (
Issue-Commit: c87e0070262747e387e8822a4b20ee702e43e688

# emr RemoveAutoScalingPolicy refuses an unknown ClusterId with the instance-group-not-valid sentence instead of EMR's cluster-not-valid sentence

> _Note: this issue was identified against the code base at commit `c87e0070262747e387e8822a4b20ee702e43e688`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RemoveAutoScalingPolicy

## Description
One net-new root cause.

Cause 1 of 1: RemoveAutoScalingPolicy resolves the cluster and the instance group against a SINGLE message template, so a request naming a cluster the account does not hold is refused in terms of the instance group instead of the cluster. The cluster-not-found case and the group-not-found case are therefore indistinguishable to a client. Failing check id: emr.remove-autoscaling-policy-without-existing-policy-rejected.

AWS:      Smithy: RemoveAutoScalingPolicy input is {ClusterId (required, @length 0..256), InstanceGroupId (required, @length 0..256)}; the operation's modeled `errors` list is EMPTY and the output shape has no members, so the model states nothing about refusals — recorded behavior only. The recording shows EMR answering HTTP 400 `ValidationException` on both unresolvable-target paths, with two DIFFERENT, target-specific messages: for an instance group that carries no policy, `Auto Scaling policy was not found for instance group '<InstanceGroupId>'.`; for a ClusterId the account holds no cluster for, `Cluster id '<ClusterId>' is not valid.` In the cluster sentence the fixed text is `Cluster id '` … `' is not valid.` and the only variable part is the ClusterId copied verbatim from the request; the policy sentence's fixed text is `Auto Scaling policy was not found for instance group '` … `'.` with the request's InstanceGroupId as its only variable. In other words AWS validates the CLUSTER scope before the group, and names the member that actually failed to resolve.

Emulator: the status (400) and the code (`ValidationException`) are right, and the no-policy sentence is right, but for an unknown ClusterId the emulator serves the GROUP sentence: `Instance group id '<InstanceGroupId>' is not valid.` The replayed check passed the attach, the detach and the `Auto Scaling policy was not found for instance group '<id>'.` assertion, then failed on the unknown-cluster assertion with: `AssertionError: expected a cluster-not-found message distinct from the no-policy case, got "Instance group id 'ig-0000000000002' is not valid."` (the `ig-…` value is this run's group id filling the template, not part of the requirement). The handler deliberately collapses both cases: the `store.cluster_mut(...)` miss returns `crate::state::unknown_instance_group_message(&input.instance_group_id)` — the same message the group miss returns.

Why:      a client that passes a stale, mistyped or cross-account cluster id is told the INSTANCE GROUP does not exist. Operator tooling and tests that branch on the refusal text (retry/refresh the cluster id vs. re-enumerate instance groups, or assert the cluster-scoped sentence) take the wrong branch or fail outright, and the emulator reports a group id as invalid that is in fact perfectly valid inside its real cluster. It also makes the two negative classes AWS distinguishes impossible to tell apart.

Fix:      the RemoveAutoScalingPolicy handler's target resolution — resolve the cluster scope FIRST and, on a miss, refuse with EMR's cluster-not-valid sentence (the same template the other cluster-scoped EMR operations use) rather than the instance-group template; keep the group-not-found and no-policy sentences for the cases that genuinely name a group. Note: open issues 20261108T090729Z-elasticmapreduce-emr-auto-termination-policy-operations-refuse-unknown-clusterid.md and 20261118T033253Z-elasticmapreduce-emr-authors-its-own-response-text-and-shape.md describe neighbouring EMR refusal-text divergences but neither covers this operation or this precedence, so they do not cover this check.

Traces:   emr.remove-autoscaling-policy-without-existing-policy-rejected; handler citation services/service-com-amazonaws-emr-elasticmapreduce/src/ops/remove_auto_scaling_policy.rs:173-177.

## Evidence
Expected (AWS, recorded): RemoveAutoScalingPolicy(ClusterId=<a cluster id the account holds no cluster for>, InstanceGroupId=<a real group id>) -> HTTP 400, Error.Code `ValidationException`, Error.Message `Cluster id '<ClusterId>' is not valid.` — distinct from the group-scoped refusal `Auto Scaling policy was not found for instance group '<InstanceGroupId>'.`

Actual (emulator): HTTP 400, Error.Code `ValidationException`, Error.Message `Instance group id '<InstanceGroupId>' is not valid.` — the group template, so the cluster-not-found class is not distinguishable from the group-not-found class.

Replay output:
  [mismatch] emr.remove-autoscaling-policy-without-existing-policy-rejected
  File "check_emr_remove_autoscaling_policy_without_existing_policy_rejected.py", line 142, in run
    assert message2 == f"Cluster id '{bogus_cluster_id}' is not valid."
  AssertionError: expected a cluster-not-found message distinct from the no-policy case, got "Instance group id 'ig-0000000000002' is not valid."

Source of the collapse (services/service-com-amazonaws-emr-elasticmapreduce/src/ops/remove_auto_scaling_policy.rs:173-177):
  let Some(cluster) = store.cluster_mut(&input.cluster_id, now) else {
      return Err(crate::scaffolded::Scaffolded::validation(
          crate::state::unknown_instance_group_message(&input.instance_group_id),
      ));
  };

Model check (smithy_query operation=RemoveAutoScalingPolicy): errors: [] — no modeled client error, so the ValidationException wording is recorded behavior only.

## Steps to Reproduce
1. Create an EMR autoscaling role and launch a cluster with AutoScalingRole set; wait for it to reach RUNNING.
2. ListInstanceGroups and take the CORE group's Id.
3. PutAutoScalingPolicy(ClusterId=<cluster>, InstanceGroupId=<core group>, AutoScalingPolicy=<any valid simple-scaling policy>); confirm ListInstanceGroups now reports AutoScalingPolicy on the group.
4. RemoveAutoScalingPolicy(ClusterId=<cluster>, InstanceGroupId=<core group>) and poll until the policy is gone.
5. RemoveAutoScalingPolicy again with the SAME ids -> 400 ValidationException "Auto Scaling policy was not found for instance group '<core group>'." (this part matches AWS).
6. RemoveAutoScalingPolicy(ClusterId="j-" + 13 random uppercase hex chars, InstanceGroupId=<core group>) -> emulator answers 400 ValidationException "Instance group id '<core group>' is not valid."; AWS answers 400 ValidationException "Cluster id '<that bogus cluster id>' is not valid."
