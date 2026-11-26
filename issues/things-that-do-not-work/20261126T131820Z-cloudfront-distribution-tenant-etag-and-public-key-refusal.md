Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-distribution-tenant-lifecycle
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-distribution-tenant-lifecycle cloudfront-public-key-duplicate-and-notfound-errors cloudfront-public-key-error-paths
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|ListDistributionTenants|2xx->2xx|$.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag
Issue-Key: cloudfront|GetPublicKey|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 94bcd943a3c4e0346859674a6a27263b9bdf544a

# CloudFront distribution-tenant ETag and public-key refusal disagree with the stored resource record

> _Note: this issue was identified against the code base at commit `94bcd943a3c4e0346859674a6a27263b9bdf544a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDistributionTenants

## Description
Two net-new causes, one shared fault: a CloudFront read answers from something other than the resource record the service actually holds -- a list projection publishes a version string that is not the tenant's current version, and a read-by-id refuses an identifier the store should have been allowed to decide on. In both the HTTP status matches AWS exactly and only one value in the answer diverges, so neither is a serialization or routing fault; both are the wrong SOURCE for the value being reported.

Cause 1 of 2: ListDistributionTenants reports a summary ETag that is not the tenant's current version.
AWS: Smithy (cloudfront 2020-05-31) ListDistributionTenants -- http {method POST, uri /2020-05-31/distribution-tenants, code 200}, paginated {inputToken Marker, outputToken NextMarker, items DistributionTenantList, pageSize MaxItems}; output DistributionTenantList is a list of DistributionTenantSummary, whose member ETag is @required and documented as 'The current version of the distribution tenant.' That makes the listed ETag the SAME version string the tenant's own create/get answers with, and the value a caller is expected to pass back as If-Match; it is not a per-listing token the service is free to mint.
Emulator: answers 200 with a well-formed but UNRELATED version string in the summary. In this fixture the tenant's version as established earlier in the same run is 'EJZ50Z92T9DP30' and the list entry reports 'E1MJV490Q2CKJ1'. Both are 14-character 'E'-prefixed version strings, so the FORM is right and the PROVENANCE is wrong: the list projection derives or caches a version of its own rather than reading the tenant record's current version. The replay binds generated identifiers and version strings elsewhere in this corpus (every ETag assertion in the cfetag-* chain fixtures passes), so this is reported as a value_mismatch after that binding -- i.e. the emitted value is not the one the tenant's own record reports, not merely a per-run difference. Hypothesis: the summary is built with a freshly computed version instead of the stored one, which is why it is self-consistently shaped but unbound.
Why: a caller's standard concurrency flow is list -> pick the tenant -> UpdateDistributionTenant/DeleteDistributionTenant with If-Match set to the listed ETag. With a version that belongs to nothing, every such call is refused PreconditionFailed and the loop never converges; a caller that compares the listed ETag with the one GetDistributionTenant returns concludes the tenant changed under it and re-reads forever. The two concrete version strings quoted here are per-run values and are not the requirement -- the requirement is that the listed ETag is the stored tenant's current version.
Fix: ListDistributionTenants' summary projection in the cloudfront operation handler -- populate DistributionTenantSummary.ETag from the stored distribution-tenant record's current version, the same source the tenant's own ETag header is served from, so list and get agree.
Traces: cloudfront-distribution-tenant-lifecycle#task#4.

Cause 2 of 2: GetPublicKey refuses a well-formed-but-absent public-key Id as an unreadable identifier.
AWS: Smithy GetPublicKey -- Id @required @httpLabel on GET /2020-05-31/public-key/{Id}, success 200; modeled errors AccessDenied (httpError 403) and NoSuchPublicKey (httpError 404, 'The specified public key doesn't exist.'). Recorded behavior: an Id that IS a readable CloudFront public-key identifier but names no public key is answered 404 in the REST-XML ErrorResponse envelope with the not-found Code and Message 'The specified CloudFront public key does not exist.' That sentence is entirely FIXED TEXT for this class -- AWS interpolates nothing from the request or from state into it, not even the offending id -- so the sentence itself is the requirement and no recorded id is part of it. The envelope's RequestId is per-run and not asserted.
Emulator: answers 404 with Message 'Id format is invalid' -- the unreadable-identifier sentence -- for a value the store should simply have missed on. The status (404) and the error Code match AWS; only $.ErrorResponse.Error.Message diverges, and it diverges identically on both probes in the cluster, so the decision is made once in the public-key id admission path ahead of the store lookup.
Why: key-rotation and signed-URL setup code reads this message to decide whether to create the key it could not find or to report a bad reference back to the developer. Told its perfectly good id is malformed, it stops instead of creating the missing key, and the genuine malformed-id path becomes untestable because the absent-record path now emits the same body. This is the admission check over-refusing, the exact opposite direction of the fault it was introduced to fix.
Fix: the shared public-key id admission check the four Id-addressed public-key operations go through (per the resolution notes of the resolved 20261125T014453Z-cloudfront-reads-report-a-malformed-identifier-with-the.md this is state::unresolved_public_key). It must refuse only values that could not be an identifier CloudFront issued for the public-key kind -- outside the identifier alphabet, written wider than the issued width, or written exactly as another kind's namespace letter -- and must carry every other value through to the store, where a miss yields NoSuchPublicKey's absent-record sentence. The two branches must stay separate in both directions.
Traces: cloudfront-public-key-duplicate-and-notfound-errors#task#3 (cluster also covers cloudfront-public-key-error-paths#task#2).

Context on cause 2's neighbours, none of which owns it (the replay reports this cluster with owned_by empty): the resolved 20261125T014453Z-cloudfront-reads-report-a-malformed-identifier-with-the.md fixed the REVERSE substitution on GetPublicKey, and a resolved issue covers no live failure; the open 20261118T131652Z-cloudfront-getoriginaccesscontrolconfig-reports-a-well-formed-absent-id.md and 20261125T213329Z-cloudfront-reopens-updateanycastiplist-still-refuses-a-well-formed-but.md report this same over-refusal on other resource kinds (origin access control, anycast IP list) and not on GetPublicKey; the open 20261125T151601Z-cloudfront-field-level-encryption-reads-and-deletes-answer.md owns the four field-level-encryption clusters in this same replay, not this one.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, /__admin/health answered 200, data listener accepts on :36211; 202 fixtures replayed, 1411 steps, 1329 passed, score 94.2%, 31 divergence clusters. Both clusters below are reported with no blocking stub (stub_path empty) and no owning open issue (owned_by empty).

Cause 1 -- fixture cloudfront-distribution-tenant-lifecycle, step task#4, ListDistributionTenants (200 -> 200, 1 affected step):
  expected: $.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag = 'EJZ50Z92T9DP30'
  actual:   $.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag = 'E1MJV490Q2CKJ1'  (verdict value_mismatch)
  divergence_key: service=cloudfront|ListDistributionTenants|status=200->200|keys=$.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag
  Both values are 14-character 'E'-prefixed version strings, so the member's form is already right; what fails is that the listed value is not the tenant's stored current version. Every other ETag assertion in the corpus (the cfetag-* chain fixtures) passes, so generated version strings are bound by the replay and a purely per-run difference would not have been reported here.

Cause 2 -- fixture cloudfront-public-key-duplicate-and-notfound-errors, step task#3, GetPublicKey (404 -> 404, 2 affected steps: cloudfront-public-key-duplicate-and-notfound-errors#task#3, cloudfront-public-key-error-paths#task#2):
  expected: HTTP 404, $.ErrorResponse.Error.Message = 'The specified CloudFront public key does not exist.'
  actual:   HTTP 404, $.ErrorResponse.Error.Message = 'Id format is invalid'  (verdict value_mismatch)
  divergence_key: service=cloudfront|GetPublicKey|status=404->404|keys=$.ErrorResponse.Error.Message
  The status and the error Code already match; only the message diverges.

Model evidence (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json):
  ListDistributionTenants -- POST /2020-05-31/distribution-tenants, code 200; input {AssociationFilter, Marker, MaxItems}; output {NextMarker, DistributionTenantList}; paginated {inputToken Marker, outputToken NextMarker, items DistributionTenantList, pageSize MaxItems}; errors AccessDenied(403), EntityNotFound(404), InvalidArgument(400).
  DistributionTenantSummary.ETag -- target string, @required, doc 'The current version of the distribution tenant.' (siblings Id, DistributionId, Name, Arn, Domains, CreatedTime, LastModifiedTime all @required).
  GetPublicKey -- GET /2020-05-31/public-key/{Id}, code 200; Id @required @httpLabel; output {PublicKey -> httpPayload, ETag -> httpHeader 'ETag'}; errors AccessDenied(403), NoSuchPublicKey(404).
  NoSuchPublicKey -- client error, httpError 404, member Message, doc 'The specified public key doesn't exist.'

Fixed vs variable text: 'The specified CloudFront public key does not exist.' is fixed text in full -- no id, name, value, limit or member name from the request or from emulator state is interpolated into it, so no recorded id is part of the requirement. 'Id format is invalid' is likewise fixed text and is the correct wording only for a value that could not be a public-key identifier at all. For cause 1 the requirement is the ETag's provenance (the stored tenant's current version); the two 14-character strings quoted are per-run generated values and are not themselves requirements.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout; replay the wire corpus (replay_wire_corpus, trace set simparity-wire-aws-v1).
2. Cause 1: replay fixture cloudfront-distribution-tenant-lifecycle and observe step task#4 -- POST /2020-05-31/distribution-tenants after the tenant has been created and read earlier in the fixture. Compare DistributionTenantSummary.ETag in the list entry with the ETag the tenant's own create/get returned for the same tenant id: they differ. Then take the listed ETag and send it as If-Match on UpdateDistributionTenant/DeleteDistributionTenant for that tenant -- it must be accepted, and today it is not the stored version.
3. Cause 2: replay fixture cloudfront-public-key-duplicate-and-notfound-errors and observe step task#3 -- GET /2020-05-31/public-key/{Id} with a readable CloudFront public-key id that names no public key. Expect 404 with Message 'The specified CloudFront public key does not exist.'; the emulator answers 404 with 'Id format is invalid'. Fixture cloudfront-public-key-error-paths#task#2 reproduces it.
4. For cause 2 compare only $.ErrorResponse.Error.Message -- the status (404) and the error Code already match -- and when fixing, re-replay the fixtures graded by the resolved issue 20261125T014453Z (cloudfront-get-public-key-wrong-resource-type-id-rejected, cloudfront-public-key-and-key-group-lifecycle) to confirm the malformed-id branch still answers 'Id format is invalid' while the readable-but-absent branch answers the absent-record sentence.
