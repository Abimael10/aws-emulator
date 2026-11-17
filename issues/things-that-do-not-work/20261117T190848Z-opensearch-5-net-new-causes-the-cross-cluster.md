Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: opensearch.add-replication-peer-dry-run-matches-live-validation opensearch.replication-peer-ops-on-missing-domain opensearch.replication-peer-ops-without-relationship-not-found opensearch.domain-replication-and-maintenance-status-missing-domain-rejected opensearch.domain-replication-and-configuration-comparison-reject-unknown-targets opensearch.compare-domain-configuration-malformed-arn-and-unknown-category-rejected
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: opensearch.add-replication-peer-dry-run-matches-live-validation|run#9|AssertionError|assert dry_code == "ValidationException", f"DryRun=True: expected ValidationException, got {dry_code}: {dry_message!r}"
Issue-Key: opensearch.replication-peer-ops-on-missing-domain|run#6|AssertionError|assert code == "ValidationException", f"AddReplicationPeer: got {code} / {message!r}"
Issue-Key: opensearch.replication-peer-ops-without-relationship-not-found|run#10|AssertionError|assert code == "ResourceNotFoundException", f"RemoveReplicationPeer: expected ResourceNotFoundException, got {code}: {message!r}"
Issue-Key: opensearch.domain-replication-and-maintenance-status-missing-domain-rejected|run#5|AssertionError|assert malformed_refusal == ("ValidationException", expected_malformed_message), (
Issue-Key: opensearch|DescribeDomainReplication|NotImplemented
Issue-Key: opensearch.compare-domain-configuration-malformed-arn-and-unknown-category-rejected|run#6|AssertionError|assert source_refusal == ("ValidationException", expected_source_message), (
Issue-Commit: 5f72b15ceea68fb83a63d5bc988a846400d3d8ef

# opensearch: 5 net-new causes — the cross-cluster-replication control plane (AddReplicationPeer, RemoveReplicationPeer, Failover/SwitchoverReplication, DescribeDomainReplication) and CompareDomainConfiguration are unrouted and answer NotImplemented

> _Note: this issue was identified against the code base at commit `5f72b15ceea68fb83a63d5bc988a846400d3d8ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AddReplicationPeer

## Description
Six behavioral checks fail because a whole family of OpenSearch control-plane operations is absent from the emulated service surface. Every call is answered by the emulator's catch-all with a non-AWS error code `NotImplemented` and the message `operation <METHOD> <path> is not implemented in this emulator`, so neither the success shape nor any of the modeled refusals of these operations can ever be observed. None of these operations appear in the service's on-disk Smithy model (services/service-com-amazonaws-opensearch-amazonopensearchservice/smithy-model/com-amazonaws-opensearch-amazonopensearchservice.json — smithy_query reports AddReplicationPeer/RemoveReplicationPeer/FailoverReplication/SwitchoverReplication/DescribeDomainReplication/CompareDomainConfiguration as 'not found'), so they are not scaffold stubs: there is no handler file for them at all, and no stub issue covers them. The statuses, codes and messages below are from the recorded AWS behavior the checks replay (recorded behavior only; the model is silent because it predates these operations). In every message, the fixed text is the quoted wording; the variable parts are the domain name from the request, the peer/domain ARN built from the request's partition, region, account and domain name, the rejected ARN or category value as sent, and the supported-category list which is AWS's fixed catalogue. Never reproduce one run's generated domain name or account id as literal text.

Cause 1 of 5: AddReplicationPeer (POST /2021-01-01/opensearch/domain/{DomainName}/replication/addPeer) is unrouted.
AWS: recorded behavior only. For a PeerDomainArn that names no existing domain, AWS refuses with 400 ValidationException and message `Domain not found: <PeerDomainArn as sent>`, and it refuses IDENTICALLY — same code, same message — whether DryRun is true or false, and establishes no relationship in either case. For a DomainName (the local domain) that does not exist, AWS refuses with ValidationException and message `Domain not found: arn:aws:es:<region>:<account>:domain/<DomainName>`.
Emulator: `NotImplemented` / 'operation POST /2021-01-01/opensearch/domain/<domain>/replication/addPeer is not implemented in this emulator'.
Why: a client cannot set up cross-cluster replication at all, and validation-before-effect (DryRun parity) is unobservable; code-based error handling on ValidationException never matches.
Fix: the AddReplicationPeer operation must exist in the opensearch service crate — model binding plus an operation handler under services/service-com-amazonaws-opensearch-amazonopensearchservice/src/ops/ — with peer-ARN and local-domain resolution in its validation path.
Traces: opensearch.add-replication-peer-dry-run-matches-live-validation, opensearch.replication-peer-ops-on-missing-domain.

Cause 2 of 5: RemoveReplicationPeer (POST /2021-01-01/opensearch/domain/{DomainName}/replication/removePeer) is unrouted.
AWS: recorded behavior only. Called on a domain that has no replication relationship (whether the domain exists or not), AWS refuses with ResourceNotFoundException and message `Domain is not replicating: arn:aws:es:<region>:<account>:domain/<DomainName>` — note the subject is the CALLING domain's own ARN, not the peer's.
Emulator: `NotImplemented` / 'operation POST /2021-01-01/opensearch/domain/<domain>/replication/removePeer is not implemented in this emulator'.
Why: a client can neither tear down replication nor distinguish 'no relationship' from 'unknown domain'; the independent read that proves AddReplicationPeer left no relationship behind is impossible.
Fix: add the RemoveReplicationPeer handler and the replication-relationship lookup it refuses on, in the opensearch service crate's ops module and its domain state.
Traces: opensearch.replication-peer-ops-without-relationship-not-found, opensearch.replication-peer-ops-on-missing-domain, opensearch.add-replication-peer-dry-run-matches-live-validation.

Cause 3 of 5: FailoverReplication and SwitchoverReplication are unrouted.
AWS: recorded behavior only. On a DomainName that does not exist both refuse with ResourceNotFoundException and message `Domain not found: arn:aws:es:<region>:<account>:domain/<DomainName>`. On an existing domain that has no replication relationship both refuse with ValidationException and message `Domain is not replicating: arn:aws:es:<region>:<account>:domain/<DomainName>`, and the domain is left untouched (DescribeDomain still reports it with Deleted=false).
Emulator: both answer `NotImplemented` from the catch-all.
Why: the two distinct refusal classes (unknown domain vs. not replicating) collapse into one non-AWS code, so a client's retry/branch logic on these follower-promotion calls is wrong in both directions.
Fix: add FailoverReplication and SwitchoverReplication handlers in the opensearch service crate's ops module, each gating on domain existence first and replication state second.
Traces: opensearch.replication-peer-ops-on-missing-domain, opensearch.replication-peer-ops-without-relationship-not-found.

Cause 4 of 5: DescribeDomainReplication (GET /2021-01-01/opensearch/domain/{DomainName}/replication) is unrouted.
AWS: recorded behavior only. For an existing domain with no replication arrangement it answers 200 with `Peers` present and EMPTY and NO `Role` member at all. For a DomainName that violates the service's domain-name pattern it refuses with 400 ValidationException and message `1 validation error detected: Value '<DomainName>' at 'domainName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-z][a-z0-9\-]+`. For a well-formed but unknown domain it refuses with ResourceNotFoundException and message `Domain not found: arn:aws:es:<region>:<account>:domain/<DomainName>`.
Emulator: `NotImplemented` / 'operation GET /2021-01-01/opensearch/domain/<domain>/replication is not implemented in this emulator'.
Why: replication state is not readable, so a client cannot confirm an arrangement exists or was removed; the constraint-violation refusal is also lost, so a malformed name is reported as a server-side gap rather than a client error.
Fix: add the DescribeDomainReplication handler in the opensearch service crate's ops module, with the domainName pattern validation running before the domain lookup, and an output serializer that omits Role when there is no arrangement.
Traces: opensearch.domain-replication-and-maintenance-status-missing-domain-rejected, opensearch.domain-replication-and-configuration-comparison-reject-unknown-targets.

Cause 5 of 5: CompareDomainConfiguration (POST /2021-01-01/opensearch/compareDomainConfiguration) is unrouted.
AWS: recorded behavior only. ARN-format and category validation run BEFORE any domain lookup. A SourceDomainArn or TargetDomainArn that is not a domain ARN is refused with 400 ValidationException and message `Invalid domain ARN format. Expected domain ARN (arn:aws*:*:*:*:domain/*) but received: <value as sent>. ` (note the trailing period-and-space, which is part of the fixed text). A Categories entry outside the supported set is refused with ValidationException and message `Unsupported category: <value as sent>. Supported: [cluster_settings, security_role_mappings, ism_policies, security_internal_users, aliases, alerting_monitors, templates, component_templates, security_roles, security_action_groups, ingest_pipelines]`. A well-formed ARN naming a domain that does not exist is refused with ValidationException and message `Source domain <domainName>: Domain <domainName> not found.` or `Target domain <domainName>: Domain <domainName> not found.` according to which side is unresolvable. A domain compared against itself answers 200 with Categories.<category>.Status == 'compared' and Diverged, SourceOnly and TargetOnly all 0.
Emulator: `NotImplemented` / 'operation POST /2021-01-01/opensearch/compareDomainConfiguration is not implemented in this emulator'.
Why: a client cannot diff two domains' configuration at all, and all four refusal classes (malformed source ARN, malformed target ARN, unresolvable source, unresolvable target, unsupported category) are indistinguishable.
Fix: add the CompareDomainConfiguration operation to the opensearch service crate — model binding, a handler under src/ops/, ARN-format and category validation ahead of domain resolution, and the per-category diff summary in the serializer.
Traces: opensearch.compare-domain-configuration-malformed-arn-and-unknown-category-rejected, opensearch.domain-replication-and-configuration-comparison-reject-unknown-targets.

No open issue covers these operations: the nearest neighbours (20261109T160051Z-opensearch-a-restart-search-process-maintenance-action-never.md, 20261030T115510Z-opensearch-the-startdomainmaintenance-getdomainmaintenancestatus-lifecycle-never-reaches-a.md) are about domain maintenance, not replication or configuration comparison.

## Evidence
Expected (AWS, from the replayed recordings):
- AddReplicationPeer, missing peer, DryRun=True and DryRun=False alike: ValidationException / 'Domain not found: arn:aws:es:<peer-region>:<account>:domain/<peer-name>'.
- AddReplicationPeer, missing local domain: ValidationException / 'Domain not found: arn:aws:es:<region>:<account>:domain/<domain>'.
- RemoveReplicationPeer, no relationship: ResourceNotFoundException / 'Domain is not replicating: arn:aws:es:<region>:<account>:domain/<domain>'.
- FailoverReplication / SwitchoverReplication, unknown domain: ResourceNotFoundException / 'Domain not found: arn:aws:es:<region>:<account>:domain/<domain>'; existing domain with no relationship: ValidationException / 'Domain is not replicating: arn:aws:es:<region>:<account>:domain/<domain>'.
- DescribeDomainReplication, unreplicated domain: 200 with Peers == [] and no Role member; malformed name: ValidationException / "1 validation error detected: Value 'Abc-Domain' at 'domainName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-z][a-z0-9\-]+"; unknown name: ResourceNotFoundException / 'Domain not found: <domain ARN>'.
- CompareDomainConfiguration: ValidationException / 'Invalid domain ARN format. Expected domain ARN (arn:aws*:*:*:*:domain/*) but received: not-an-arn. '; ValidationException / 'Unsupported category: <value>. Supported: [cluster_settings, security_role_mappings, ism_policies, security_internal_users, aliases, alerting_monitors, templates, component_templates, security_roles, security_action_groups, ingest_pipelines]'; ValidationException / 'Source domain <name>: Domain <name> not found.' and 'Target domain <name>: Domain <name> not found.'; self-compare 200 with Status 'compared' and Diverged/SourceOnly/TargetOnly == 0.

Actual (emulator replay output):
- 'AddReplicationPeer: got NotImplemented / "operation POST /2021-01-01/opensearch/domain/<domain>/replication/addPeer is not implemented in this emulator"' (opensearch.replication-peer-ops-on-missing-domain, opensearch.add-replication-peer-dry-run-matches-live-validation).
- 'RemoveReplicationPeer: expected ResourceNotFoundException, got NotImplemented: "operation POST /2021-01-01/opensearch/domain/<domain>/replication/removePeer is not implemented in this emulator"' (opensearch.replication-peer-ops-without-relationship-not-found).
- botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the DescribeDomainReplication operation: operation GET /2021-01-01/opensearch/domain/<domain>/replication is not implemented in this emulator (opensearch.domain-replication-and-configuration-comparison-reject-unknown-targets).
- 'expected ValidationException with AWS's exact message for a DomainName violating the service's pattern, got ("NotImplemented", "operation GET /2021-01-01/opensearch/domain/Abc-Domain/replication is not implemented in this emulator")' (opensearch.domain-replication-and-maintenance-status-missing-domain-rejected).
- 'expected ValidationException with AWS's exact message for a malformed SourceDomainArn, got ("NotImplemented", "operation POST /2021-01-01/opensearch/compareDomainConfiguration is not implemented in this emulator")' (opensearch.compare-domain-configuration-malformed-arn-and-unknown-category-rejected).

Model evidence: smithy_query(query="operation", operation="AddReplicationPeer,RemoveReplicationPeer,FailoverReplication,SwitchoverReplication,DescribeDomainReplication,CompareDomainConfiguration", model_path="services/service-com-amazonaws-opensearch-amazonopensearchservice/smithy-model/com-amazonaws-opensearch-amazonopensearchservice.json") returns "operation ... not found" for all six; services/service-com-amazonaws-opensearch-amazonopensearchservice/src/ops/mod.rs generates one file per modeled operation, so no handler exists for any of them.

## Steps to Reproduce
1. Build and boot the emulator and point an opensearch client at it.
2. CreateDomain a t3.small.search / gp3 10 GiB domain (EngineVersion OpenSearch_2.11) and wait for it.
3. Call AddReplicationPeer(DomainName=<domain>, PeerDomainArn=arn:aws:es:us-east-1:<account>:domain/<never-created>, DryRun=True) and again with DryRun=False — expect identical ValidationException 'Domain not found: <peer arn>'; observe NotImplemented.
4. Call RemoveReplicationPeer / FailoverReplication(AllowDataLoss=True) / SwitchoverReplication on that domain with the same peer ARN — expect ResourceNotFoundException / ValidationException 'Domain is not replicating: <own arn>'; observe NotImplemented.
5. Repeat steps 3-4 against a domain name that was never created — expect 'Domain not found: <own arn>'; observe NotImplemented.
6. Call DescribeDomainReplication(DomainName=<domain>) — expect 200 with Peers=[] and no Role; then with DomainName='Abc-Domain' (pattern violation) and with an unknown well-formed name.
7. Call CompareDomainConfiguration with SourceDomainArn='not-an-arn', with TargetDomainArn='also-not-an-arn', with Categories=['not_a_real_category'], with an unresolvable source/target ARN, and with the real domain ARN on both sides and Categories=['cluster_settings'].
8. Re-replay: opensearch.add-replication-peer-dry-run-matches-live-validation, opensearch.replication-peer-ops-on-missing-domain, opensearch.replication-peer-ops-without-relationship-not-found, opensearch.domain-replication-and-maintenance-status-missing-domain-rejected, opensearch.domain-replication-and-configuration-comparison-reject-unknown-targets, opensearch.compare-domain-configuration-malformed-arn-and-unknown-category-rejected.
