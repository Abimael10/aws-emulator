Issue-Source: fix-residue
Issue-Trace: service-quotas-catalog-list-services
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: service-quotas-catalog-list-services
Issue-Shape-Id: com.amazonaws.servicequotas#ServiceQuotasV20190624
Issue-Key: service-quotas|ListServices|2xx->2xx|$.Services[*]

# ListServices still diverges on the inventory entries whose registered strings the corpus withholds

## Operation
ListServices

## Description
RESIDUE of the fix for
`20261104T181718Z-service-quotas-listservices-catalog-omits-service-codes-and-re-words.md`
(now under `issues/things-that-now-work/`). That report quoted AWS's registered
`(ServiceCode, ServiceName)` pairs for twelve codes; eleven of them were already served
exactly, and the twelfth — `elasticfilesystem`, registered `Amazon Elastic File System
(EFS)` — was corrected by the fix, which also corrected the two further registered names
the checkout's other service-quotas reports quote (`sso` and `vpc-lattice`). The replayed
step now agrees with the recording on `$.Services[0]` through `$.Services[105]`.

What is left is the inventory entries no report in this checkout quotes a recorded value
for. The step still answers HTTP 200 and still diverges on the positions listed under
Evidence.

THE RULE (unchanged, and the one the fix applied to the quoted entries). `ListServices`
reports one `ServiceInfo` per `ServiceCode` the integrated-service inventory registers,
ordered by `ServiceCode`, and an entry's `ServiceName` is the display name the inventory
registers for THAT code, reproduced character for character — and the same string is what
`ListAWSDefaultServiceQuotas` / `GetAWSDefaultServiceQuota` / `ListServiceQuotas` report in
`ServiceQuota.ServiceName` for that code. Smithy fixes the entry SHAPE only
(`ServiceInfo{ServiceCode @length 1..63 @pattern ^[a-zA-Z][a-zA-Z0-9-]{1,63}$,
ServiceName}`, output `{NextToken, Services: ServiceInfoListDefinition}`, paginated on
`NextToken`/`MaxResults` @range 1..100); which codes the inventory holds, and the string
registered for each, are recorded behaviour, so the recorded `ListServices` response is the
reference.

WHAT A FIXER NEEDS, AND WHY THIS IS FILED SEPARATELY. The remaining entries cannot be
authored from anything in this checkout:

* the replay tool reports only the diverging `$.Services[n]` positions and withholds the
  recorded entries, and the report this residue comes from quotes AWS's pairs only for the
  codes already served;
* the positions below are not all of one kind. Because the comparison is positional and
  the two listings re-align after each diverging run (every position outside the runs
  below matches), each run holds the same NUMBER of entries on both sides, so a run is
  either a display name this catalog words differently from the inventory, or a code the
  inventory registers that this catalog does not hold standing against a code this catalog
  holds that the inventory does not — and nothing in the checkout names the codes on the
  inventory's side of that trade. The default-quota fixtures of the codes in the runs
  (`glacier`, `groundstation`, `guardduty`, `imagebuilder`, `iotevents`, `ivschat`,
  `kafkaconnect`, `internetmonitor`) report no `ServiceName` divergence, so for those codes
  the string this catalog serves is already the registered one and the divergence at their
  position is the array sliding, not their name.

Authoring a plausible display name, or inventing a `ServiceCode` to pad the array with, is
the defect itself: it would publish a code `ListAWSDefaultServiceQuotas` resolves and a
name a console renders, neither of which AWS registers. Closing this needs the recorded
`ListServices` response (its `(ServiceCode, ServiceName)` pairs).

## Evidence
Replay of trace set simparity-wire-aws-v1 after the fix, fixture
`service-quotas-catalog-list-services`, step `task#1`: expected 200, actual 200, body only.
`$.Services[0]`..`$.Services[105]` agree. The positions the replay reports, with the entry
this catalog serves at each (the reporter truncates its list after `$.Services[168]`):

- 106 `elastictranscoder` / 'Amazon Elastic Transcoder'
- 126 `glacier` / 'Amazon Glacier', 127 `globalaccelerator` / 'AWS Global Accelerator'
- 131 `groundstation` / 'AWS Ground Station', 132 `guardduty` / 'Amazon GuardDuty',
  133 `health-agent` / 'health-agent', 134 `healthlake` / 'AWS HealthLake',
  135 `iam` / 'AWS Identity and Access Management (IAM)'
- 139 `interconnect` / 'interconnect', 140 `internetmonitor` / 'Amazon CloudWatch Internet
  Monitor', 141 `iot` / 'AWS IoT Core', 142 `iotanalytics` / 'AWS IoT Analytics'
- 144 `iotevents` / 'AWS IoT Events'
- 149 `ivschat` / 'Amazon IVS Chat'
- 151 `kafkaconnect` / 'Amazon MSK Connect'
- 167 `lookoutequipment` / 'Amazon Lookout for Equipment', 168 `lookoutmetrics` / 'Amazon
  Lookout for Metrics'
- further positions past 168 that the reporter's list is truncated before.

Two entries in the list above are served with the `ServiceCode` echoed as the `ServiceName`
(`health-agent`, `interconnect`), which is the stand-in `src/state.rs` uses for a code whose
registered display name no source in this checkout states; the recorded strings for those
two are part of what this report needs.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Replay fixture `service-quotas-catalog-list-services`, step `task#1` (POST
   `ServiceQuotasV20190624.ListServices` with `{}`); the response is 200 with the whole
   inventory in one page.
3. Compare `$.Services` position by position against the recording: positions 0..105 agree,
   and the runs above diverge.
4. The step is clean when every position carries the recorded `(ServiceCode, ServiceName)`
   pair, which also makes `ServiceQuota.ServiceName` right for those codes, since
   `src/state.rs::seed_services` is the one table both listings read.
