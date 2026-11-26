Issue-Source: trace-sweep-divergence
Issue-Trace: es-engine-version-and-instance-catalog
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: es-engine-version-and-instance-catalog opensearch-domain-version-capacity-and-software-update-management esdef-create-domain-gp3-server-defaults
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: es|ListInstanceTypeDetails|2xx->2xx|$.InstanceTypeDetails[*]
Issue-Key: es|DescribeInstanceTypeLimits|4xx->4xx|$.message
Issue-Key: es|DescribeDomain|2xx->2xx|$.DomainStatus.ServiceSoftwareOptions.CurrentVersion
Issue-Commit: 80a7c7f5468625a96d1dbbdd9db26cb93639edde

# es instance-type catalog ordering and service-software release label are emulator-authored, not AWS's

> _Note: this issue was identified against the code base at commit `80a7c7f5468625a96d1dbbdd9db26cb93639edde`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListInstanceTypeDetails

## Description
Four root causes, all on read-only metadata paths of the OpenSearch/Elasticsearch control plane (service token `es`, Smithy service com.amazonaws.opensearch#AmazonOpenSearchService). In every one the HTTP status matches and only served values diverge: the emulator publishes its own catalog sequence and its own service-software release stamp instead of the ones AWS serves. A fixer MUST close all FOUR causes before submitting.

Cause 1 of 4: ListInstanceTypeDetails publishes the instance-type catalog in an order AWS does not use.
AWS: Smithy com.amazonaws.opensearch#ListInstanceTypeDetails - GET /2021-01-01/opensearch/instanceTypeDetails/{EngineVersion} (EngineVersion @required @httpLabel, VersionString length 14..18, pattern ^Elasticsearch_[0-9]{1}\.[0-9]{1,2}$|^OpenSearch_[0-9]{1,2}\.[0-9]{1,2}$; MaxResults @httpQuery range max 100; NextToken @httpQuery; RetrieveAZs @httpQuery), success status 200, output InstanceTypeDetails -> InstanceTypeDetailsList of InstanceTypeDetails{InstanceType -> OpenSearchPartitionInstanceType enum, EncryptionEnabled, CognitoEnabled, AppLogsEnabled, AdvancedSecurityEnabled, WarmEnabled, InstanceRole -> InstanceRoleList, AvailabilityZones -> AvailabilityZoneList}; the operation is @paginated (inputToken NextToken / outputToken NextToken / pageSize MaxResults). The SEQUENCE the list is served in is recorded behavior only: the recording leads with the current-generation families - index 0 m7i.2xlarge.search, index 2 c8g.12xlarge.search, index 3 oi2.8xlarge.search, index 4 r7i.12xlarge.search - each entry carrying its own InstanceRole set (oi2.8xlarge.search carries ['data'] only).
Emulator: answers 200 but leads with the legacy c5 family - index 0 c5.large.search, index 2 c5.2xlarge.search, index 3 c5.4xlarge.search, index 4 c5.9xlarge.search - and the cluster's divergence key lists roughly 160 indices of InstanceTypeDetails whose element differs. Because a different type sits at each index, the per-entry members diverge with it (at index 3 InstanceRole is served as ['data','master','coordinator'] where the recording has ['data']).
Why: this list is paginated, so its order is load-bearing rather than cosmetic - a client that reads the first page (MaxResults) to choose an instance type is offered legacy c5 types where AWS offers the current generation, and an index- or page-addressed read never reaches the types AWS puts first. Capacity planners and instance-type pickers driven off page 1 therefore pick a different type against the emulator than against AWS.
Fix: the catalog projection behind the ListInstanceTypeDetails handler in the opensearch service crate must publish the catalog in the order AWS publishes it, as the order is what paginates.
Traces: es-engine-version-and-instance-catalog#task#3 (service es, verdict mismatch, 200 -> 200).

Cause 2 of 4: the same catalog order is served for the OTHER recorded engine release, so the published order does not vary with EngineVersion.
AWS: as cause 1; for this engine release the recording leads with m7i.2xlarge.search and places c8g.12xlarge.search at index 2, r7i.12xlarge.search at index 3 and c8g.8xlarge.search at index 4 - a DIFFERENT sequence from the one recorded for the release in cause 1, i.e. the order AWS serves is a function of the EngineVersion path label.
Emulator: serves the identical c5-led sequence (index 0 c5.large.search, 2 c5.2xlarge.search, 3 c5.4xlarge.search, 4 c5.9xlarge.search) for this release too, with ~120 diverging indices in the cluster's key.
Why: an EngineVersion-scoped read answers the same ordering for every release, so a client cannot tell from the first page which types a given release leads with, and the same wrong-page effect as cause 1 applies to every release.
Fix: key the published order by the request's EngineVersion in the same handler/catalog projection, rather than emitting one global sequence.
Traces: opensearch-domain-version-capacity-and-software-update-management#task#5 (service es, operation ListInstanceTypeDetails, verdict mismatch, 200 -> 200).

Cause 3 of 4: the enum value set rendered inside the DescribeInstanceTypeLimits constraint refusal is stated in a different order from AWS's.
AWS: Smithy com.amazonaws.opensearch#DescribeInstanceTypeLimits - GET /2021-01-01/opensearch/instanceTypeLimits/{EngineVersion}/{InstanceType}, InstanceType @required @httpLabel targeting the OpenSearchPartitionInstanceType enum; modeled errors ValidationException(400), BaseException(400, defaulted), InvalidTypeException(409), LimitExceededException(409), ResourceNotFoundException(409), InternalException(500). The refusal for a path value outside the enum is the framework constraint report; the recording shows HTTP 400 with message "1 validation error detected: Value '<instanceType as sent>' at 'instanceType' failed to satisfy constraint: Member must satisfy enum value set: [<the service's value set>]". Its FIXED text is that sentence ("1 validation error detected: Value '", "' at 'instanceType' failed to satisfy constraint: Member must satisfy enum value set: [", "]"); its VARIABLE parts are the rejected value, taken verbatim from the request URI path, and the bracketed value set, which must be the service's set stated in the order the service states it - the recording's set begins m6g.xlarge.search, im4gn.8xlarge.search, m3.xlarge.search, r4.16xlarge.search, c6g.4xlarge.search, m5.xlarge.search, ... and is NOT alphabetical, family-sorted or the model's declaration order.
Emulator: answers the same 400 for the same rejected value but renders the bracketed set differently, so $.message diverges at 400 -> 400 (the replay cluster's key is service=es|DescribeInstanceTypeLimits|status=400->400|keys=$.message). Hypothesis, since the replay's detail is truncated before the emulator's rendering is quoted: the set's MEMBERSHIP was refreshed by the (now resolved) issue below and what remains divergent is the ORDER the set is stated in - the same ordering fault as causes 1 and 2, surfacing in the message instead of the body.
Why: the permitted set is machine-read out of this message by clients and SDK test fixtures, and read by humans debugging a rejected instance type; a set stated in a different order does not match the recorded refusal, so assertions on the refusal text fail and a diff of the two messages is unreadable.
Fix: the constraint-message formatter for enum-targeted members in the opensearch service crate must state OpenSearchPartitionInstanceType's values in AWS's order, the same order cause 1 asks the catalog to publish.
Traces: es-engine-version-and-instance-catalog#task#5 (service es, operation DescribeInstanceTypeLimits, verdict mismatch, 400 -> 400).

Cause 4 of 4: DescribeDomain reports a fixed service-software release stamp.
AWS: Smithy com.amazonaws.opensearch#DescribeDomain - GET /2021-01-01/opensearch/domain/{DomainName}, output DomainStatus @required; DomainStatus.ServiceSoftwareOptions -> ServiceSoftwareOptions{CurrentVersion (String, "The current service software version present on the domain"), NewVersion, UpdateAvailable, Cancellable, UpdateStatus, Description, AutomatedUpdateDate, OptionalDeployment}. The label's value is recorded behavior only: it has the form OpenSearch_<major>_<minor>_R<YYYYMMDD> and names the service-software release the service currently provisions for the domain's engine version - the rule, not one run's digits, is the requirement.
Emulator: serves one constant stamp for every OpenSearch 2.19 domain. The replay reports '$.DomainStatus.ServiceSoftwareOptions.CurrentVersion: value_mismatch (expected OpenSearch_2_19_R20260929, got OpenSearch_2_19_R20250101)' at 200 -> 200, i.e. the engine-version prefix is derived correctly and only the R<date> release component is frozen.
Why: a client that gates on CurrentVersion - a version floor check, an UpdateAvailable/StartServiceSoftwareUpdate decision, or a drift report comparing a domain against the current release - reads every domain as running an older build than the service provisions, and takes the opposite branch it would take against AWS.
Fix: the domain-status projection shared by CreateDomain/DescribeDomain in the opensearch service crate (its service-software release helper in the crate's state module) must take the release component from a per-engine-version service-software release table rather than a literal.
NOTE on history: the resolved issue 20261117T201942Z-es-instance-type-and-service-software-catalogs-are.md covered the same four clusters; it fixed the catalog's MEMBERSHIP and the accepted enum set but explicitly DECLINED the published ORDER (causes 1-3) and DECLINED this release stamp (cause 4). That issue is RESOLVED, so it owns none of these clusters - this replay reports all four as unowned (owned_by empty) - and they are re-filed here with that history stated so a fixer can re-argue or close them deliberately. Open issue 20261125T032424Z-es-opensearch-describeinstancetypelimits-serves-limits-for-an-instance-type.md does NOT cover cause 3: it concerns a MISSING 409 InvalidTypeException for an in-enum type a release does not offer, not the value set rendered in the 400 constraint message. Open issue 20261124T122846Z-opensearch-instance-type-and-reserved-instance-catalogs-serve.md does NOT cover causes 1-2: it carries the replay keys of the `opensearch`-token catalog clusters, while these are the `es`-token ListInstanceTypeDetails clusters, reported unowned.

## Evidence
All four clusters are replay verdict 'mismatch', service 'es', stub_path empty, owned_by empty (net-new), trace set simparity-wire-aws-v1.

1) fixture es-engine-version-and-instance-catalog, step task#3 - ListInstanceTypeDetails, expected_status 200, actual_status 200, affected_count 1:
   $.InstanceTypeDetails[0]: missing (expected {'InstanceType': 'm7i.2xlarge.search', 'AvailabilityZones': None, 'EncryptionEnabled': True, 'CognitoEnabled': True, 'AppLogsEnabled': True, 'AdvancedSecurityEnabled': True, 'WarmEnabled': True, 'InstanceRole': ['data','master','coordinator']}, got {'InstanceType': 'c5.large.search', ...})
   $.InstanceTypeDetails[2]: expected c8g.12xlarge.search, got c5.2xlarge.search
   $.InstanceTypeDetails[3]: expected oi2.8xlarge.search with InstanceRole ['data'], got c5.4xlarge.search with InstanceRole ['data','master','coordinator']
   $.InstanceTypeDetails[4]: expected r7i.12xlarge.search, got c5.9xlarge.search
   divergence_key: service=es|ListInstanceTypeDetails|status=200->200|keys=$.InstanceTypeDetails[0],[2],[3],[4],... (~160 indices)

2) fixture opensearch-domain-version-capacity-and-software-update-management, step task#5 - ListInstanceTypeDetails, 200 -> 200, affected_count 1:
   $.InstanceTypeDetails[0]: expected m7i.2xlarge.search, got c5.large.search
   $.InstanceTypeDetails[2]: expected c8g.12xlarge.search, got c5.2xlarge.search
   $.InstanceTypeDetails[3]: expected r7i.12xlarge.search, got c5.4xlarge.search
   $.InstanceTypeDetails[4]: expected c8g.8xlarge.search, got c5.9xlarge.search
   divergence_key: service=es|ListInstanceTypeDetails|status=200->200|keys=... (~120 indices). Note the EXPECTED sequence here differs from (1) at indices 3 and 4 while the EMULATOR's sequence is byte-identical to (1) - the order AWS serves varies with EngineVersion and the emulator's does not.

3) fixture es-engine-version-and-instance-catalog, step task#5 - DescribeInstanceTypeLimits, expected_status 400, actual_status 400, affected_count 1:
   $.message: value_mismatch (expected "1 validation error detected: Value 't9.nonexistent.search' at 'instanceType' failed to satisfy constraint: Member must satisfy enum value set: [m6g.xlarge.search, im4gn.8xlarge.search, m3.xlarge.search, r4.16xlarge.search, c6g.4xlarge.search, m5.xlarge.search, r7gd.4xlarge.search, m8g.12xlarge.search, ... c8g.16xlarge.search, ...]", got <the emulator's rendering; the replay detail is truncated at this point>)
   divergence_key: service=es|DescribeInstanceTypeLimits|status=400->400|keys=$.message
   Required message: fixed text "1 validation error detected: Value '" + <rejected value from the URI path> + "' at 'instanceType' failed to satisfy constraint: Member must satisfy enum value set: [" + <the service's enum value set, in AWS's order> + "]".

4) fixture esdef-create-domain-gp3-server-defaults, step task#2 - DescribeDomain, 200 -> 200, affected_count 1:
   $.DomainStatus.ServiceSoftwareOptions.CurrentVersion: value_mismatch (expected 'OpenSearch_2_19_R20260929', got 'OpenSearch_2_19_R20250101')
   divergence_key: service=es|DescribeDomain|status=200->200|keys=$.DomainStatus.ServiceSoftwareOptions.CurrentVersion
   The R<YYYYMMDD> component is a release label, stable across runs of the emulator (the same constant every time), so the divergence is not a per-run value.

Smithy grounding (smithy_query, model_path services/service-com-amazonaws-opensearch-amazonopensearchservice/smithy-model/com-amazonaws-opensearch-amazonopensearchservice.json):
  ListInstanceTypeDetails: GET /2021-01-01/opensearch/instanceTypeDetails/{EngineVersion}, code 200, paginated{inputToken NextToken, outputToken NextToken, pageSize MaxResults}, output InstanceTypeDetails -> InstanceTypeDetailsList, NextToken.
  InstanceTypeDetails members: InstanceType -> OpenSearchPartitionInstanceType (enum), EncryptionEnabled, CognitoEnabled, AppLogsEnabled, AdvancedSecurityEnabled, WarmEnabled, InstanceRole -> InstanceRoleList, AvailabilityZones -> AvailabilityZoneList.
  DescribeInstanceTypeLimits: GET /2021-01-01/opensearch/instanceTypeLimits/{EngineVersion}/{InstanceType}, InstanceType @required @httpLabel -> OpenSearchPartitionInstanceType; errors ValidationException(400), BaseException(400 defaulted), InvalidTypeException(409), LimitExceededException(409), ResourceNotFoundException(409), InternalException(500).
  DescribeDomain: GET /2021-01-01/opensearch/domain/{DomainName}, code 200, output DomainStatus @required; ServiceSoftwareOptions.CurrentVersion -> String, doc 'The current service software version present on the domain'.

## Steps to Reproduce
1. git clone the repo, build and boot the emulator from this checkout, and point an opensearch/es client at the data listener.
2. Cause 1: replay es-engine-version-and-instance-catalog#task#3 - GET /2021-01-01/opensearch/instanceTypeDetails/<the fixture's EngineVersion> (boto3: opensearch.list_instance_type_details(EngineVersion=...)). Compare InstanceTypeDetails element by element with the recorded reply: the emulator leads with c5.large.search / c5.2xlarge.search / c5.4xlarge.search / c5.9xlarge.search where AWS leads with m7i.2xlarge.search / c8g.12xlarge.search / oi2.8xlarge.search / r7i.12xlarge.search. Also call it with MaxResults=10 to see that page 1 contains a different set of types than AWS's page 1.
3. Cause 2: replay opensearch-domain-version-capacity-and-software-update-management#task#5 - the same call for the OTHER recorded EngineVersion. The emulator returns the identical c5-led sequence, while the recorded sequence for this release differs from the one in step 2 at indices 3 and 4.
4. Cause 3: replay es-engine-version-and-instance-catalog#task#5 - GET /2021-01-01/opensearch/instanceTypeLimits/<EngineVersion>/t9.nonexistent.search. Confirm HTTP 400 and diff $.message's bracketed enum value set against the recorded one token by token: the sentence must match and the set must be stated in AWS's order, beginning m6g.xlarge.search, im4gn.8xlarge.search, m3.xlarge.search, r4.16xlarge.search, ...
5. Cause 4: replay esdef-create-domain-gp3-server-defaults#task#2 - CreateDomain an OpenSearch_2.19 gp3 domain, then DescribeDomain it and read DomainStatus.ServiceSoftwareOptions.CurrentVersion. The emulator answers the same R<date> stamp on every run and for every 2.19 domain; it must name the release the service currently provisions for that engine version.
6. Re-replay ALL FOUR: es-engine-version-and-instance-catalog#task#3, opensearch-domain-version-capacity-and-software-update-management#task#5, es-engine-version-and-instance-catalog#task#5 and esdef-create-domain-gp3-server-defaults#task#2, and confirm every one passes before submitting.
