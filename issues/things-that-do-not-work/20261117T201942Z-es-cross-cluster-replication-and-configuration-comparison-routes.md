Issue-Source: trace-sweep-divergence
Issue-Trace: opensearch-domain-replication-management-rejections
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: opensearch-domain-replication-management-rejections
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: es|DescribeDomainReplication|4xx->5xx|$.message
Issue-Key: es|DescribeDomainReplication|4xx->5xx|$headers.x-amzn-errortype
Issue-Key: es|DescribeDomainReplication|4xx->5xx|status
Issue-Key: es|AddReplicationPeer|4xx->5xx|$.message
Issue-Key: es|AddReplicationPeer|4xx->5xx|$headers.x-amzn-errortype
Issue-Key: es|AddReplicationPeer|4xx->5xx|status
Issue-Key: es|RemoveReplicationPeer|4xx->5xx|$.message
Issue-Key: es|RemoveReplicationPeer|4xx->5xx|$headers.x-amzn-errortype
Issue-Key: es|RemoveReplicationPeer|4xx->5xx|status
Issue-Key: es|FailoverReplication|4xx->5xx|$.message
Issue-Key: es|FailoverReplication|4xx->5xx|$headers.x-amzn-errortype
Issue-Key: es|FailoverReplication|4xx->5xx|status
Issue-Key: es|SwitchoverReplication|4xx->5xx|$.message
Issue-Key: es|SwitchoverReplication|4xx->5xx|$headers.x-amzn-errortype
Issue-Key: es|SwitchoverReplication|4xx->5xx|status
Issue-Key: es|CompareDomainConfiguration|4xx->5xx|$.message
Issue-Key: es|CompareDomainConfiguration|4xx->5xx|$headers.x-amzn-errortype
Issue-Key: es|CompareDomainConfiguration|4xx->5xx|status
Issue-Commit: 631384b37737f37e096a0856f40f7c3e45239888

# es cross-cluster replication and configuration-comparison routes answer 501 NotImplemented

> _Note: this issue was identified against the code base at commit `631384b37737f37e096a0856f40f7c3e45239888`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDomainReplication

## Description
Six wire steps in one fixture fail with the SAME shape of defect: the OpenSearch-version (2021-01-01) domain-replication family and CompareDomainConfiguration are not routed by the es service surface at all, so the emulator's catch-all answers HTTP 501 with x-amzn-errortype 'NotImplemented' and the message 'operation <METHOD> <path> is not implemented in this emulator' instead of the modeled refusal AWS makes. Each route is a separate cause because each one has its own status, error code and message template. None of these six operations is present in the on-disk Smithy model (smithy_query query="operation" on services/service-com-amazonaws-opensearch-amazonopensearchservice/smithy-model/com-amazonaws-opensearch-amazonopensearchservice.json reports AddReplicationPeer/RemoveReplicationPeer/FailoverReplication/SwitchoverReplication/DescribeDomainReplication/CompareDomainConfiguration as not found), so for every cause the status/code/message requirement is 'recorded behavior only', taken from the replayed response. In every message the fixed text is the quoted wording; the variable parts are the domain name from the request path and the domain ARN composed from the request's partition, region, caller account and that domain name, plus (for CompareDomainConfiguration) the rejected category value exactly as sent. The supported-category list is AWS's fixed catalogue and is part of the fixed text. Never reproduce a recording's generated domain name or the 000000000000 account literally.

Cause 1 of 6: DescribeDomainReplication (GET /2021-01-01/opensearch/domain/{DomainName}/replication) on a domain that does not exist. AWS: recorded behavior only - HTTP 404, x-amzn-errortype 'ResourceNotFoundException', message 'Domain not found: arn:aws:es:<region>:<account>:domain/<DomainName>' (note the recorded status is 404, not the 409 the model's ResourceNotFoundException httpError would default to for this service). Emulator: HTTP 501, 'NotImplemented', 'operation GET /2021-01-01/opensearch/domain/<DomainName>/replication is not implemented in this emulator'. Why: replication state is unreadable and a client branching on ResourceNotFoundException instead sees a server-side gap, so it retries a permanent client error. Fix: add the DescribeDomainReplication route and handler to the opensearch/es service crate's ops module, resolving the domain before serving Peers. Traces: opensearch-domain-replication-management-rejections#task#1.

Cause 2 of 6: AddReplicationPeer (POST /2021-01-01/opensearch/domain/{DomainName}/replication/addPeer) naming a local domain that does not exist. AWS: recorded behavior only - HTTP 400, x-amzn-errortype 'ValidationException', message 'Domain not found: arn:aws:es:<region>:<account>:domain/<DomainName>'. Emulator: HTTP 501, 'NotImplemented', 'operation POST /2021-01-01/opensearch/domain/<DomainName>/replication/addPeer is not implemented in this emulator'. Why: cross-cluster replication cannot be established, and the 400 client-error class is reported as a 501, so validation-before-effect is unobservable. Fix: add the AddReplicationPeer route and handler with local-domain and peer-ARN resolution in its validation path. Traces: opensearch-domain-replication-management-rejections#task#2.

Cause 3 of 6: RemoveReplicationPeer (POST /2021-01-01/opensearch/domain/{DomainName}/replication/removePeer) on a domain with no replication relationship. AWS: recorded behavior only - HTTP 404, 'ResourceNotFoundException', message 'Domain is not replicating: arn:aws:es:<region>:<account>:domain/<DomainName>' - the subject is the CALLING domain's own ARN, not the peer's. Emulator: HTTP 501, 'NotImplemented', 'operation POST /2021-01-01/opensearch/domain/<DomainName>/replication/removePeer is not implemented in this emulator'. Why: a client can neither tear down replication nor distinguish 'no relationship' from 'unknown domain'. Fix: add the RemoveReplicationPeer handler and the replication-relationship lookup it refuses on, in the service crate's ops module and its domain state. Traces: opensearch-domain-replication-management-rejections#task#3.

Cause 4 of 6: FailoverReplication (POST /2021-01-01/opensearch/domain/{DomainName}/replication/failover) on a domain that does not exist. AWS: recorded behavior only - HTTP 404, 'ResourceNotFoundException', message 'Domain not found: arn:aws:es:<region>:<account>:domain/<DomainName>'. Emulator: HTTP 501, 'NotImplemented', 'operation POST /2021-01-01/opensearch/domain/<DomainName>/replication/failover is not implemented in this emulator'. Why: follower promotion is unavailable and the unknown-domain refusal class is lost. Fix: add the FailoverReplication handler, gating on domain existence first and replication state second. Traces: opensearch-domain-replication-management-rejections#task#4.

Cause 5 of 6: SwitchoverReplication (POST /2021-01-01/opensearch/domain/{DomainName}/replication/switchover) on a domain that does not exist. AWS: recorded behavior only - HTTP 404, 'ResourceNotFoundException', message 'Domain not found: arn:aws:es:<region>:<account>:domain/<DomainName>'. Emulator: HTTP 501, 'NotImplemented', 'operation POST /2021-01-01/opensearch/domain/<DomainName>/replication/switchover is not implemented in this emulator'. Why: same as cause 4 for the planned-switchover path; a client cannot tell a missing domain from an unimplemented service. Fix: add the SwitchoverReplication handler with the same existence-then-state gating. Traces: opensearch-domain-replication-management-rejections#task#5.

Cause 6 of 6: CompareDomainConfiguration (POST /2021-01-01/opensearch/compareDomainConfiguration) with a Categories entry outside the supported set. AWS: recorded behavior only - HTTP 400, 'ValidationException', message 'Unsupported category: <value as sent>. Supported: [cluster_settings, security_role_mappings, ism_policies, security_internal_users, aliases, alerting_monitors, templates, component_templates, security_roles, security_action_groups, ingest_pipelines]'; category validation runs before any domain lookup. Emulator: HTTP 501, 'NotImplemented', 'operation POST /2021-01-01/opensearch/compareDomainConfiguration is not implemented in this emulator'. Why: configuration diffing is unavailable and the category-validation refusal is indistinguishable from an unsupported service. Fix: add the CompareDomainConfiguration route and handler with category and ARN-format validation ahead of domain resolution. Traces: opensearch-domain-replication-management-rejections#task#6.

Related open issue, which does NOT cover these wire steps: 20261117T190848Z-opensearch-5-net-new-causes-the-cross-cluster.md is a BEHAVIORAL (simparity-behavioral-aws-v1) issue about the same operation family; it carries only behavioral check ids, no wire replay key, so the replay reported these six clusters as unowned and they are filed here with their fixture/step bindings.

## Evidence
Fixture opensearch-domain-replication-management-rejections, all six task steps (replay verdict 'mismatch', service 'es'):
- task#1 DescribeDomainReplication: expected status 404 / x-amzn-errortype 'ResourceNotFoundException' / message 'Domain not found: arn:aws:es:us-west-2:000000000000:domain/<domain>'; actual status 501 / 'NotImplemented' / 'operation GET /2021-01-01/opensearch/domain/<domain>/replication is not implemented in this emulator'.
- task#2 AddReplicationPeer: expected 400 / 'ValidationException' / 'Domain not found: arn:aws:es:us-west-2:000000000000:domain/<domain>'; actual 501 / 'NotImplemented' / '... /replication/addPeer is not implemented in this emulator'.
- task#3 RemoveReplicationPeer: expected 404 / 'ResourceNotFoundException' / 'Domain is not replicating: arn:aws:es:us-west-2:000000000000:domain/<domain>'; actual 501 / 'NotImplemented' / '... /replication/removePeer is not implemented in this emulator'.
- task#4 FailoverReplication: expected 404 / 'ResourceNotFoundException' / 'Domain not found: arn:aws:es:us-west-2:000000000000:domain/<domain>'; actual 501 / 'NotImplemented' / '... /replication/failover is not implemented in this emulator'.
- task#5 SwitchoverReplication: expected 404 / 'ResourceNotFoundException' / 'Domain not found: arn:aws:es:us-west-2:000000000000:domain/<domain>'; actual 501 / 'NotImplemented' / '... /replication/switchover is not implemented in this emulator'.
- task#6 CompareDomainConfiguration: expected 400 / 'ValidationException' / 'Unsupported category: ClusterConfig. Supported: [cluster_settings, security_role_mappings, ism_policies, security_internal_users, aliases, alerting_monitors, templates, component_templates, security_roles, security_action_groups, ingest_pipelines]'; actual 501 / 'NotImplemented' / 'operation POST /2021-01-01/opensearch/compareDomainConfiguration is not implemented in this emulator'.
The domain names and the 000000000000 account above are this recording's values filling the message templates; only the quoted wording and the supported-category list are fixed text.
Model evidence: smithy_query(query="operation", operation="AddReplicationPeer,RemoveReplicationPeer,FailoverReplication,SwitchoverReplication,DescribeDomainReplication,CompareDomainConfiguration", model_path="services/service-com-amazonaws-opensearch-amazonopensearchservice/smithy-model/com-amazonaws-opensearch-amazonopensearchservice.json") returns none of the six - the routes are absent, not stubbed (replay reported stub_path empty and not_implemented=0).

## Steps to Reproduce
1. Build and boot the emulator from this checkout and point an opensearch/es client at the data listener.
2. Replay fixture opensearch-domain-replication-management-rejections (steps task#1 .. task#6).
3. task#1: GET /2021-01-01/opensearch/domain/<never-created-domain>/replication - expect 404 ResourceNotFoundException 'Domain not found: <domain ARN>'; observe 501 NotImplemented.
4. task#2: POST /2021-01-01/opensearch/domain/<never-created-domain>/replication/addPeer - expect 400 ValidationException 'Domain not found: <domain ARN>'.
5. task#3: POST .../replication/removePeer on a domain with no relationship - expect 404 ResourceNotFoundException 'Domain is not replicating: <domain ARN>'.
6. task#4 and task#5: POST .../replication/failover and .../replication/switchover on a never-created domain - expect 404 ResourceNotFoundException 'Domain not found: <domain ARN>'.
7. task#6: POST /2021-01-01/opensearch/compareDomainConfiguration with Categories containing an unsupported value - expect 400 ValidationException 'Unsupported category: <value>. Supported: [cluster_settings, ...]'.
8. Re-replay opensearch-domain-replication-management-rejections#task#1 through #task#6 to confirm all six causes are closed.
