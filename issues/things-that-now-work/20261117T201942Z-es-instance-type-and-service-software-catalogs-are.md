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
Issue-Commit: 631384b37737f37e096a0856f40f7c3e45239888

# es instance-type and service-software catalogs are stale emulator-authored data

> _Note: this issue was identified against the code base at commit `631384b37737f37e096a0856f40f7c3e45239888`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListInstanceTypeDetails

## Description
Four wire clusters fail because the emulator serves its own hardcoded catalogs for the OpenSearch/Elasticsearch instance-type surface and for a domain's service-software release, instead of the catalog AWS publishes. All four are read-only metadata paths: the status matches, only the served values diverge.

Cause 1 of 4: ListInstanceTypeDetails (GET /2021-01-01/opensearch/instanceTypeDetails/{EngineVersion}) serves the wrong instance-type catalog. AWS: Smithy - ListInstanceTypeDetails output InstanceTypeDetails: InstanceTypeDetailsList of InstanceTypeDetails{InstanceType (OpenSearchPartitionInstanceType enum), EncryptionEnabled, CognitoEnabled, AppLogsEnabled, AdvancedSecurityEnabled, WarmEnabled, InstanceRole (InstanceRoleList), AvailabilityZones (AvailabilityZoneList)}; the published catalogue for a current engine version is 'recorded behavior only' and the recording shows it leads with current-generation families (m7i/c8g/oi2/r7i and the i8g/i8ge/om2/or2/r8g(d) families) and carries the AvailabilityZones member on every entry. Emulator: serves a shorter, older, differently ordered list (it leads with c5.large.search / c5.2xlarge.search / c5.4xlarge.search / c5.9xlarge.search) and omits AvailabilityZones from every entry, so the element at essentially every index of InstanceTypeDetails differs. Why: a client that enumerates supported instance types for an engine version is told current-generation types do not exist and cannot read per-type Availability Zone support, so capacity planning and any instance-type picker driven off this call is wrong. Fix: the ListInstanceTypeDetails handler's instance-type catalog in the es/opensearch service crate's ops module must be sourced from the published per-engine-version catalogue (including the AvailabilityZones member and the ordering AWS serves) rather than an emulator-authored list, and the OpenSearchPartitionInstanceType enum in the on-disk model must be refreshed to contain those families. Traces: es-engine-version-and-instance-catalog#task#3.

Cause 2 of 4: the same catalog defect on the second recorded engine version. AWS: as cause 1 - the recorded reply leads with m7i.2xlarge.search, c8g.12xlarge.search, r7i.12xlarge.search, c8g.8xlarge.search and includes AvailabilityZones per entry. Emulator: the same c5-led emulator-authored list is served for this engine version too, which also shows the catalog does not vary with EngineVersion as AWS's does. Why: as cause 1, and in addition an engine-version-sensitive catalog read returns identical results for different versions, so a client cannot tell which types a given version supports. Fix: make the catalog lookup keyed by EngineVersion in the same handler. Traces: opensearch-domain-version-capacity-and-software-update-management#task#5.

Cause 3 of 4: DescribeInstanceTypeLimits (GET /2021-01-01/opensearch/instanceTypeLimits/{EngineVersion}/{InstanceType}) refuses an out-of-enum instanceType with the wrong enum value set in its message. AWS: Smithy - DescribeInstanceTypeLimits.InstanceType targets the OpenSearchPartitionInstanceType enum and is @required/@httpLabel, and the refusal is the framework constraint message; the recording shows HTTP 400 with message '1 validation error detected: Value '<instanceType as sent>' at 'instanceType' failed to satisfy constraint: Member must satisfy enum value set: [<the service's full enum value set>]'. The fixed text is the quoted sentence; the variable parts are the rejected value, taken from the request path, and the enum value set, which must be the service's current enum. Emulator: answers the same 400 but renders a DIFFERENT enum value set in the message - the on-disk OpenSearchPartitionInstanceType enum lacks the current-generation members the recorded set contains (m7i/m8g/c7i/c8g/oi2/om2/or2/i4i/i7i/i8g/i8ge/r7i/r7g(d)/r8g(d)...), so $.message diverges. Why: a client that parses the permitted set out of the validation message, or a human reading it, is given a list that omits types the service in fact accepts. Fix: refresh the OpenSearchPartitionInstanceType enum in the service's Smithy model and have the constraint-validation path that formats this message render the full current set. Traces: es-engine-version-and-instance-catalog#task#5.

Cause 4 of 4: DescribeDomain reports a hardcoded service-software release label. AWS: Smithy - DomainStatus.ServiceSoftwareOptions (ServiceSoftwareOptions.CurrentVersion, 'The current service software version present on the domain'); the actual label is recorded behavior only, and the recording shows a freshly created OpenSearch 2.19 domain reporting the current service-software release for that engine version in the form OpenSearch_<major>_<minor>_R<YYYYMMDD>. Emulator: serves a constant release stamp (OpenSearch_2_19_R20250101) that does not track the engine version's current service-software release (the recording shows OpenSearch_2_19_R20260929 for the same engine version). Why: a client that compares CurrentVersion against the release it requires, or that drives UpdateDomainConfig/StartServiceSoftwareUpdate decisions off it, reads a domain as running an older build than the service actually provisions. Fix: derive ServiceSoftwareOptions.CurrentVersion in the CreateDomain/DescribeDomain domain-status projection from the engine version's service-software catalog instead of a hardcoded literal. Traces: esdef-create-domain-gp3-server-defaults#task#2.

Related open issue, which does NOT cover these clusters: 20261117T190848Z-opensearch-listinstancetypedetails-re-words-the-az-unsupported-instancetype.md concerns the wording of the AZ-unsupported InstanceType REFUSAL on ListInstanceTypeDetails, not the contents of the 200 catalog, the DescribeInstanceTypeLimits enum set or the service-software label; the replay reported all four clusters here as unowned.

## Evidence
All four clusters are replay verdict 'mismatch', service 'es', with matching HTTP status - only body values diverge.
1) es-engine-version-and-instance-catalog#task#3, ListInstanceTypeDetails, 200 -> 200: '$.InstanceTypeDetails[0]: missing (expected {... 'AvailabilityZones': None, 'InstanceType': 'm7i.2xlarge.search' ...}, got {'InstanceType': 'c5.large.search', ... no AvailabilityZones})', and the same for $.InstanceTypeDetails[2] (expected c8g.12xlarge.search, got c5.2xlarge.search), [3] (expected oi2.8xlarge.search, got c5.4xlarge.search), [4] (expected r7i.12xlarge.search, got c5.9xlarge.search) - the divergence key lists ~160 diverging indices.
2) opensearch-domain-version-capacity-and-software-update-management#task#5, ListInstanceTypeDetails, 200 -> 200: same shape of diff, expected [0] m7i.2xlarge.search / [2] c8g.12xlarge.search / [3] r7i.12xlarge.search / [4] c8g.8xlarge.search against the same c5-led emulator list.
3) es-engine-version-and-instance-catalog#task#5, DescribeInstanceTypeLimits, 400 -> 400: '$.message: value_mismatch (expected "1 validation error detected: Value 't9.nonexistent.search' at 'instanceType' failed to satisfy constraint: Member must satisfy enum value set: [m6g.xlarge.search, im4gn.8xlarge.search, ... m7i.2xlarge.search, r7i.12xlarge.search, i8ge.18xlarge.search, or2.16xlarge.search, c8g.16xlarge.search, ...]")'. The expected set contains families absent from the on-disk OpenSearchPartitionInstanceType enum (smithy_query query="shape" shape="OpenSearchPartitionInstanceType" lists only up to the r6gd/t4g generation), which is the set the emulator renders.
4) esdef-create-domain-gp3-server-defaults#task#2, DescribeDomain, 200 -> 200: '$.DomainStatus.ServiceSoftwareOptions.CurrentVersion: value_mismatch (expected 'OpenSearch_2_19_R20260929', got 'OpenSearch_2_19_R20250101')'. The R<date> suffix is a release label of the engine version's service software, not a per-run value: the emulator serves the same constant on every run.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and point an opensearch/es client at the data listener.
2. Cause 1: replay es-engine-version-and-instance-catalog#task#3 - GET /2021-01-01/opensearch/instanceTypeDetails/<EngineVersion>?retrieveAZs=... - and compare InstanceTypeDetails element by element with the recorded reply: the emulator's list is c5-led, shorter, and omits AvailabilityZones.
3. Cause 2: replay opensearch-domain-version-capacity-and-software-update-management#task#5 - the same call for the other recorded EngineVersion returns the identical emulator list.
4. Cause 3: replay es-engine-version-and-instance-catalog#task#5 - GET /2021-01-01/opensearch/instanceTypeLimits/<EngineVersion>/t9.nonexistent.search - and diff $.message's enum value set against the recorded one.
5. Cause 4: replay esdef-create-domain-gp3-server-defaults#task#2 - CreateDomain an OpenSearch_2.19 t3.small.search/gp3 domain, then DescribeDomain and read DomainStatus.ServiceSoftwareOptions.CurrentVersion; the emulator always answers OpenSearch_2_19_R20250101.
6. Re-replay es-engine-version-and-instance-catalog#task#3, opensearch-domain-version-capacity-and-software-update-management#task#5, es-engine-version-and-instance-catalog#task#5 and esdef-create-domain-gp3-server-defaults#task#2 to confirm all four causes are closed.

## Resolution

Causes 1, 2 and 3 — the catalog's MEMBERSHIP and, with it, the set the service ACCEPTS,
are fixed. A type the service publishes is a type a caller may name, so the
instance-type catalog and `OpenSearchPartitionInstanceType` are now one set:

* `INSTANCE_TYPE_CATALOG` publishes both halves of each generation the service offers as
  a pair — the EBS-backed `r7g` beside the already-published NVMe `r7gd`, and the Intel
  `i4i` beside the already-published Graviton `im4gn` — the two families this report's
  recorded evidence names that the table still omitted (16 instance types);
* the on-disk `OpenSearchPartitionInstanceType` enum was refreshed to the whole catalog
  (137 added values). Before this, the request boundary admitted only the model's older
  103-value snapshot, so every current-generation type the listing published — `m7i`,
  `c8g`, `oi2`, `r7i`, `i8g(e)`, `om2`, `or2`, `r8g(d)`, `r7gd`, `im4gn`, `i7i` — was
  refused with 400 on `DescribeInstanceTypeLimits`, `CreateDomain`, `UpdateDomainConfig`
  and every other member targeting that enum: the catalog advertised types the service
  would not take. The refusal's `Member must satisfy enum value set: [...]` clause states
  that same set, so it now names every family the recording names.

Still DECLINED, as in 20261114T061447Z, 20261101T032039Z and 20261030T001859Z: the ORDER
the set is stated in, which is what the `ListInstanceTypeDetails` and
`DescribeInstanceTypeLimits` replay keys still report. `InstanceTypeDetailsList` carries
no ordering trait and no documented order; the recorded orders differ between recordings
of the same operation (one leads with `m7i.2xlarge.search` and places
`c8g.12xlarge.search`, `oi2.8xlarge.search`, `r7i.12xlarge.search` at 2, 3, 4, another has
`r7i.12xlarge.search` at 3 and `c8g.8xlarge.search` at 4, a third leads with
`r5.large.search` and a fourth with `r4.2xlarge.search`), the bracketed set inside the
refusal message is in a different order again, and the recorded sequences are not the
walk of a hash table over those values either: the 31-polynomial string hash the front
door keys a value set by orders none of the recorded positions monotonically at any table
width from 16 to 4096, with no spread, with the `h ^ (h >>> 16)` spread or with the
older two-step one. It is neither the modelled value set's order nor a sort of family,
generation, size or name, so writing those positions in would be a memorised prefix
rather than a rule.

Cause 4 is DECLINED. `ServiceSoftwareOptions$CurrentVersion` is already derived rather
than fixed: `state::service_software_release` names the engine version the domain runs
(`OpenSearch_<major>_<minor>_R<release>`, the bare release label on the legacy
Elasticsearch line), the label is stable across reads, and a domain still being
provisioned or torn down publishes none — held over a table of engine versions by
`tests/all/domain_descriptor_projection.rs`. What diverges is the `R<YYYYMMDD>` stamp
alone: that is AWS's own service-software release date, published in no model and in no
machine-readable form, drifting with each release AWS ships, and the recorded value is one
run's stamp. Deriving it from the engine version would make it less faithful, not more —
AWS ships one release date across the engine versions it builds for — and copying the
recorded stamp is writing a single run's timestamp into a response.

Held by `tests/all/published_family_is_whole.rs` (new: a published family is reported by
the listing, has limits for the roles it serves with the storage its family carries,
is stated by the accepted-value set, and carries a domain a caller provisions on it, over
both halves of three generations), with rows added to
`tests/all/current_generation_instance_types.rs` and
`tests/all/published_instance_type_value_set.rs`.
