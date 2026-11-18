Issue-Source: trace-sweep-divergence
Issue-Trace: fis-catalog-list-actions
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: fis-catalog-list-actions
Issue-Shape-Id: com.amazonaws.fis#FaultInjectionSimulator
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ec2:api-error"]
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ec2:api-insufficient-instance-capacity-error"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ec2:asg-insufficient-instance-capacity-error"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ec2:send-spot-instance-interruptions"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ecs:drain-container-instances"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ecs:stop-task"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ecs:task-cpu-stress"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ecs:task-io-stress"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ecs:task-kill-process"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ecs:task-network-blackhole-port"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ecs:task-network-latency"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ecs:task-network-packet-loss"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:eks:inject-kubernetes-custom-resource"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:eks:pod-cpu-stress"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:eks:pod-delete"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:eks:pod-io-stress"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:eks:pod-memory-stress"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:eks:pod-network-blackhole-port"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:eks:pod-network-latency"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:eks:pod-network-packet-loss"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:eks:terminate-nodegroup-instances"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:elasticache:replicationgroup-interrupt-az-power"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:fis:inject-api-internal-error"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:fis:inject-api-throttle-error"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:fis:inject-api-unavailable-error"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:fis:wait"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:kinesis:stream-expired-iterator-exception"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:kinesis:stream-expired-iterator-exception"].targets.KinesisStreams
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:kinesis:stream-provisioned-throughput-exception"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:kinesis:stream-provisioned-throughput-exception"].targets.KinesisStreams
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:lambda:invocation-add-delay"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:lambda:invocation-error"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:lambda:invocation-http-integration-response"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:memorydb:multi-region-cluster-pause-replication"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:memorydb:multi-region-cluster-pause-replication"].targets.MultiRegionClusters
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:network:disrupt-connectivity"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:network:disrupt-vpc-endpoint"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:network:disrupt-vpc-endpoint"].targets.VPCEndpoints
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:network:route-table-disrupt-connectivity"]
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:network:route-table-disrupt-cross-region-connectivity"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:network:route-table-disrupt-cross-region-connectivity"].targets.Subnets
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:network:transit-gateway-disrupt-cross-region-connectivity"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:rds:failover-db-cluster"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:rds:reboot-db-instances"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:s3:bucket-pause-replication"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ssm:send-command"].description
Issue-Commit: 9951930cd14f9d759a628e03b294f2c016f90e5f

# FIS action catalog rows diverge from the published catalog: ListActions reports paraphrased descriptions and renamed target keys

> _Note: this issue was identified against the code base at commit `9951930cd14f9d759a628e03b294f2c016f90e5f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListActions

## Description
ListActions answers 200 with a page of the service-owned action catalog, but the rows of that catalog are this emulator's own restatement of the published FIS action catalog rather than the catalog itself: ~45 of the reported actions carry a description AWS does not publish for that id, five carry a targets map keyed by a name AWS does not use, and two action entries diverge wholesale. The catalog is a constant table (crate::state::catalog::ACTION_CATALOG) that both ListActions and GetAction report from, so every row that is wrong is wrong in both operations and for every caller, not just in this fixture. 3 causes — numbered 1, 2 and 3 below; close all three before submitting.

Cause 1 of 3: the `description` of most catalog rows is a paraphrase rather than the string FIS publishes for that action id.
AWS: Smithy model services/service-com-amazonaws-fis-faultinjectionsimulator/smithy-model/com-amazonaws-fis-faultinjectionsimulator.json — ListActions GET /actions -> 200 (error ValidationException 400), output ActionSummaryList of ActionSummary{id, arn, description: ActionDescription, targets: ActionTargetMap, tags}; GetAction GET /actions/{id} -> 200 reports the same description on Action. The model documents `description` only as the action's description and points at the FIS actions reference for the content, so the TEXT is recorded behavior only: the recording is the authority. The recorded response publishes, per id, a fixed string that is the action's own published summary — e.g. for aws:ec2:api-insufficient-instance-capacity-error 'Cause the EC2 service to return insufficient capacity error responses for specific callers.', for aws:ec2:asg-insufficient-instance-capacity-error 'Cause the targeted AutoScaling Groups to receive insufficient instance capacity errors when attempting to provision new instances.', for aws:ec2:send-spot-instance-interruptions 'Interrupt the specified EC2 Spot instances.', for aws:ecs:drain-container-instances 'Drain percentage of underlying EC2 instances on an ECS cluster.', for aws:ecs:stop-task 'Stop the specified ECS tasks of an ECS cluster.', for aws:ecs:task-cpu-stress 'It runs CPU stress via stress-ng tool.', for aws:ecs:task-io-stress 'It runs IO stress via stress-ng tool.' Each of these is wholly fixed text for its id — no part of it comes from the request or from state — so the whole string, wording and final punctuation included, is the requirement, and it is a per-id requirement: the published strings follow no single pattern, so a row's string cannot be derived from a sibling's.
Emulator: it reports a differently worded sentence of its own for the same ids — 'Injects insufficient instance capacity errors into the EC2 RunInstances requests made with a set of IAM roles.', 'Injects insufficient instance capacity errors into the instance launches of a set of Auto Scaling groups.', 'Interrupts a set of Spot Instances after sending an interruption notice.', 'Drains a percentage of the container instances of a set of Amazon ECS clusters.', 'Stops a set of Amazon ECS tasks.', 'Runs CPU stress on a set of Amazon ECS tasks.', 'Runs I/O stress on a set of Amazon ECS tasks.' respectively. The replay reports verdict mismatch, expected_status 200, actual_status 200, with a value_mismatch at $.actions[arn=...].description for the rows listed under Evidence: the aws:ec2, aws:ecs, aws:eks, aws:elasticache, aws:fis, aws:kinesis, aws:lambda, aws:memorydb, aws:network, aws:rds, aws:s3 and aws:ssm families — about 45 rows in all, i.e. most of the table. Hypothesis: the rows were written from the emulator's own reading of what each action does (the table's own doc comment claims to publish the reference's strings character for character) instead of being transcribed from it.
Why: the catalog is the only place a client learns what an action does. A console, a template builder or a doc generator that renders Action$description shows text the real service never returns, and any snapshot, golden-file or contains-assertion written against AWS's published catalog fails against the emulator. Because the same table backs GetAction, the point read of an action is wrong in exactly the same way.
Fix: the catalog table itself — the `description` field of the ActionCatalogEntry rows in crate::state::catalog (ACTION_CATALOG), which must carry the string published for each id rather than a restatement of it; ListActions' handler and ActionCatalogEntry::to_summary/to_action only copy what the row holds and need no change.
Traces: fis-catalog-list-actions#task#1.

Cause 2 of 3: five rows report their `targets` map under a key AWS does not use, so the target key an experiment template must refer to is absent from the catalog.
AWS: same model — ActionSummary$targets / Action$targets is an ActionTargetMap keyed by the target NAME, and that key is the same key an experiment template's action uses in ExperimentTemplateAction$targets to bind the action to a declared target; ActionTarget$resourceType names the TargetResourceTypeId. The key is recorded behavior only in its spelling, and the recording fixes it per action: the recorded response keys aws:kinesis:stream-expired-iterator-exception and aws:kinesis:stream-provisioned-throughput-exception under 'KinesisStreams', aws:memorydb:multi-region-cluster-pause-replication under 'MultiRegionClusters', aws:network:disrupt-vpc-endpoint under 'VPCEndpoints', and aws:network:route-table-disrupt-cross-region-connectivity under 'Subnets'.
Emulator: those rows name the target differently — 'Streams' for both kinesis rows, 'Clusters' for the memorydb row, 'VpcEndpoints' for the vpc-endpoint row and 'RouteTables' for the cross-region route-table row (crate::state::catalog, the ActionCatalogTarget `name` of each of those entries). The replay therefore reports a divergence at the key the recording expects and the emulator does not emit: $.actions[arn=...aws:kinesis:stream-expired-iterator-exception].targets.KinesisStreams, ...stream-provisioned-throughput-exception].targets.KinesisStreams, ...aws:memorydb:multi-region-cluster-pause-replication].targets.MultiRegionClusters, ...aws:network:disrupt-vpc-endpoint].targets.VPCEndpoints, ...aws:network:route-table-disrupt-cross-region-connectivity].targets.Subnets — all under verdict mismatch, expected_status 200, actual_status 200.
Why: this one is not cosmetic. A client that reads an action's target contract from the catalog and then builds an experiment template binds its target under the key the catalog gave it; against the emulator it learns 'Streams'/'Clusters'/'VpcEndpoints'/'RouteTables', which real FIS rejects, and conversely a template written against AWS's documented keys cannot be matched to the action the emulator published. Note also that the cross-region route-table action targets subnets in AWS's catalog, not route tables, so the resourceType bound to that key is wrong as well as its name.
Fix: the `targets` field (ActionCatalogTarget `name`, and the `resource_type` it is paired with for the cross-region route-table row) of those five ActionCatalogEntry rows in crate::state::catalog.
Traces: fis-catalog-list-actions#task#1.

Cause 3 of 3: two action entries diverge as whole objects rather than at one member.
AWS: same model and the same recorded ListActions page. For arn .../action/aws:ec2:api-error and arn .../action/aws:network:route-table-disrupt-connectivity the replay keys the divergence at the ACTION OBJECT ($.actions[arn=...]) and not at a member of it, which is how it reports an entry the recording and the emulator do not agree on as a whole — an entry present on one side and not the other, or one whose published shape differs from the row wholesale — unlike every other row in this cluster, which diverges only at .description or at .targets.<Key>. The recorded page is the authority for both entries; re-replay the step to read the two keys in full before editing, since the reported detail for this cluster is long enough to be clipped.
Emulator: crate::state::catalog holds a row for each of those two ids (aws:ec2:api-error described as 'Injects the specified error into the EC2 API requests made with a set of IAM roles.' with targets Roles -> aws:iam:role; aws:network:route-table-disrupt-connectivity described as 'Disrupts connectivity to a service for the subnets a set of route tables serve.' with targets RouteTables -> aws:ec2:route-table), yet the replay reports the entry itself as diverging: verdict mismatch, expected_status 200, actual_status 200.
Why: a whole-entry divergence means the catalog either omits an action FIS publishes — in which case GetAction on that id answers ResourceNotFoundException for an action that exists, and no client can build a template using it — or publishes one shaped unlike the real entry. Either way the published surface of those two actions cannot be relied on.
Fix: the ACTION_CATALOG rows for aws:ec2:api-error and aws:network:route-table-disrupt-connectivity in crate::state::catalog — the entry must match the published catalog entry for that id in every member ListActions reports (id, arn, description, targets, tags), and GetAction must report the same row.
Traces: fis-catalog-list-actions#task#1.

Relation to open issues: 20261109T122825Z-fis-experiment-records-diverge-run-timestamps-carry-host.md reports that FIS experiment-record timestamps and state reasons use the emulator's own values; it owns the CreateExperimentTemplate/StartExperiment/GetExperiment steps of fis-wait-experiment-runs-to-completed and touches no catalog row, so it does not cover this. The two resolved-targets issues (20261104T032609Z, 20261107T200341Z) and 20261109T155854Z-cloudformation-fis-... are unrelated to ListActions.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 48 fixtures, 282 steps, 278 passed, 4 mismatched, 4 divergence clusters, score 98.6%, emulator_up true, bootstrap_signal false. Three of the four clusters are owned by 20261109T122825Z-fis-experiment-records-diverge-run-timestamps-carry-host.md; the cluster below is the one net-new cluster and reports empty stub_path and empty owned_by.

Cluster: service=fis, operation ListActions, fixture fis-catalog-list-actions, step task#1, verdict mismatch, expected_status 200, actual_status 200, affected_count 1.
divergence_key: service=fis|ListActions|status=200->200|keys=...

Cause 1 — value_mismatch at $.actions[arn="arn:aws:fis:<region>:<account>:action/<id>"].description, expected (AWS) vs actual (emulator), for the rows the detail quoted in full:
  aws:ec2:api-insufficient-instance-capacity-error
    expected: 'Cause the EC2 service to return insufficient capacity error responses for specific callers.'
    actual:   'Injects insufficient instance capacity errors into the EC2 RunInstances requests made with a set of IAM roles.'
  aws:ec2:asg-insufficient-instance-capacity-error
    expected: 'Cause the targeted AutoScaling Groups to receive insufficient instance capacity errors when attempting to provision new instances.'
    actual:   'Injects insufficient instance capacity errors into the instance launches of a set of Auto Scaling groups.'
  aws:ec2:send-spot-instance-interruptions
    expected: 'Interrupt the specified EC2 Spot instances.'
    actual:   'Interrupts a set of Spot Instances after sending an interruption notice.'
  aws:ecs:drain-container-instances
    expected: 'Drain percentage of underlying EC2 instances on an ECS cluster.'
    actual:   'Drains a percentage of the container instances of a set of Amazon ECS clusters.'
  aws:ecs:stop-task
    expected: 'Stop the specified ECS tasks of an ECS cluster.'
    actual:   'Stops a set of Amazon ECS tasks.'
  aws:ecs:task-cpu-stress
    expected: 'It runs CPU stress via stress-ng tool.'
    actual:   'Runs CPU stress on a set of Amazon ECS tasks.'
  aws:ecs:task-io-stress
    expected: 'It runs IO stress via stress-ng tool.'
    actual:   'Runs I/O stress on a set of Amazon ECS tasks.'
The cluster's key list reports a .description divergence for these ids as well (expected text not quoted in the clipped detail; re-replay the step to read it): aws:ecs:task-kill-process, aws:ecs:task-network-blackhole-port, aws:ecs:task-network-latency, aws:ecs:task-network-packet-loss, aws:eks:inject-kubernetes-custom-resource, aws:eks:pod-cpu-stress, aws:eks:pod-delete, aws:eks:pod-io-stress, aws:eks:pod-memory-stress, aws:eks:pod-network-blackhole-port, aws:eks:pod-network-latency, aws:eks:pod-network-packet-loss, aws:eks:terminate-nodegroup-instances, aws:elasticache:replicationgroup-interrupt-az-power, aws:fis:inject-api-internal-error, aws:fis:inject-api-throttle-error, aws:fis:inject-api-unavailable-error, aws:fis:wait, aws:kinesis:stream-expired-iterator-exception, aws:kinesis:stream-provisioned-throughput-exception, aws:lambda:invocation-add-delay, aws:lambda:invocation-error, aws:lambda:invocation-http-integration-response, aws:memorydb:multi-region-cluster-pause-replication, aws:network:disrupt-connectivity, aws:network:disrupt-vpc-endpoint, aws:network:route-table-disrupt-cross-region-connectivity, aws:network:transit-gateway-disrupt-cross-region-connectivity, aws:rds:failover-db-cluster, aws:rds:reboot-db-instances, aws:s3:bucket-pause-replication, aws:ssm:send-command.
(The ARNs in the keys carry the replay's region and account; those are per-environment values and are not part of the requirement. The requirement is the per-id description string.)

Cause 2 — divergence at a targets key the recording carries and the emulator does not emit:
  $.actions[arn=...action/aws:kinesis:stream-expired-iterator-exception].targets.KinesisStreams      (emulator keys it 'Streams')
  $.actions[arn=...action/aws:kinesis:stream-provisioned-throughput-exception].targets.KinesisStreams (emulator keys it 'Streams')
  $.actions[arn=...action/aws:memorydb:multi-region-cluster-pause-replication].targets.MultiRegionClusters (emulator keys it 'Clusters')
  $.actions[arn=...action/aws:network:disrupt-vpc-endpoint].targets.VPCEndpoints                     (emulator keys it 'VpcEndpoints')
  $.actions[arn=...action/aws:network:route-table-disrupt-cross-region-connectivity].targets.Subnets  (emulator keys it 'RouteTables' -> aws:ec2:route-table)

Cause 3 — divergence keyed at the whole action object, not at a member:
  $.actions[arn="arn:aws:fis:<region>:<account>:action/aws:ec2:api-error"]
  $.actions[arn="arn:aws:fis:<region>:<account>:action/aws:network:route-table-disrupt-connectivity"]

Model consulted (smithy_query query=operation, model_path services/service-com-amazonaws-fis-faultinjectionsimulator/smithy-model/com-amazonaws-fis-faultinjectionsimulator.json):
  ListActions GET /actions -> 200; errors ValidationException(400); input maxResults(1..100, httpQuery), nextToken(len 1..1024, ^[\S]+$, httpQuery); output actions: ActionSummaryList, nextToken; paginated items=actions.
  GetAction GET /actions/{id} -> 200; errors ResourceNotFoundException(404), ValidationException(400); output action: Action.
  The model documents no description text and no target key names, so the recorded page is the authority for both.

Emulator code consulted: services/service-com-amazonaws-fis-faultinjectionsimulator/src/ops/list_actions.rs (handler pages crate::state::ACTION_CATALOG through crate::state::catalog_page with entry.to_summary(&region) and writes nothing), services/service-com-amazonaws-fis-faultinjectionsimulator/src/state/catalog.rs lines 223+ (pub const ACTION_CATALOG, whose rows hold the description/targets reported above; lines 115-135 ActionCatalogEntry::to_summary, lines 72-106 ::to_action, so GetAction reports the same rows).

## Steps to Reproduce
1. git clone the repo and boot the emulator built from this checkout.
2. GET /actions (ListActions) with no maxResults and read the raw JSON body.
3. Cause 1: compare $.actions[*].description against the published FIS action catalog. For aws:ec2:send-spot-instance-interruptions the body reports 'Interrupts a set of Spot Instances after sending an interruption notice.' where AWS publishes 'Interrupt the specified EC2 Spot instances.'; the same mismatch holds for ~45 rows, listed in the Evidence section.
4. Cause 2: read $.actions[?(@.id=='aws:kinesis:stream-expired-iterator-exception')].targets — the map is keyed 'Streams' where AWS keys it 'KinesisStreams'. Likewise 'Clusters' vs 'MultiRegionClusters' for aws:memorydb:multi-region-cluster-pause-replication, 'VpcEndpoints' vs 'VPCEndpoints' for aws:network:disrupt-vpc-endpoint, and 'RouteTables' -> aws:ec2:route-table vs 'Subnets' for aws:network:route-table-disrupt-cross-region-connectivity.
5. Cause 3: compare the whole entries for aws:ec2:api-error and aws:network:route-table-disrupt-connectivity against the recorded page; the replay keys a divergence at the action object itself for both. Also GET /actions/{id} (GetAction) for those two ids — it reports the same row, so whatever the list gets wrong the point read gets wrong too.
6. Re-replay fixture fis-catalog-list-actions step task#1 after the fix; it must pass, which requires all three causes closed (the step reports one cluster covering all of them).
