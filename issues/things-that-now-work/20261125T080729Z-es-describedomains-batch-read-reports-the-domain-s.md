Issue-Source: trace-sweep-divergence
Issue-Trace: opensearch-domain-lifecycle-and-config-inspection
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: opensearch-domain-lifecycle-and-config-inspection
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: es|DescribeDomains|2xx->2xx|$.DomainStatusList[*]
Issue-Commit: c04bccdb0ecb00ae5d5856d22919c31d15c06321

# es DescribeDomains batch read reports the domain's AccessPolicies pending value with a rewritten IAM principal

> _Note: this issue was identified against the code base at commit `c04bccdb0ecb00ae5d5856d22919c31d15c06321`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDomains

## Description
ONE net-new cause on the es (OpenSearch) multi-domain read path. The operation answers the modelled status (200 -> 200); the divergence is entirely in the response body, and the comparator reports it as whole-element inequality at $.DomainStatusList[0] because DescribeDomains returns a list of DomainStatus and the element it serves for the requested domain is not the DomainStatus AWS records for that domain.

Cause 1 of 1 - DescribeDomains (fixture opensearch-domain-lifecycle-and-config-inspection, step task#3): the DomainStatus element the batch read serves for the domain carries, in its ModifyingProperties entry for AccessPolicies, a PendingValue whose IAM principal has been rewritten from the form the request submitted.

AWS: Smithy com.amazonaws.opensearch#DescribeDomains{input DomainNames: DomainNameList (required); output DomainStatusList: DomainStatusList (required)}, HTTP POST /2021-01-01/opensearch/domain-info, success 200 — documented "Returns domain configuration information about the specified Amazon OpenSearch Service domains", i.e. the batch read reports exactly the DomainStatus the single-domain DescribeDomain reports for the same domain, one entry per requested name. Within DomainStatus, com.amazonaws.opensearch#ModifyingProperties{Name: String, ActiveValue: String, PendingValue: String, ValueType: PropertyValueType[PLAIN_TEXT|STRINGIFIED_JSON]} declares PendingValue a plain String and prescribes no normalization of its contents. Recorded behavior: the AccessPolicies PendingValue is the policy document EXACTLY as the request supplied it, so a Statement[].Principal.AWS submitted as a bare account id is read back as that bare account id.

Emulator: the DomainStatus element served at DomainStatusList[0] differs from the recorded one. The diverging member is the AccessPolicies ModifyingProperties PendingValue: it comes back with Principal {"AWS":"<account-id>"} expanded to {"AWS":"arn:aws:iam::<account-id>:root"}, the rest of the document unchanged. This is the same member, for the same domain in the same fixture run, that the single-domain read diverges on one step earlier (DescribeDomain, step task#2, reported in this same replay as the ONLY diverging member of that response), and the two read paths share one projection: services/service-com-amazonaws-opensearch-amazonopensearchservice/src/ops/describe_domains.rs:181 calls crate::service::domain_configuration::reported_status, whose line 471 fills modifying_properties from reported_modifying_properties (domain_configuration.rs:642), which takes the AccessPolicies leaf from the stored domain.access_policies (domain_configuration.rs:945). The stored copy is the canonicalized one, so every read path that projects it — including this batch one — republishes the rewrite.

Why: the emulator silently alters a security document the caller authored, on the read a client uses to inspect several domains at once. A client that reads the policy back, edits one statement and re-submits it writes a policy it never wrote; a configuration-management client that diffs the policy it submitted against the policy the batch read reports sees a spurious change on every poll and re-applies the policy forever. Because this is the batch read, the damage is per-domain across a whole fleet inspection, not one domain.

Fix: the domain access-policy storage path (store the submitted policy document verbatim — no principal canonicalization) and the shared DomainStatus projection that DescribeDomains serializes, so the batch element equals the single-domain read's DomainStatus member for member.

Traces: opensearch-domain-lifecycle-and-config-inspection#task#3 (DescribeDomains, the net-new cluster). Code: services/service-com-amazonaws-opensearch-amazonopensearchservice/src/ops/describe_domains.rs:180-184; src/service/domain_configuration.rs:468-474, 642-648, 943-945.

Open issues that do NOT cover this cluster: 20261124T122846Z-es-the-submitted-access-policy-is-rewritten-and.md describes the same policy-rewrite class on DescribeDomain (task#2), DescribeDomainConfig (task#4), DescribeDomainChangeProgress (task#5) and CreateDomain, and its Issue-Key lines name only those four operations — the replay reports this DescribeDomains cluster as owned by no open issue, so no filed replay key re-runs step task#3. 20261117T201942Z-es-instance-type-and-service-software-catalogs-are.md covers the ServiceSoftwareOptions.CurrentVersion catalog on DescribeDomain for a different fixture and says nothing about the batch read. Fixing the shared storage path is expected to close this cluster too, but step task#3 must be replayed explicitly to confirm it.

## Evidence
Replayed corpus simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, 99 fixtures, 492 steps, 354 passed, 138 mismatched, 0 errored, 0 blocked, score 72.0%, 33 divergence clusters. This is the ONLY cluster the replay reports with an empty owned_by and an empty stub_path.

Cluster: service es, operation DescribeDomains, fixture opensearch-domain-lifecycle-and-config-inspection, step task#3, verdict mismatch, expected status 200, actual status 200, affected_count 1, divergence_key "service=es|DescribeDomains|status=200->200|keys=$.DomainStatusList[0]".

  $.DomainStatusList[0]: missing — the expected element is a full DomainStatus for the fixture's domain (ARN arn:aws:es:<region>:<account-id>:domain/<domain-name>, AccessPolicies the submitted document, AdvancedOptions {override_main_response_version:"false", rest.action.multi.allow_explicit_index:"true"}, ClusterConfig InstanceCount 1 / InstanceType t3.small.search, ChangeProgressDetails ConfigChangeStatus "Pending", AutomatedSnapshotPauseOptions {Enabled:false, State:"Disabled"}, DeploymentStrategyOptions {DeploymentStrategy:"CapacityOptimized"}, AIMLOptions with NaturalLanguageQueryGenerationOptions CurrentState NOT_ENABLED / DesiredState DISABLED). The replay's detail string for this cluster is cut off by the report's per-cluster detail budget before the "got" element, so the diverging member is not spelled out in the cluster itself.

The diverging member is pinned by the sibling cluster for the SAME fixture run and the SAME domain, one step earlier, which this replay reports in full:

  fixture opensearch-domain-lifecycle-and-config-inspection step task#2, service es, operation DescribeDomain, 200 -> 200:
    $.DomainStatus.ModifyingProperties[3].PendingValue: value_mismatch
      expected '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"<account-id>"},"Action":"es:*","Resource":"arn:aws:es:<region>:<account-id>:domain/<domain-name>/*"}]}'
      got      the same document with "Principal":{"AWS":"arn:aws:iam::<account-id>:root"}
    (divergence_key "service=es|DescribeDomain|status=200->200|keys=$.DomainStatus.ModifyingProperties[3].PendingValue" — that single member is the ONLY divergence in the whole DescribeDomain body.)

Because DescribeDomains projects each element through the identical reported_status/reported_modifying_properties path (describe_domains.rs:181 -> domain_configuration.rs:468-474 -> 642-648 -> 945), the one member that makes the single-domain DomainStatus unequal is the one that makes the batch element unequal.

Per-run values are NOT the requirement: the account id, region, the generated domain name, the ChangeId, the LastUpdatedTime/StartTime epochs and the ModifyingProperties element index of this recording all vary per environment. The requirement is that DescribeDomains' DomainStatusList element for a requested domain equals, member for member, the DomainStatus AWS reports for that domain, and specifically that the AccessPolicies ModifyingProperties PendingValue echoes the submitted policy document byte-for-byte with its Statement[].Principal.AWS in the form the request used.

Smithy cited (services/service-com-amazonaws-opensearch-amazonopensearchservice/smithy-model/com-amazonaws-opensearch-amazonopensearchservice.json): com.amazonaws.opensearch#DescribeDomains — input DescribeDomainsRequest{DomainNames: DomainNameList required}, output DescribeDomainsResponse{DomainStatusList: DomainStatusList required}, http POST /2021-01-01/opensearch/domain-info code 200, errors BaseException(400)/InternalException(500)/ValidationException(400); com.amazonaws.opensearch#ModifyingProperties{Name, ActiveValue, PendingValue: String, ValueType: PropertyValueType[PLAIN_TEXT|STRINGIFIED_JSON]}.

## Steps to Reproduce
1. git_clone, then replay_wire_corpus against the emulator built from this checkout.
2. Drive fixture opensearch-domain-lifecycle-and-config-inspection to step task#3 (DescribeDomains). The preceding steps create a domain whose AccessPolicies document has Statement[].Principal.AWS set to the bare account id.
3. Compare the single element of $.DomainStatusList against the recorded DomainStatus for that domain member for member. It must be equal; today the comparison fails at $.DomainStatusList[0].
4. In that element, locate the ModifyingProperties entry whose Name is AccessPolicies and compare its PendingValue byte-for-byte with the policy document the create submitted: the principal must stay in the submitted form and must NOT be expanded to arn:aws:iam::<account-id>:root.
5. Cross-check step task#2 (DescribeDomain) of the same fixture: the same member diverges there, confirming the shared projection, and both steps must pass against their recorded responses.
6. Re-replay opensearch-domain-lifecycle-and-config-inspection#task#3 specifically — the already-filed policy-rewrite issue's replay keys do not include it, so a fix confirmed only on task#2/#4/#5 leaves this step unexercised.
