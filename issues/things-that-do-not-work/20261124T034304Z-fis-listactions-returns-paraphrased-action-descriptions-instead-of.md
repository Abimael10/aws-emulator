Issue-Source: trace-sweep-divergence
Issue-Trace: fis-catalog-list-actions
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: fis-catalog-list-actions
Issue-Shape-Id: com.amazonaws.fis#FaultInjectionSimulator
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
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:kinesis:stream-provisioned-throughput-exception"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:lambda:invocation-add-delay"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:lambda:invocation-error"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:lambda:invocation-http-integration-response"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:memorydb:multi-region-cluster-pause-replication"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:network:disrupt-connectivity"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:network:disrupt-vpc-endpoint"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:network:route-table-disrupt-cross-region-connectivity"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:network:transit-gateway-disrupt-cross-region-connectivity"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:rds:failover-db-cluster"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:rds:reboot-db-instances"].description
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:s3:bucket-pause-replication"].description
Issue-Commit: 34b956776d59299fbb546444fafe35aa6ddbd93a

# fis: ListActions returns paraphrased action descriptions instead of the AWS action-catalog description strings

> _Note: this issue was identified against the code base at commit `34b956776d59299fbb546444fafe35aa6ddbd93a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListActions

## Description
One net-new cause.

Cause 1 of 1: GET /actions (ListActions) answers 200 with the right set of pre-defined FIS actions and the right ARNs, but the `description` member of essentially every ActionSummary carries emulator-authored prose rather than the description AWS publishes for that action.

AWS: Smithy: ListActions -> ListActionsResponse.actions : ActionSummaryList of ActionSummary; ActionSummary.description targets ActionDescription (@length min 0 max 512, pattern ^[\s\S]+$), documented as "The description for the action." The FIS action catalog is service-owned, read-only data: every AWS account sees the same `id`, `arn`, `description` and `targets` for a given pre-defined action (aws:ecs:task-kill-process, aws:eks:pod-delete, aws:fis:wait, ...). The model constrains the member's shape but not its text, so the required text is recorded behavior only — the fixture captures the real service's catalog strings, e.g. aws:ecs:task-kill-process => "It kills a particular process by name, using the killall command."; aws:ecs:task-network-blackhole-port => "It drops incoming or outgoing traffic for a configurable protocol (tcp or udp) and port which is useful for simulating dependency failures."; aws:ecs:task-network-latency => "It adds latency, with jitter, to outgoing or incoming traffic from a configurable list of sources (Supported: IPv4, IPv4/CIDR, domain name, DYNAMODB|S3)."; aws:eks:pod-cpu-stress => "Runs CPU stress on the target pods."; aws:eks:inject-kubernetes-custom-resource => "Injects the specified kubernetes custom resource in the target EKS cluster.". These strings are fixed text keyed by action id — nothing in them is derived from the request, the account or the clock, so they are a stable requirement and not a per-run value.

Emulator: the response is 200 and the ARNs match, but each description is a different sentence of the emulator's own writing: aws:ecs:task-kill-process => "Kills a process on a set of Amazon ECS tasks."; aws:ecs:task-network-blackhole-port => "Drops traffic on a port of a set of Amazon ECS tasks."; aws:ecs:task-network-latency => "Adds latency and jitter to the network traffic of a set of Amazon ECS tasks."; aws:ecs:task-network-packet-loss => "Drops a percentage of the network packets of a set of Amazon ECS tasks."; aws:eks:inject-kubernetes-custom-resource => "Applies a Kubernetes custom resource to an Amazon EKS cluster."; aws:eks:pod-cpu-stress => "Runs CPU stress on a set of Kubernetes pods.". The replay reports value_mismatch on `$.actions[arn=...].description` for ~31 distinct actions in one response, spanning the aws:ecs, aws:eks, aws:elasticache, aws:fis, aws:kinesis, aws:lambda, aws:memorydb, aws:network, aws:rds and aws:s3 families — i.e. the whole seeded catalog, not a few stale entries.

Why: the FIS action catalog is the service's own documentation surface. Consoles, CLI `aws fis list-actions` output, experiment-authoring UIs and docs generators render `description` verbatim, and tests/goldens that pin the catalog (or that diff ListActions against GetAction) compare it as fixed service data. A client built against AWS sees different text for every action, so any snapshot assertion, catalog cache or UI golden fails against the emulator even though the action set itself is correct, and a user reading the emulator's catalog is told something subtly different about what an action does (e.g. the emulator's latency text omits the configurable source list AWS documents).

Fix: the fis service's pre-defined action catalog seed data — the table of action id/arn/description/targets that the ListActions handler (and GetAction, which must agree with it) serializes. The descriptions must be the catalog's published strings keyed by action id, not hand-written summaries; GetAction for the same id must return the identical string.

Traces: fis-catalog-list-actions#task#1 (ListActions, expected 200, actual 200, ~31 `$.actions[arn=...].description` value_mismatch entries).

Not covered by the open fis issues: 20261117T121318Z-... (stop-condition source refusal wording, UpdateExperimentTemplate validation) and 20261117T164338Z-... (CreateExperimentTemplate parallel-action quota, UpdateSafetyLever) are both experiment-template write-path findings and carry no replay key for this read-only catalog step; 20261109T155854Z-... is a CloudFormation AWS::FIS::ExperimentTemplate failure-reason issue.

## Evidence
Fixture fis-catalog-list-actions, step task#1, operation ListActions.
Expected status 200; actual status 200 — the divergence is in the body.
verdict: mismatch, affected_count 1 (single-step cluster).

Per-member value_mismatch (expected = recorded AWS, got = emulator):
- $.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:ecs:task-kill-process"].description
  expected 'It kills a particular process by name, using the killall command.'
  got      'Kills a process on a set of Amazon ECS tasks.'
- ...aws:ecs:task-network-blackhole-port
  expected 'It drops incoming or outgoing traffic for a configurable protocol (tcp or udp) and port which is useful for simulating dependency failures.'
  got      'Drops traffic on a port of a set of Amazon ECS tasks.'
- ...aws:ecs:task-network-latency
  expected 'It adds latency, with jitter, to outgoing or incoming traffic from a configurable list of sources (Supported: IPv4, IPv4/CIDR, domain name, DYNAMODB|S3).'
  got      'Adds latency and jitter to the network traffic of a set of Amazon ECS tasks.'
- ...aws:ecs:task-network-packet-loss
  expected 'It adds packet loss to outgoing or incoming traffic from a configurable list of sources (Supported: IPv4, IPv4/CIDR, domain name, DYNAMODB|S3).'
  got      'Drops a percentage of the network packets of a set of Amazon ECS tasks.'
- ...aws:eks:inject-kubernetes-custom-resource
  expected 'Injects the specified kubernetes custom resource in the target EKS cluster.'
  got      'Applies a Kubernetes custom resource to an Amazon EKS cluster.'
- ...aws:eks:pod-cpu-stress
  expected 'Runs CPU stress on the target pods.'
  got      'Runs CPU stress on a set of Kubernetes pods.'
- ...aws:eks:pod-delete
  expected 'Deletes pods of a given Kubernetes namespace using pod identifying information ...'
  got      (emulator paraphrase)

The cluster's divergence key lists the same value_mismatch on .description for ~31 action ARNs in all: aws:ecs:task-kill-process, aws:ecs:task-network-blackhole-port, aws:ecs:task-network-latency, aws:ecs:task-network-packet-loss, aws:eks:inject-kubernetes-custom-resource, aws:eks:pod-cpu-stress, aws:eks:pod-delete, aws:eks:pod-io-stress, aws:eks:pod-memory-stress, aws:eks:pod-network-blackhole-port, aws:eks:pod-network-latency, aws:eks:pod-network-packet-loss, aws:eks:terminate-nodegroup-instances, aws:elasticache:replicationgroup-interrupt-az-power, aws:fis:inject-api-internal-error, aws:fis:inject-api-throttle-error, aws:fis:inject-api-unavailable-error, aws:fis:wait, aws:kinesis:stream-expired-iterator-exception, aws:kinesis:stream-provisioned-throughput-exception, aws:lambda:invocation-add-delay, aws:lambda:invocation-error, aws:lambda:invocation-http-integration-response, aws:memorydb:multi-region-cluster-pause-replication, aws:network:disrupt-connectivity, aws:network:disrupt-vpc-endpoint, aws:network:route-table-disrupt-cross-region-connectivity, aws:network:transit-gateway-disrupt-cross-region-connectivity, aws:rds:failover-db-cluster, aws:rds:reboot-db-instances, aws:s3:bucket-pause-replication.

No other member diverges: ids, ARNs and targets match, so only the description text is wrong.

Model evidence (smithy_query on services/service-com-amazonaws-fis-faultinjectionsimulator/smithy-model/com-amazonaws-fis-faultinjectionsimulator.json):
- ListActions: GET /actions, code 200, output ListActionsResponse{actions: ActionSummaryList, nextToken}, error ValidationException(400), paginated(items=actions).
- ActionSummary.description -> ActionDescription (string, @length 0..512, pattern ^[\s\S]+$), doc "The description for the action."
- GetAction: GET /actions/{id} returns Action for the same catalog entry and must agree.

## Steps to Reproduce
1. Boot the emulator from this checkout (fis is bootstrapped: services/service-com-amazonaws-fis-faultinjectionsimulator).
2. Replay fixture fis-catalog-list-actions, step task#1 — or by hand: GET /actions on the fis endpoint (equivalently `aws fis list-actions`), with no query parameters.
3. Read the 200 response body and compare each entry's `description` against the recorded AWS catalog text for the same `id`/`arn`.
4. Observed: for ~31 of the pre-defined actions the description is an emulator-authored sentence, e.g. action aws:ecs:task-kill-process returns 'Kills a process on a set of Amazon ECS tasks.' where AWS returns 'It kills a particular process by name, using the killall command.'; aws:eks:pod-cpu-stress returns 'Runs CPU stress on a set of Kubernetes pods.' where AWS returns 'Runs CPU stress on the target pods.'
5. Expected: each ActionSummary.description is the catalog string AWS publishes for that action id, byte-for-byte; `aws fis get-action --id <id>` returns the same string for the same action.
