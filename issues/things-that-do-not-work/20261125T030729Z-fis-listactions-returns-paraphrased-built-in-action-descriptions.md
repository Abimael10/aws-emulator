Issue-Source: trace-sweep-divergence
Issue-Trace: fis-catalog-list-actions
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: fis-catalog-list-actions
Issue-Shape-Id: com.amazonaws.fis#FaultInjectionSimulator
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:eks:pod-delete"].description
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
Issue-Key: fis|ListActions|2xx->2xx|$.actions[arn="arn:aws:fis:us-west-2:000000000000:action/aws:s3:bucket-pause-replication"].description
Issue-Commit: a451b2494ad63ebcb0a59d0c4e30e275403e3b3d

# fis: ListActions returns paraphrased built-in action descriptions instead of the AWS action catalog text

> _Note: this issue was identified against the code base at commit `a451b2494ad63ebcb0a59d0c4e30e275403e3b3d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListActions

## Description
One root cause: the FIS emulator's built-in action catalog carries hand-written, paraphrased `description` strings for the AWS-managed actions, so every `aws:*` action it serves describes itself with different words than the real FIS action catalog does. 23 of the catalog's actions diverge on `description` in a single ListActions response.

Cause 1 of 1: the seeded action-catalog rows' `description` values are paraphrases rather than the AWS catalog text.
  AWS: Smithy (services/service-com-amazonaws-fis-faultinjectionsimulator/smithy-model/com-amazonaws-fis-faultinjectionsimulator.json): ListActions is GET /actions, code 200, output `actions: ActionSummaryList`; ActionSummary.description targets ActionDescription (string, @length min 0 max 512, @pattern ^[\s\S]+$) and is documented as "The description for the action". GetAction (GET /actions/{id}) returns the same `description` on the Action shape. The model does not enumerate the catalog's contents, so for the text itself this is recorded behavior only: the AWS-managed actions are a fixed, account-independent catalog whose id, arn, targets and description are published by FIS (fis-actions-reference), and the recorded response is that published text verbatim. The description is fixed text per action id — it interpolates nothing from the request, the account or the region (only the `arn` carries account/region). Examples of the required text, by action id: aws:eks:pod-delete = "Deletes pods of a given Kubernetes namespace using pod identifying information such as label selectors, deployment names or pod names."; aws:eks:pod-network-blackhole-port = "Drops incoming or outgoing traffic for a configurable protocol (tcp or udp) and port which is useful for simulating dependency failures."; aws:eks:pod-network-latency = "Adds latency, with jitter, to outgoing or incoming traffic from a configurable list of sources (Supported: IPv4, IPv4/CIDR, domain name, DYNAMODB|S3)."; aws:eks:pod-network-packet-loss = "Adds packet loss to outgoing or incoming traffic from a configurable list of sources (Supported: IPv4, IPv4/CIDR, domain name, DYNAMODB|S3)."; aws:eks:terminate-nodegroup-instances = "Terminates a percentage of the underlying EC2 instances in an EKS cluster."; aws:elasticache:replicationgroup-interrupt-az-power = "Simulate AZ outage in ElastiCache replication group.".
  Emulator: ListActions answers HTTP 200 (the expected status) but serves its own paraphrase for each of these ids — aws:eks:pod-delete = "Deletes a set of Kubernetes pods.", aws:eks:pod-network-blackhole-port = "Drops traffic on a port of a set of Kubernetes pods.", aws:eks:pod-network-latency = "Adds latency and jitter to the network traffic of a set of Kubernetes pods.", aws:eks:pod-network-packet-loss = "Drops a percentage of the network packets of a set of Kubernetes pods.", aws:eks:terminate-nodegroup-instances = "Terminates a percentage of the instances of a set of Amazon EKS node groups.", aws:elasticache:replicationgroup-interrupt-az-power = "Interrupts power in one Availability Zone of a set of ElastiCache replication groups.". The replay reports the same value_mismatch on `description` for 23 action arns in total: the five aws:eks ones above, aws:elasticache:replicationgroup-interrupt-az-power, aws:fis:inject-api-internal-error, aws:fis:inject-api-throttle-error, aws:fis:inject-api-unavailable-error, aws:fis:wait, aws:kinesis:stream-expired-iterator-exception, aws:kinesis:stream-provisioned-throughput-exception, aws:lambda:invocation-add-delay, aws:lambda:invocation-error, aws:lambda:invocation-http-integration-response, aws:memorydb:multi-region-cluster-pause-replication, aws:network:disrupt-connectivity, aws:network:disrupt-vpc-endpoint, aws:network:route-table-disrupt-cross-region-connectivity, aws:network:transit-gateway-disrupt-cross-region-connectivity, aws:rds:failover-db-cluster and aws:s3:bucket-pause-replication. No field other than `description` diverges, so the ids, arns and targets of the catalog are correct and only the text is wrong.
  Why: the AWS-managed action catalog is reference data, not per-run data, so clients treat it as stable and quotable. A console, catalog browser or experiment-template authoring tool that renders `ListActions`/`GetAction` descriptions shows text that does not exist in AWS; documentation or UI snapshots generated against the emulator do not match a real account; and any client that selects or filters actions by matching on description text (searching for "label selectors", "with jitter", "IPv4/CIDR", "DYNAMODB|S3" — details the paraphrases drop entirely) finds nothing against the emulator and the right action against AWS. The paraphrases also lose semantic detail that distinguishes actions: aws:eks:pod-network-latency vs -packet-loss are described by their configurable source list in AWS, and the emulator's wording hides that.
  Fix: the FIS action-catalog seed state behind ListActions/GetAction (the built-in AWS-managed action rows in the fis service crate's state/catalog seed, not the ListActions handler, which serialises correctly). Replace each row's `description` with the published AWS catalog text for that action id so ListActions and GetAction return it verbatim; the whole string is fixed per id, with no request- or account-derived parts.
  Traces: fixture fis-catalog-list-actions step task#1 (ListActions, expected 200 / actual 200, 23 value_mismatch entries on $.actions[...].description).

Not covered by any open issue: the open FIS issue 20261117T164338Z-fis-2-net-new-causes-createexperimenttemplate-accepts-11.md names CreateExperimentTemplate's parallel-action quota and UpdateSafetyLeverState's reason-only conflict, and the open cloudformation__fis issue names AWS::FIS::ExperimentTemplate CREATE_FAILED reasons — none of them touches the action catalog's description text or ListActions.

## Evidence
Fixture fis-catalog-list-actions, step task#1 — operation ListActions, verdict mismatch, expected status 200, actual status 200 (status matches; body diverges).

Expected vs actual on $.actions[arn=...].description (value_mismatch), 6 of the 23 reported:
  aws:eks:pod-delete
    expected: 'Deletes pods of a given Kubernetes namespace using pod identifying information such as label selectors, deployment names or pod names.'
    actual:   'Deletes a set of Kubernetes pods.'
  aws:eks:pod-network-blackhole-port
    expected: 'Drops incoming or outgoing traffic for a configurable protocol (tcp or udp) and port which is useful for simulating dependency failures.'
    actual:   'Drops traffic on a port of a set of Kubernetes pods.'
  aws:eks:pod-network-latency
    expected: 'Adds latency, with jitter, to outgoing or incoming traffic from a configurable list of sources (Supported: IPv4, IPv4/CIDR, domain name, DYNAMODB|S3).'
    actual:   'Adds latency and jitter to the network traffic of a set of Kubernetes pods.'
  aws:eks:pod-network-packet-loss
    expected: 'Adds packet loss to outgoing or incoming traffic from a configurable list of sources (Supported: IPv4, IPv4/CIDR, domain name, DYNAMODB|S3).'
    actual:   'Drops a percentage of the network packets of a set of Kubernetes pods.'
  aws:eks:terminate-nodegroup-instances
    expected: 'Terminates a percentage of the underlying EC2 instances in an EKS cluster.'
    actual:   'Terminates a percentage of the instances of a set of Amazon EKS node groups.'
  aws:elasticache:replicationgroup-interrupt-az-power
    expected: 'Simulate AZ outage in ElastiCache replication group.'
    actual:   'Interrupts power in one Availability Zone of a set of ElastiCache replication groups.'

The remaining diverging action ids (same field, same kind of paraphrase) are: aws:fis:inject-api-internal-error, aws:fis:inject-api-throttle-error, aws:fis:inject-api-unavailable-error, aws:fis:wait, aws:kinesis:stream-expired-iterator-exception, aws:kinesis:stream-provisioned-throughput-exception, aws:lambda:invocation-add-delay, aws:lambda:invocation-error, aws:lambda:invocation-http-integration-response, aws:memorydb:multi-region-cluster-pause-replication, aws:network:disrupt-connectivity, aws:network:disrupt-vpc-endpoint, aws:network:route-table-disrupt-cross-region-connectivity, aws:network:transit-gateway-disrupt-cross-region-connectivity, aws:rds:failover-db-cluster, aws:s3:bucket-pause-replication.

The account id and region in the reported arns (000000000000, us-west-2) are environment values and are not part of the requirement; only the per-id description text is.

Model reference: ActionSummary.description -> ActionDescription (string, @length 0..512, @pattern ^[\s\S]+$); ListActions GET /actions -> 200 with actions: ActionSummaryList.

## Steps to Reproduce
1. Boot the emulator and point a FIS client at it (no setup needed — the AWS-managed action catalog is built in).
2. Call fis.ListActions (GET /actions), paginating with maxResults/nextToken until all actions are collected; or call fis.GetAction(id="aws:eks:pod-delete") for a single row.
3. Read each returned action's `description`.
4. Compare against the AWS action catalog text for the same id. AWS returns e.g. for aws:eks:pod-delete "Deletes pods of a given Kubernetes namespace using pod identifying information such as label selectors, deployment names or pod names."; the emulator returns "Deletes a set of Kubernetes pods.". The same mismatch appears on 22 further ids listed in the evidence, while `id`, `arn` and `targets` agree.
5. Replay fixture fis-catalog-list-actions step task#1 to see the full 23-field value_mismatch cluster.
