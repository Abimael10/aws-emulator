Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudhsmv2.deploy-cloudformation-cloudhsm-cluster-round-trip-and-delete
Issue-Key: cloudformation__cloudhsmv2.deploy-cloudformation-cloudhsm-cluster-round-trip-and-delete|run#23|AssertionError|assert [c["State"] for c in after] == ["DELETED"], f"after the stack delete: {after!r}"
Issue-Commit: 76731ae5a5033c825a554f156003e9b32c35e657

# Deleted CloudHSM cluster disappears from DescribeClusters instead of staying describable as the terminal DELETED tombstone

> _Note: this issue was identified against the code base at commit `76731ae5a5033c825a554f156003e9b32c35e657`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeClusters

## Description
One cause (Cause 1 of 1), grounded in the behavioral check cloudformation__cloudhsmv2.deploy-cloudformation-cloudhsm-cluster-round-trip-and-delete. It is the cloudhsmv2 side of that cross-service check (the check is listed under the suite heading `cloudformation__cloudhsmv2`, which is the service token this issue carries); the CloudFormation half of the check — CREATE_COMPLETE, Ref/GetAtt, tags, and DELETE_COMPLETE of the stack — all passed.

Cause 1 of 1: a cluster that has finished deleting is dropped from the store, so DescribeClusters stops reporting it at all instead of reporting it in the terminal DELETED state.

AWS:      `ClusterState` models DELETED as a state a cluster REPORTS (Smithy: enum ClusterState values CREATE_IN_PROGRESS, UNINITIALIZED, INITIALIZE_IN_PROGRESS, INITIALIZED, ACTIVE, UPDATE_IN_PROGRESS, MODIFY_IN_PROGRESS, ROLLBACK_IN_PROGRESS, DELETE_IN_PROGRESS, DELETED, DEGRADED), and DescribeClusters' documented `states` filter selects on exactly those values — a state no record can ever carry would not be filterable. Recorded behavior: after DeleteCluster has run to completion (here driven by the deletion of the AWS::CloudHSM::Cluster resource in a CloudFormation stack, after the stack reaches DELETE_COMPLETE), DescribeClusters(Filters={"clusterIds": [<clusterId>]}) answers HTTP 200 with a Clusters list of exactly ONE element whose State is "DELETED" — a tombstone, not a NotFound and not an empty list. The tombstone outlives the delete by far longer than the delete itself takes; it is not a few-seconds window.

Emulator: the same call answers HTTP 200 with `Clusters: []`. The deleted cluster reaches DELETE_IN_PROGRESS and then DELETED correctly, but the record is then reclaimed from the store, after which neither a clusterIds filter nor a states filter can see it. Source of the reclamation (citations under Traces): DeleteCluster sets `reclaim_at = now + DELETED_CLUSTER_RETENTION`, `DELETED_CLUSTER_RETENTION` is 2 seconds, and `settle()` retains only clusters with `now < reclaim_at`, so every DescribeClusters more than 2 s after the delete reads an empty list. The stack-delete poll loop alone takes longer than that, so the tombstone is unobservable in practice.

Why:      a client that polls a deleted cluster for its terminal state never observes DELETED — the cluster vanishes mid-poll, which is indistinguishable from an id that never existed. Code that tells "deleted" apart from "never created" (deletion verification, reconciliation of CloudFormation-managed clusters, a `states=[DELETED]` audit listing) gets the wrong answer, and an `OperationAborted`/not-found branch is taken where AWS returns a record.

Fix:      the cloudhsmv2 service state's retention of deleted clusters (the DELETED_CLUSTER_RETENTION window and the reclaim predicate in `settle`), plus the `reclaim_at` DeleteCluster stamps on the record. The DELETED record must stay readable by DescribeClusters for the life of the emulator process (or a window far longer than any single check's delete-and-read), while remaining excluded from the live-cluster checks that `is_gone()` guards.

Traces:   behavioral check cloudformation__cloudhsmv2.deploy-cloudformation-cloudhsm-cluster-round-trip-and-delete, final assertion `[c["State"] for c in after] == ["DELETED"]` -> AssertionError: after the stack delete: []. Code citations: services/service-com-amazonaws-cloudhsmv2-baldrapiservice/src/state.rs:96 (DELETED_CLUSTER_RETENTION = 2 s), :342-343 (settle retains only `reclaim_at.is_none_or(|at| now < at)`), :289-291 (is_gone), and services/service-com-amazonaws-cloudhsmv2-baldrapiservice/src/ops/delete_cluster.rs:203-213 (sets DELETE_IN_PROGRESS, owes DELETED, stamps reclaim_at).

## Evidence
Expected (AWS): after the cluster's deletion completes, DescribeClusters(Filters={"clusterIds": [<clusterId>]}) -> HTTP 200, Clusters = [ { ClusterId: <clusterId>, State: "DELETED", ... } ] (one element, terminal tombstone). No error code, no empty list.

Actual (emulator): HTTP 200 with Clusters = []. Replay output:
  File "check_cloudformation__cloudhsmv2_deploy_cloudformation_cloudhsm_cluster_round_trip_and_delete.py", line 210, in run
    assert [c["State"] for c in after] == ["DELETED"], f"after the stack delete: {after!r}"
  AssertionError: after the stack delete: []

The assertion text "after the stack delete:" is the check's fixed text; the empty list is the emulator's whole DescribeClusters answer. The cluster id in a reproduction is minted per run and is not part of the requirement; what is required is that the record for the id just deleted is still returned with State "DELETED".

Mechanism, from the checkout: DELETED_CLUSTER_RETENTION is 2 seconds and settle() drops any cluster whose reclaim_at has passed, so the record is gone before any caller that polled the delete to completion can read it.

## Steps to Reproduce
1. Boot the emulator and point a botocore client at cloudformation and cloudhsmv2 in the same region.
2. CreateStack with a template containing an AWS::EC2::VPC, an AWS::EC2::Subnet in it, and an AWS::CloudHSM::Cluster with HsmType hsm2m.medium, Mode NON_FIPS, SubnetIds [subnet]; wait for CREATE_COMPLETE.
3. DescribeClusters(Filters={"vpcIds": [<stack VpcId>]}) -> one cluster in State UNINITIALIZED; note its ClusterId.
4. DeleteStack and poll DescribeStacks until DELETE_COMPLETE (equivalently: call cloudhsmv2 DeleteCluster directly and sleep >2 s).
5. DescribeClusters(Filters={"clusterIds": [<ClusterId from step 3>]}).
   Expected: Clusters has one element with State "DELETED".
   Actual: Clusters is [].
