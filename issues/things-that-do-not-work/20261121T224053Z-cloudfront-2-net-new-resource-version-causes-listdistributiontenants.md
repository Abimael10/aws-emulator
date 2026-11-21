Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-distribution-tenant-lifecycle
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-distribution-tenant-lifecycle cloudfront-trust-store-delete-not-found cloudfront-trust-store-error-paths cloudfront-trust-store-lifecycle cloudfront-trust-store-lookup-and-missing-source-error
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|ListDistributionTenants|2xx->2xx|$.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag
Issue-Key: cloudfront|DeleteTrustStore|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: cloudfront|DeleteTrustStore|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|DeleteTrustStore|4xx->4xx|status
Issue-Commit: e9f854af1390e4ca07b6c48ce548492d5c4ae9ff

# cloudfront: 2 net-new resource-version causes — ListDistributionTenants reports a non-current tenant ETag and DeleteTrustStore answers EntityNotFound instead of InvalidIfMatchVersion

> _Note: this issue was identified against the code base at commit `e9f854af1390e4ca07b6c48ce548492d5c4ae9ff`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDistributionTenants

## Description
Two net-new causes in the CloudFront emulator's handling of a resource's version (its ETag / the If-Match version derived from it). Both are wire divergences replayed from simparity-wire-aws-v1 against this checkout; neither is owned by an open issue (the replay reports an empty owned_by for both clusters).

Cause 1 of 2 — ListDistributionTenants publishes an ETag that is not the tenant's current version (fixture cloudfront-distribution-tenant-lifecycle, step task#4, operation ListDistributionTenants).
AWS: Smithy (com.amazonaws.cloudfront#Cloudfront2020_05_31) ListDistributionTenants -> POST /2020-05-31/distribution-tenants, 200, output ListDistributionTenantsResult.DistributionTenantList of DistributionTenantSummary; DistributionTenantSummary.ETag is @required and documented as "The current version of the distribution tenant." So the ETag a tenant summary carries in a listing is the SAME version token that CreateDistributionTenant / GetDistributionTenant / UpdateDistributionTenant return for that tenant in their ETag header or body at that moment — a client may use it directly as the If-Match of the next mutation. The concrete ETag strings in any one run are generated per run and are not themselves the requirement; the requirement is the equality between the listed summary's ETag and that tenant's current version.
Emulator: the listing answers 200 with a well-formed summary, but its ETag member is a value unrelated to the version the same run's tenant read reported for that tenant: the replay recorded $.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag as 'E1MJV490Q2CKJ1' where the tenant's own current version in that run was 'EJZ50Z92T9DP30'. Hypothesis: the list projection mints or caches a version token of its own instead of reading the stored tenant's version.
Why: a client that lists tenants and feeds the listed ETag into UpdateDistributionTenant/DeleteDistributionTenant (the documented purpose of the member) sends a version the service does not consider current, so against AWS the mutation succeeds and against the emulator it is refused as a precondition failure — and any reconciliation loop that compares listed versions to detect drift sees spurious changes on every poll.
Fix: the ListDistributionTenants handler's summary projection (and the shared tenant-summary serializer it uses) must read the ETag from the stored distribution-tenant's current version, the same source GetDistributionTenant/CreateDistributionTenant serve.
Traces: cloudfront-distribution-tenant-lifecycle#task#4.

Cause 2 of 2 — DeleteTrustStore resolves the trust store before validating the If-Match version, so a missing/invalid version is reported as a non-existent entity (fixture cloudfront-trust-store-delete-not-found, step task#1, operation DeleteTrustStore).
AWS: Smithy models DeleteTrustStore -> DELETE /2020-05-31/trust-store/{Id}, success code 204, with Id (@required, @httpLabel, length 1..64) and IfMatch (@required, @httpHeader If-Match). Its modeled errors include both InvalidIfMatchVersion (httpError 400, documented "The If-Match version is missing or not valid.") and EntityNotFound (httpError 404). When the If-Match version is missing or is not a valid version token, AWS answers HTTP 400 with Error.Type 'Sender', Error.Code 'InvalidIfMatchVersion' and the message 'The If-Match version is missing or not valid for the resource.' — that sentence is entirely fixed text for this error class, with no id, name, ETag or timestamp interpolated — regardless of whether the Id in the path resolves to an existing trust store. In other words the version check precedes resource resolution.
Emulator: it resolves the path Id first, so the request is refused with HTTP 404, Error.Code 'EntityNotFound' and the message 'The specified trust store does not exist.' instead of 400 / 'InvalidIfMatchVersion' / 'The If-Match version is missing or not valid for the resource.'. The replay shows this on four fixtures (delete-not-found, error-paths, lifecycle, lookup-and-missing-source-error), all with the identical divergence key service=cloudfront|DeleteTrustStore|status=400->404.
Why: a client distinguishes these two classes to decide what to do next — InvalidIfMatchVersion means "re-read the resource and retry with its current ETag" (a 400 Sender fault in the request), while EntityNotFound means "the resource is gone, stop" (and is commonly treated as a successful idempotent delete). The emulator inverts that: a caller with a bad If-Match concludes the trust store was already deleted and skips the retry, and teardown logic that swallows 404s silently leaves the trust store in place.
Fix: the DeleteTrustStore handler's validation order — validate the required If-Match header (presence and version well-formedness) and raise the modeled InvalidIfMatchVersion with AWS's fixed sentence, before looking the Id up in trust-store state and before raising EntityNotFound. Note the open issue 20261121T221736Z-cloudfront-s3-the-trust-store-stale-if-match.md covers a different leg of the same family (a STALE but well-formed If-Match answering 412 PreconditionFailed with a non-AWS sentence) and does not carry these replay keys, so it does not cover this cause; 20261029T212223Z-cloudfront-if-match-etag-handling-wrong-precondition-message.md likewise covers precondition message text, not this validation-order/status-class fault.
Traces: cloudfront-trust-store-delete-not-found#task#1, cloudfront-trust-store-error-paths#task#3, cloudfront-trust-store-lifecycle#task#4, cloudfront-trust-store-lookup-and-missing-source-error#task#3.

## Evidence
Replay of simparity-wire-aws-v1 against this checkout (emulator_up=true, 1411 steps, 1315 passed, score 93.2%). Both clusters report stub_path="" and owned_by=[] — i.e. net-new and not owned by any open issue.

[mismatch] cloudfront | ListDistributionTenants | fixture cloudfront-distribution-tenant-lifecycle step task#4
  divergence_key: service=cloudfront|ListDistributionTenants|status=200->200|keys=$.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag
  expected status 200, actual status 200
  $.ListDistributionTenantsResult.DistributionTenantList.DistributionTenantSummary.ETag: value_mismatch (expected 'EJZ50Z92T9DP30', got 'E1MJV490Q2CKJ1')
  Requirement: the summary's @required ETag equals that tenant's current version as returned by Create/Get/UpdateDistributionTenant in the same run. The two literal tokens above are per-run generated values and are not part of the requirement.

[mismatch] cloudfront | DeleteTrustStore | fixture cloudfront-trust-store-delete-not-found step task#1 (+3 affected: cloudfront-trust-store-error-paths#task#3, cloudfront-trust-store-lifecycle#task#4, cloudfront-trust-store-lookup-and-missing-source-error#task#3)
  divergence_key: service=cloudfront|DeleteTrustStore|status=400->404|keys=$.ErrorResponse.Error.Code,$.ErrorResponse.Error.Message,status
  status: expected 400, got 404
  $.ErrorResponse.Error.Code: value_mismatch (expected 'InvalidIfMatchVersion', got 'EntityNotFound')
  $.ErrorResponse.Error.Message: value_mismatch (expected 'The If-Match version is missing or not valid for the resource.', got 'The specified trust store does not exist.')
  Expected (AWS): HTTP 400, Error.Type 'Sender', Error.Code 'InvalidIfMatchVersion', Error.Message exactly 'The If-Match version is missing or not valid for the resource.' (all fixed text; the RequestId is per-run and not asserted).
  Actual (emulator): HTTP 404, Error.Code 'EntityNotFound', Error.Message 'The specified trust store does not exist.'

## Steps to Reproduce
Cause 1 (ListDistributionTenants ETag):
1. Boot the emulator from this checkout.
2. CreateDistributionTenant against a multi-tenant distribution; note the ETag returned for the new tenant (call it E1).
3. GetDistributionTenant on that tenant and confirm it reports E1 as its current version.
4. ListDistributionTenants (POST /2020-05-31/distribution-tenants) and read the DistributionTenantSummary for that tenant: its ETag is some other token, not E1.
5. Re-replay fixture cloudfront-distribution-tenant-lifecycle step task#4 to confirm a fix.

Cause 2 (DeleteTrustStore validation order):
1. Boot the emulator from this checkout.
2. Send DeleteTrustStore (DELETE /2020-05-31/trust-store/{Id}) for a well-formed trust-store Id that does not exist, with the If-Match header missing or carrying a version token that is not valid.
3. Observe HTTP 404 / EntityNotFound / 'The specified trust store does not exist.' instead of HTTP 400 / InvalidIfMatchVersion / 'The If-Match version is missing or not valid for the resource.'.
4. Re-replay fixtures cloudfront-trust-store-delete-not-found#task#1, cloudfront-trust-store-error-paths#task#3, cloudfront-trust-store-lifecycle#task#4 and cloudfront-trust-store-lookup-and-missing-source-error#task#3 to confirm a fix.
