Issue-Source: trace-sweep-divergence
Issue-Trace: supplychain-bill-of-materials-import-job
Issue-Step: setup#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: supplychain-bill-of-materials-import-job supplychain-datalake-namespace-dataset supplychain-instance-lifecycle supplychain-list-instances-pagination supplychain-tagging-workflow
Issue-Shape-Id: com.amazonaws.supplychain#GalaxyPublicAPIGateway
Issue-Key: scn|CreateInstance|2xx->2xx|$.instance.idcPortalDomain
Issue-Key: scn|CreateInstance|2xx->2xx|$.instance.primaryDomain
Issue-Key: scn|CreateInstance|2xx->2xx|$.instance.webAppDnsDomain
Issue-Commit: c9f8e020bc3ef948218b81db7247c67e862725ba

# scn CreateInstance instance domains announce an unserved scn.global.on.aws host

> _Note: this issue was identified against the code base at commit `c9f8e020bc3ef948218b81db7247c67e862725ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateInstance

## Description
The `Instance` projection that every scn instance operation answers from builds its host-bearing members from a hard-coded public AWS DNS zone and from the instance id, so a CreateInstance 200 announces hostnames the emulator does not serve and a domain label that is just the instance identifier. Both causes live in the single projection `InstanceRecord::to_wire` and the two domain helpers it calls, so they are one fault with two observable faces. 2 causes.

Cause 1 of 2 — primaryDomain and idcPortalDomain announce a host the emulator does not own (fixture supplychain-bill-of-materials-import-job, step setup#1).
AWS: recorded behavior only. The on-disk Smithy model (services/service-com-amazonaws-supplychain-galaxypublicapigateway, sdkId SupplyChain / endpointPrefix scn) models `Instance` with instanceId, awsAccountId, state, errorMessage, webAppDnsDomain, createdTime, lastModifiedTime, instanceName, instanceDescription, kmsKeyArn and versionNumber — it declares NO `primaryDomain` and NO `idcPortalDomain` member at all. The recording shows CreateInstance answering HTTP 200 with both members present and carrying a hostname that is REACHABLE as the instance's web endpoint; the replay asserts them shape-only (`<shaped-not-asserted>`), i.e. any host is acceptable provided it is a host this endpoint actually serves. The requirement is therefore structural: an instance's announced web-app and Identity-Center hostnames must address the endpoint the client is already talking to.
Emulator: it answers 200 with `$.instance.primaryDomain` and `$.instance.idcPortalDomain` set to `<webAppDnsDomain-label>.scn.global.on.aws` (replay detail: `host_not_own`, got `86668e01-6585-4c9b-8343-157437069ca7.scn.global.on.aws`). The suffix is the fixed constant `WEB_APP_ZONE = "scn.global.on.aws"` and the hostname is assembled unconditionally from it, with no reference to the host or port the request arrived on. No HTTP status or error-code divergence is involved: both sides are 200 and no modeled error (ValidationException 400 / ConflictException 409 / ServiceQuotaExceededException 402) is in play here.
Why: a client that reads primaryDomain or idcPortalDomain to reach the instance's web app or Identity Center portal is handed a name in AWS's real public zone instead of the emulator it is driving. The lookup either fails to resolve or resolves away from the emulator, so any flow that follows the announced domain (browser hand-off, redirect construction, health probe) leaves the emulator entirely — and an assertion that the announced host is reachable can never pass. Note the names are also not per-environment: two different emulator deployments announce the identical hostname.
Fix: the `Instance` projection and the hostname helper it uses in this service's state — the domains must be derived from the endpoint the emulator is reached on rather than from a compiled-in public zone, and the projection must keep both members consistent with that.
Traces: supplychain-bill-of-materials-import-job#setup#1 (cluster representative; affected_count 1). Fix sites: services/service-com-amazonaws-supplychain-galaxypublicapigateway/src/state.rs lines 74-89 (WEB_APP_ZONE and `web_app_zone_hostname`) and lines 197-221 (`InstanceRecord::to_wire`, which sets primary_domain/idc_portal_domain); produced on the create path at src/ops/create_instance.rs line 263.

Cause 2 of 2 — the generated webAppDnsDomain is the instance id verbatim, and the derived domains inherit it (fixture supplychain-datalake-namespace-dataset, step setup#1).
AWS: recorded behavior only for the generated value; the model constrains its form. Smithy: `Instance$webAppDnsDomain` targets `InstanceWebAppDnsDomain` with @pattern `^(?![-])[a-zA-Z0-9-]{1,62}[a-zA-Z0-9]$`, documented as "The DNS subdomain of the web app. This would be \"example\" in the URL \"example.scn.global.on.aws\"" — i.e. a service-generated DNS LABEL, distinct from and independent of `Instance$instanceId` (a separate member, UUID-shaped, @pattern `^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`). The recording of a CreateInstance that names no webAppDnsDomain shows a 200 whose webAppDnsDomain is such a generated label, asserted shape-only; the replay's shape check for that member rejects the UUID the emulator returns.
Emulator: for a create request that omits webAppDnsDomain it answers 200 with `$.instance.webAppDnsDomain` equal to the new instanceId (replay detail: `value_mismatch`, expected `<shaped-not-asserted>`, got `963098ec-f9be-4bcb-aab1-85ddb60b0433`), and then builds `primaryDomain`/`idcPortalDomain` from that same value, so the same step also reports `host_not_own` on both (got `963098ec-f9be-4bcb-aab1-85ddb60b0433.scn.global.on.aws`). Again both sides are 200 with no error code involved.
Why: the web-app DNS label and the instance identifier are two different identities in this API, and the emulator collapses them. A client that keys anything on the label (a URL it builds, a DNS record it expects, a uniqueness check against a label another instance holds) sees the instance id instead of a label, so label-vs-id handling, label-collision behavior and any assertion that the two members differ all behave unlike AWS. The value is also not a plausible subdomain for the documented `label.scn.global.on.aws` form.
Fix: the generated-domain helper in this service's state (and the create handler's use of it), so an omitted webAppDnsDomain yields an independently generated DNS label satisfying `InstanceWebAppDnsDomain`'s pattern rather than the instance id — and the projection in Cause 1 then derives the hostnames from that label plus the emulator's own reachable host.
Traces: supplychain-datalake-namespace-dataset#setup#1 (cluster representative; affected_count 5 — also supplychain-instance-lifecycle#task#1, supplychain-list-instances-pagination#setup#1, supplychain-list-instances-pagination#setup#2, supplychain-tagging-workflow#setup#1). Fix sites: services/service-com-amazonaws-supplychain-galaxypublicapigateway/src/state.rs lines 91-104 (`generated_web_app_dns_domain`, which returns `instance_id.to_string()`) and src/ops/create_instance.rs lines 248-256.

Neither cause is covered by an open issue. The existing open scn issues cover different faults and do NOT carry these replay keys: 20261029T180412Z-scn-refusal-messages-are-emulator-authored-prose-instead.md (not-found / validation message wording), 20261104T203648Z-scn-deleted-data-lake-resources-stay-readable-getdatalakedataset.md (deleted data-lake resources still readable) and 20261114T215910Z-scn-data-integration-plane-serves-hollow-flow-and.md (data-integration flow/event projections). Both causes here are in the instance plane's domain projection.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator (emulator_up true, bootstrap_signal false, score 56.4%, 10 divergence clusters).

Cause 1 — fixture supplychain-bill-of-materials-import-job, step setup#1, operation CreateInstance, service scn, verdict mismatch, expected_status 200 / actual_status 200:
  $.instance.primaryDomain: host_not_own (expected '<shaped-not-asserted>', got '86668e01-6585-4c9b-8343-157437069ca7.scn.global.on.aws')
  $.instance.idcPortalDomain: host_not_own (expected '<shaped-not-asserted>', got '86668e01-6585-4c9b-8343-157437069ca7.scn.global.on.aws')
Expected: both members name a host this endpoint actually serves (shape-only assertion). Actual: a name under the fixed public zone scn.global.on.aws, which the emulator does not serve.

Cause 2 — fixture supplychain-datalake-namespace-dataset, step setup#1, operation CreateInstance, service scn, verdict mismatch, expected_status 200 / actual_status 200 (affected_count 5):
  $.instance.webAppDnsDomain: value_mismatch (expected '<shaped-not-asserted>', got '963098ec-f9be-4bcb-aab1-85ddb60b0433')
  $.instance.primaryDomain: host_not_own (expected '<shaped-not-asserted>', got '963098ec-f9be-4bcb-aab1-85ddb60b0433.scn.global.on.aws')
  $.instance.idcPortalDomain: host_not_own (expected '<shaped-not-asserted>', got '963098ec-f9be-4bcb-aab1-85ddb60b0433.scn.global.on.aws')
Expected: a generated DNS label matching InstanceWebAppDnsDomain's @pattern ^(?![-])[a-zA-Z0-9-]{1,62}[a-zA-Z0-9]$ and distinct from the instanceId, with the hostnames built from it addressing the serving endpoint. Actual: the instanceId reused as the label, and the hostnames built from it under scn.global.on.aws.

Model evidence (services/service-com-amazonaws-supplychain-galaxypublicapigateway/smithy-model/com-amazonaws-supplychain-galaxypublicapigateway.json): CreateInstance POST /api/instance, success 200, output member `instance` (Instance) required; Instance members are instanceId/awsAccountId/state/errorMessage/webAppDnsDomain/createdTime/lastModifiedTime/instanceName/instanceDescription/kmsKeyArn/versionNumber — primaryDomain and idcPortalDomain are unmodeled (recorded behavior only). Instance$webAppDnsDomain doc: "The DNS subdomain of the web app. This would be \"example\" in the URL \"example.scn.global.on.aws\"."

The recorded values quoted above (86668e01-…, 963098ec-…) are per-run instance identifiers and are NOT part of the requirement; the requirement is that the announced host is one the emulator serves and that the domain label is an independently generated label.

## Steps to Reproduce
1. git clone the project and boot the emulator built from the checkout.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Cause 1: replay fixture `supplychain-bill-of-materials-import-job`, step `setup#1` — CreateInstance answers 200 and $.instance.primaryDomain / $.instance.idcPortalDomain carry `<label>.scn.global.on.aws`, a host the emulator does not serve (host_not_own).
4. Cause 2: replay fixture `supplychain-datalake-namespace-dataset`, step `setup#1` — POST /api/instance WITHOUT a webAppDnsDomain member; the 200 response's $.instance.webAppDnsDomain equals the returned $.instance.instanceId instead of a generated DNS label, and the two derived domains repeat it under scn.global.on.aws. Same divergence replays from supplychain-instance-lifecycle#task#1, supplychain-list-instances-pagination#setup#1, supplychain-list-instances-pagination#setup#2 and supplychain-tagging-workflow#teardown-side setup step supplychain-tagging-workflow#setup#1.
5. A later GetInstance on the same instance reports the same values, since every instance operation answers from the same projection.
