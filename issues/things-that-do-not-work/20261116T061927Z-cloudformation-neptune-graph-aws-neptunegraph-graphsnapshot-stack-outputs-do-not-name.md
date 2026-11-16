Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__neptune-graph.deploy-neptune-graph-snapshot-round-trip-and-delete
Issue-Key: cloudformation__neptune-graph.deploy-neptune-graph-snapshot-round-trip-and-delete|run#22|AssertionError|assert snaps == [(snapshot_id, snapshot_arn)], (
Issue-Commit: 36712e0283f049b61debb52d750d108c70e96514

# AWS::NeptuneGraph::GraphSnapshot stack outputs do not name the snapshot neptune-graph created: Ref/Fn::GetAtt Id disagree with the (id, arn) pair ListGraphSnapshots reports

> _Note: this issue was identified against the code base at commit `36712e0283f049b61debb52d750d108c70e96514`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new root cause.

Cause 1 of 1: the identity CloudFormation publishes for an AWS::NeptuneGraph::GraphSnapshot resource does not match the snapshot the neptune-graph state actually holds. Failing check: cloudformation__neptune-graph.deploy-neptune-graph-snapshot-round-trip-and-delete.

AWS: Smithy (neptune-graph) GetGraphSnapshot returns a required `id` matching ^gs-[a-z0-9]{10}$, a required `arn` (the snapshot ARN) and a required `name` matching ^(?!gs-)[a-z][a-z0-9]*(-[a-z0-9]+)*$; ListGraphSnapshots(graphIdentifier=g-...) lists exactly those summaries for one graph. For the AWS::NeptuneGraph::GraphSnapshot resource, Ref resolves to the snapshot's ARN and Fn::GetAtt "Id" to the snapshot's gs- id — recorded behavior: for a CREATE_COMPLETE stack, the pair (Fn::GetAtt Snapshot.Id, Ref Snapshot) published in the stack's Outputs is identical to the (id, arn) pair ListGraphSnapshots reports for the entry whose `name` equals the templated SnapshotName, and GetGraphSnapshot(snapshotIdentifier=<that id>) reads back that arn, that name, the source graph id and status AVAILABLE.

Emulator: the two disagree. The stack reaches CREATE_COMPLETE and the graph half is consistent (ListGraphs by name resolves to Ref Graph, and GetGraph's arn/endpoint/status/provisionedMemory/replicaCount/deletionProtection/publicConnectivity match the outputs), but the snapshot listed under that graph for the templated SnapshotName carries an (id, arn) pair that is not the (GetAtt Id, Ref) pair the stack published, so the equality fails. Hypothesis: the resource handler derives the published identity from the template (the SnapshotName) or publishes the same value for both Ref and Id, instead of echoing the gs- id and the ARN the neptune-graph state minted for the snapshot.

Why: a client relying on AWS's behavior gets an identifier neptune-graph does not know. Every consumer of that output — another resource referencing the snapshot, a cross-stack export, or a script taking the stack output and calling GetGraphSnapshot, ListTagsForResource on the ARN, or a restore from the snapshot — is handed a value the service refuses, so a CREATE_COMPLETE stack yields unusable references while the snapshot itself exists under a different identity. It also breaks the stack's own teardown contract, where the post-delete ResourceNotFoundException is expected to name that id (`No snapshot found for customer [<account>] with snapshot id [<id>]`, fixed text with the account and the snapshot id supplied from the request/state).

Fix: the CloudFormation resource handler for AWS::NeptuneGraph::GraphSnapshot — its physical-resource-id/Ref value and its read-only attribute projection (Id, and the ARN form) must be taken from the snapshot record the neptune-graph state created, not synthesized from template properties.

Traces: cloudformation__neptune-graph.deploy-neptune-graph-snapshot-round-trip-and-delete. The open issue 20261109T115129Z-cloudformation-ec2-neptune-graph-cloudformation-aws-neptunegraph-graph-and-aws-neptunegraph-privategraphendpoint.md (AWS::NeptuneGraph::Graph and PrivateGraphEndpoint rejected as unrecognized types) does not cover this: in this replay the Graph provisioned and read back correctly and the divergence is the GraphSnapshot's published identity.

## Evidence
Replay of cloudformation__neptune-graph.deploy-neptune-graph-snapshot-round-trip-and-delete:

  File "check_cloudformation__neptune_graph_deploy_neptune_graph_snapshot_round_trip_and_delete.py", line 232, in run
    assert snaps == [(snapshot_id, snapshot_arn)], (
  AssertionError: ListGraphSnapshots for 'sim2real-ngsnap-<suffix>': [('gs-54t2qk16z6', 'arn:aws:neptune-graph:us-west-2:<account>:graph-snapshot/gs-54t2qk16z6')]

Template (abridged): Graph = AWS::NeptuneGraph::Graph {GraphName, ProvisionedMemory: 16, ReplicaCount: 0, DeletionProtection: false, PublicConnectivity: false, Tags}; Snapshot = AWS::NeptuneGraph::GraphSnapshot {GraphIdentifier: {Ref: Graph}, SnapshotName, Tags}. Outputs: SnapshotArn = {Ref: Snapshot}, SnapshotId = {Fn::GetAtt: [Snapshot, Id]}.

Expected (AWS): [(s.id, s.arn) for s in ListGraphSnapshots(graphIdentifier=<Ref Graph>) if s.name == <SnapshotName>] == [(<GetAtt Snapshot.Id>, <Ref Snapshot>)] — one entry, whose id matches ^gs-[a-z0-9]{10}$ and whose arn is the snapshot ARN, equal to the stack's published pair.
Actual (emulator): the listing holds exactly one entry for that name, with a well-formed gs- id and matching graph-snapshot ARN, but it is not the pair the stack published, so the sets differ. (The specific id/ARN/name in the quoted line are one run's generated values, not requirements; the requirement is the equality and the id/arn forms.)

Model: neptune-graph GetGraphSnapshot output id pattern ^gs-[a-z0-9]{10}$, arn required, name pattern ^(?!gs-)[a-z][a-z0-9]*(-[a-z0-9]+)*$; ListGraphSnapshots paginated on graphSnapshots with graphIdentifier pattern ^g-[a-z0-9]{10}$.

## Steps to Reproduce
1. CreateStack with an AWS::NeptuneGraph::Graph and an AWS::NeptuneGraph::GraphSnapshot whose GraphIdentifier is {Ref: Graph} and whose SnapshotName is a fresh name; publish Outputs SnapshotId = Fn::GetAtt [Snapshot, Id] and SnapshotArn = Ref Snapshot. Wait for CREATE_COMPLETE.
2. Read the stack outputs, then call neptune-graph ListGraphSnapshots(graphIdentifier=<the graph id>) and keep the entry whose name equals the templated SnapshotName.
3. AWS: that entry's (id, arn) equals (SnapshotId, SnapshotArn) from the outputs, and GetGraphSnapshot(snapshotIdentifier=SnapshotId) returns that arn, that name, the source graph id and status AVAILABLE. Emulator: the listed (id, arn) differs from the stack's published pair.
