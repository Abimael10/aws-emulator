Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__s3.continuous-deployment-staging-origin-path-promotion-serves-staging-bytes cloudfront__s3.cache-tag-invalidation-of-an-unmatched-tag-evicts-nothing cloudfront__s3.cache-tag-invalidation-refreshes-the-tagged-object-and-spares-the-untagged-one cloudfront__s3.cf-s3-large-object-first-fetch-miss-second-fetch-hit cloudfront__s3.cf-s3-response-headers-match-origin-and-via-form cloudfront__s3.cf-viewer-get-with-body-rejected cloudfront__s3.delete-continuous-deployment-policy-while-attached-to-primary-rejected cloudfront__s3.distribution-origin-access-control-type-mismatch-rejected cloudfront__s3.trust-store-stale-etag-versioning
Issue-Key: cloudfront__s3.continuous-deployment-staging-origin-path-promotion-serves-staging-bytes|_poll_until#40|AssertionError|raise AssertionError(f"timed out waiting for {description} after {timeout_s}s (last: {last!r})")
Issue-Key: cloudfront__s3.cache-tag-invalidation-of-an-unmatched-tag-evicts-nothing|_get_until#13|Timeout|raise AssertionError(f"{description} not observed within {max_attempts} attempts; last {last!r}")
Issue-Key: cloudfront__s3.cache-tag-invalidation-refreshes-the-tagged-object-and-spares-the-untagged-one|_get_until#13|Timeout|raise AssertionError(f"{description} not observed within {max_attempts} attempts; last {last!r}")
Issue-Key: cloudfront__s3.cf-s3-large-object-first-fetch-miss-second-fetch-hit|_poll#29|Timeout|time.sleep(interval_s)
Issue-Key: cloudfront__s3.cf-s3-response-headers-match-origin-and-via-form|run#32|AssertionError|assert via is not None and _VIA_RE.match(via), (
Issue-Key: cloudfront__s3.cf-viewer-get-with-body-rejected|run#25|AssertionError|assert refused.status_code == 403, (
Issue-Key: cloudfront__s3.delete-continuous-deployment-policy-while-attached-to-primary-rejected|run#28|AssertionError|assert got == _IN_USE, f"expected {_IN_USE!r} deleting an attached policy, got {got!r}"
Issue-Key: cloudfront__s3.distribution-origin-access-control-type-mismatch-rejected|_error#11|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: cloudfront__s3|UpdateTrustStore|NotImplemented
Issue-Commit: 898fb31eb69e07df3cb7a6861bbbd7f63697428c

# cloudfront__s3: 6 net-new causes — a distribution never leaves InProgress, the edge omits the Via header, a viewer GET with a body is served, the ContinuousDeploymentPolicyInUse message is rewritten, an OAC type mismatch is accepted, and UpdateTrustStore answers NotImplemented

> _Note: this issue was identified against the code base at commit `898fb31eb69e07df3cb7a6861bbbd7f63697428c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Six distinct root causes in the CloudFront distribution lifecycle, the edge response path, and two control-plane refusals.

Cause 1 of 6: a created distribution never transitions out of Status 'InProgress', so every check that waits for it to deploy before fetching through the edge stalls to its budget.
AWS: Smithy (cloudfront) models Distribution.Status; AWS mints a distribution InProgress and transitions it to 'Deployed', which is the signal the SDK's distribution_deployed waiter and the documented workflow both key on. Recorded behavior shows the checks polling for exactly that.
Emulator: 'AssertionError: timed out waiting for distribution <id> to reach Deployed after 14.70s (last: 'InProgress')' and, in the siblings that wait longer, bare 'Check timed out after 60.0s'. The status never advances.
Why: every client that creates or updates a distribution and waits for deployment before sending traffic (the only safe sequence) blocks forever; cache-tag invalidation, large-object caching and continuous-deployment promotion can never be observed.
Fix: the CloudFront distribution lifecycle/state machine that must advance a created or updated distribution from InProgress to Deployed (and back to Deployed after an update).
Traces: cloudfront__s3.continuous-deployment-staging-origin-path-promotion-serves-staging-bytes, cloudfront__s3.cache-tag-invalidation-of-an-unmatched-tag-evicts-nothing, cloudfront__s3.cache-tag-invalidation-refreshes-the-tagged-object-and-spares-the-untagged-one, cloudfront__s3.cf-s3-large-object-first-fetch-miss-second-fetch-hit.

Cause 2 of 6: the edge response carries no Via header.
AWS: recorded behavior only; a CloudFront viewer response always carries a Via header of the documented form '<http-version> <alphanumeric>.cloudfront.net (CloudFront)' — the fixed parts are the ' (CloudFront)' suffix and the '.cloudfront.net' host suffix; the HTTP version and the per-POP label are values of the serving environment, not literals.
Emulator: 'AssertionError: Via header None does not match the documented form' — the header is absent.
Why: clients and proxies that detect CloudFront by its Via header (cache diagnostics, logging, bot/CDN detection) see an origin-looking response.
Fix: the CloudFront edge response serializer (it must emit Via on every viewer response).
Traces: cloudfront__s3.cf-s3-response-headers-match-origin-and-via-form.

Cause 3 of 6: a viewer GET carrying a request body is served 200 instead of being refused 403.
AWS: recorded behavior only; CloudFront refuses a GET/HEAD viewer request that carries a body with HTTP 403.
Emulator: 'AssertionError: a GET request with a body answered 200, expected 403'.
Why: a malformed/smuggled request that AWS blocks at the edge is passed through to the origin, so a client's edge-level request hygiene does not hold.
Fix: the CloudFront edge viewer-request validation.
Traces: cloudfront__s3.cf-viewer-get-with-body-rejected.

Cause 4 of 6: DeleteContinuousDeploymentPolicy on an attached policy returns the right error code with a rewritten message.
AWS: Smithy (cloudfront) models the error ContinuousDeploymentPolicyInUse; its message is the fixed sentence 'The specified continuous deployment policy is currently associated with a distribution.' — no variable parts.
Emulator: the code is right but the message is a different, emulator-authored sentence: 'You cannot delete a continuous deployment policy that is associated with a primary distribution. First update your distribution to remove the continuous deployment policy, then you can delete the policy.'
Why: clients and tests that match on the error message (and operators reading it) see text AWS never emits.
Fix: the DeleteContinuousDeploymentPolicy handler's error construction.
Traces: cloudfront__s3.delete-continuous-deployment-policy-while-attached-to-primary-rejected.

Cause 5 of 6: CreateDistribution accepts an OriginAccessControl whose OriginAccessControlOriginType does not match the origin it is attached to.
AWS: Smithy (cloudfront) models OriginAccessControlConfig.OriginAccessControlOriginType as an enum (s3, mediastore, lambda, mediapackagev2); AWS refuses a distribution that attaches an OAC of one origin type to an origin of another, so the call must be an error, not a 201.
Emulator: 'AssertionError: expected the call to be refused, but it succeeded' with HTTP 201 and a full Distribution body — in the recording an OAC of origin type 's3' is attached to a CustomOriginConfig origin and accepted.
Why: a misconfiguration that AWS catches at create time is persisted; the client then has a distribution whose origin requests are unsigned and will fail at the edge with no control-plane signal.
Fix: CreateDistribution's (and UpdateDistribution's) validation of OriginAccessControlId against the origin's configured type.
Traces: cloudfront__s3.distribution-origin-access-control-type-mismatch-rejected.

Cause 6 of 6: UpdateTrustStore answers NotImplemented (HTTP 501).
AWS: Smithy (cloudfront) binds UpdateTrustStore; it must update a trust store and return the new ETag so a stale-ETag update is refused with a precondition failure.
Emulator: 'botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the UpdateTrustStore operation: operation UpdateTrustStore is scaffolded but not implemented in this emulator'. This check is NOT marked ALREADY FILED in the suite report, so no stub issue carries it.
Why: mutual-TLS trust-store versioning cannot be exercised; a client that rotates a CA bundle gets 501 for a modeled operation.
Fix: the UpdateTrustStore handler and the trust-store ETag/version state behind it.
Traces: cloudfront__s3.trust-store-stale-etag-versioning.

## Evidence
Cause 1 — Expected: GetDistribution Status becomes 'Deployed'. Actual: 'timed out waiting for distribution EJZ50Z92T9DP30 to reach Deployed after 14.702488758000072s (last: 'InProgress')'; siblings end with 'Check timed out after 60.0s'.
Cause 2 — Expected: Via matching '<http-version> <label>.cloudfront.net (CloudFront)'. Actual: 'Via header None'.
Cause 3 — Expected: HTTP 403. Actual: HTTP 200.
Cause 4 — Expected ('ContinuousDeploymentPolicyInUse', 'The specified continuous deployment policy is currently associated with a distribution.'). Actual ('ContinuousDeploymentPolicyInUse', 'You cannot delete a continuous deployment policy that is associated with a primary distribution. First update your distribution to remove the continuous deployment policy, then you can delete the policy.').
Cause 5 — Expected: a refusal. Actual: HTTP 201 with a Distribution whose origin uses CustomOriginConfig while OriginAccessControlId names an 's3'-type OAC.
Cause 6 — Expected: UpdateTrustStore succeeds and a stale-ETag retry is refused. Actual: '(NotImplemented) ... operation UpdateTrustStore is scaffolded but not implemented in this emulator' (501).

## Steps to Reproduce
Cause 1: CreateDistribution over an S3 origin, then poll GetDistribution for Status == 'Deployed'.
Cause 2: fetch any object through a distribution and read the response's Via header.
Cause 3: send a GET to the distribution's DomainName with a non-empty request body.
Cause 4: CreateContinuousDeploymentPolicy, attach it to a primary distribution, then DeleteContinuousDeploymentPolicy and read the error code and message.
Cause 5: CreateOriginAccessControl with OriginAccessControlOriginType 's3', then CreateDistribution attaching that OAC id to an origin configured with CustomOriginConfig.
Cause 6: CreateTrustStore, then UpdateTrustStore (and repeat with a stale ETag).
