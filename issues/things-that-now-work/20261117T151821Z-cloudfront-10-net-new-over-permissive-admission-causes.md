Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.create-distribution-nosuch-response-headers-policy-rejected cloudfront.distribution-origin-count-quota-boundary-rejected cloudfront.create-distribution-rejects-cache-tag-header-with-illegal-characters cloudfront.distribution-origin-custom-header-reserved-name-rejected cloudfront.distribution-oversized-comment-without-cache-policy-rejected cloudfront.distribution-tenant-required-default-parameter-schema-rejected cloudfront.origin-path-more-than-two-parameters-rejected cloudfront.tenant-config-on-direct-distribution-rejected cloudfront.create-and-update-distribution-tenant-invalid-web-acl-rejected cloudfront.tag-resource-rejects-reserved-aws-prefix-key
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.create-distribution-nosuch-response-headers-policy-rejected|_error#5|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: cloudfront.distribution-origin-count-quota-boundary-rejected|_error#3|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded and returned {result!r}")
Issue-Key: cloudfront.create-distribution-rejects-cache-tag-header-with-illegal-characters|expect_error#3|AssertionError|raise AssertionError(f"expected {code!r}, but the call succeeded: {result!r}")
Issue-Key: cloudfront.distribution-origin-custom-header-reserved-name-rejected|_expect_error#10|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: cloudfront.distribution-oversized-comment-without-cache-policy-rejected|_error#9|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: cloudfront.distribution-tenant-required-default-parameter-schema-rejected|_expect_error#6|AssertionError|raise AssertionError(
Issue-Key: cloudfront.origin-path-more-than-two-parameters-rejected|_expect_error#8|AssertionError|raise AssertionError(f"expected {description} to raise, but it succeeded with {result!r}")
Issue-Key: cloudfront.tenant-config-on-direct-distribution-rejected|_error#9|AssertionError|raise AssertionError("the call succeeded; real AWS rejects it, so there is no error code")
Issue-Key: cloudfront.create-and-update-distribution-tenant-invalid-web-acl-rejected|_expect_error#7|AssertionError|raise AssertionError(f"expected the call to be refused with {expected_code!r}, but it succeeded with {result!r}")
Issue-Key: cloudfront.tag-resource-rejects-reserved-aws-prefix-key|_error#15|AssertionError|raise AssertionError(f"expected the call to be refused, it succeeded with {result!r}")
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# cloudfront: 10 net-new over-permissive admission causes — CreateDistribution / UpdateDistribution / CreateDistributionTenant / TagResource accept configurations AWS refuses

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Ten net-new root causes, all in the CloudFront control plane's admission layer: the emulator answers 2xx and persists the resource for requests real AWS refuses with a modeled 4xx. Each cause is an independent missing check; all ten must be closed before submitting. The open issue 20261029T175823Z-cloudfront-10-more-divergences-the-edge-data-plane.md covers a different set of checks and does not cover any of these.

Cause 1 of 10: a cache behavior naming a ResponseHeadersPolicyId that does not exist is accepted.
AWS: Smithy: CreateDistribution/CreateDistributionWithTags bind NoSuchResponseHeadersPolicy (httpError 404). Recorded refusal: 404, Code `NoSuchResponseHeadersPolicy`, Message "The specified response headers policy does not exist." — entirely fixed text, no variable parts — and no distribution is created.
Emulator: answers 201 with a Distribution whose DefaultCacheBehavior keeps the bogus ResponseHeadersPolicyId.
Why: a client that mistypes or races a policy id believes the attachment succeeded; the distribution now references a policy that can never be resolved, and the caller's teardown/quota accounting diverges from AWS.
Fix: CreateDistribution/CreateDistributionWithTags validation — resolve every cache behavior's ResponseHeadersPolicyId before admitting the config.
Traces: cloudfront.create-distribution-nosuch-response-headers-policy-rejected.

Cause 2 of 10: more Origins than the per-distribution limit are accepted.
AWS: Smithy: CreateDistribution binds TooManyOrigins (httpError 400). Recorded refusal at one origin over the limit: 400, Code `TooManyOrigins`, Message "Your request contains more Origins than are allowed per distribution." (fixed text); a config at the limit is created and reports every origin.
Emulator: answers 201 for a config carrying one more origin than the limit (Origins.Quantity over the boundary is stored verbatim).
Why: the boundary a client probes to discover its quota never appears, so quota-aware provisioning code sees no ceiling at all.
Fix: CreateDistribution's validation — enforce the per-distribution Origins quota and raise TooManyOrigins.
Traces: cloudfront.distribution-origin-count-quota-boundary-rejected.

Cause 3 of 10: CacheTagConfig.HeaderName containing illegal header characters is accepted.
AWS: Smithy: CacheTagConfig.HeaderName is a required string; CreateDistribution binds InvalidArgument (400). Recorded refusal: 400, Code `InvalidArgument`, Message "The parameter CacheTagConfig:headerName contains <headerName> that has illegal characters." — the fixed text is "The parameter CacheTagConfig:headerName contains " ... " that has illegal characters." and the variable part is the submitted header name, taken from the request. A well-formed header token on the same config is accepted and round-trips.
Emulator: answers 201 and stores the malformed CacheTagConfig.
Why: cache-tag invalidation silently keys off a header the origin can never legally send, so tag invalidation never matches anything.
Fix: CreateDistribution's validation of CacheTagConfig.HeaderName (HTTP token charset).
Traces: cloudfront.create-distribution-rejects-cache-tag-header-with-illegal-characters.

Cause 4 of 10: an origin CustomHeaders entry naming a header CloudFront reserves is accepted.
AWS: Smithy: OriginCustomHeader.HeaderName is required; UpdateDistribution binds InvalidArgument (400). Recorded refusal: 400, Code `InvalidArgument`, Message "The parameter HeaderName : Host is not allowed." — the fixed text is "The parameter HeaderName : " ... " is not allowed." with the reserved header name from the request; the stored CustomHeaders stay unchanged, and a legal header name on the same origin still succeeds.
Emulator: answers 200 and stores CustomHeaders {HeaderName: Host}.
Why: the viewer Host header is silently overridden at the origin fetch, which changes origin routing in a way AWS forbids by contract.
Fix: UpdateDistribution/CreateDistribution origin validation — reject reserved custom header names.
Traces: cloudfront.distribution-origin-custom-header-reserved-name-rejected.

Cause 5 of 10: a Comment longer than the documented maximum is accepted.
AWS: Smithy: DistributionConfig.Comment is a string documented as at most 128 characters; CreateDistribution binds InvalidArgument (400). Recorded refusal for a 129-character Comment: HTTP 400, Code `InvalidArgument`, Message "The parameter Comment is too big." (fixed text), and no distribution is left behind; a 128-character Comment is accepted.
Emulator: answers 201 for the 129-character Comment.
Why: a client that relies on AWS to bound the field writes a config AWS would reject, and the length boundary is unobservable.
Fix: CreateDistribution's validation of DistributionConfig.Comment length.
Traces: cloudfront.distribution-oversized-comment-without-cache-policy-rejected.

Cause 6 of 10: an invalid multi-tenant parameter-definition schema is accepted.
AWS: Smithy: CreateDistribution binds InvalidArgument (400); TenantConfig carries the parameter definitions. Recorded refusal: 400, Code `InvalidArgument`, Message "One or more parameter definition schemas aren't valid. Review the parameters for the multi-tenant distribution and try again." — entirely fixed text.
Emulator: answers 201 and creates the multi-tenant distribution with the invalid schema.
Why: every later tenant create against that distribution is validated against a schema AWS would never have stored, so tenant admission diverges from AWS from then on.
Fix: CreateDistribution's TenantConfig parameter-definition validation.
Traces: cloudfront.distribution-tenant-required-default-parameter-schema-rejected.

Cause 7 of 10: an OriginPath referencing more tenant parameters than AWS allows per field is accepted.
AWS: Smithy: CreateDistribution binds InvalidArgument (400). Recorded refusal for an OriginPath referencing three declared parameters: 400, Code `InvalidArgument`, Message "Your request exceeds the maximum number of parameters per field. Specify fewer parameters and try again." (fixed text), nothing created; the same origin referencing two parameters is accepted and its OriginPath round-trips unchanged.
Emulator: answers 201 and stores the three-parameter OriginPath.
Why: the per-field parameter ceiling is invisible, so tenant templates that AWS rejects are provisioned locally and fail only in production.
Fix: CreateDistribution's origin validation — count parameter references per field against the documented maximum.
Traces: cloudfront.origin-path-more-than-two-parameters-rejected.

Cause 8 of 10: TenantConfig on a non-multi-tenant distribution is accepted.
AWS: Smithy: CreateDistribution binds InvalidArgument (400). Recorded refusal, both with no ConnectionMode and with ConnectionMode `direct`: 400, Code `InvalidArgument`, Message "The parameter TenantConfig cannot be set for a direct distribution." (fixed text); the same TenantConfig block is accepted on a tenant-only distribution.
Emulator: accepts the call, so there is no error code at all.
Why: the distribution is left in a state AWS cannot represent (a direct distribution with tenant parameters), and the client never learns ConnectionMode must be tenant-only.
Fix: CreateDistribution's validation — gate TenantConfig on ConnectionMode.
Traces: cloudfront.tenant-config-on-direct-distribution-rejected.

Cause 9 of 10: a Customizations.WebAcl override naming a web ACL that does not exist is accepted on tenant create and update.
AWS: Smithy: CreateDistributionTenant/UpdateDistributionTenant bind InvalidArgument (400) and carry Customizations. Recorded refusal for a nonexistent CLOUDFRONT-scope web ACL ARN (and for a REGIONAL-scope one): 400, Code `InvalidArgument`, Message "The specified web ACL isn't valid. Specify a web ACL that was previously validated and try again." — entirely fixed text; no tenant is created and an existing tenant keeps its prior customization.
Emulator: answers 201 (create) / 200 (AssociateDistributionTenantWebACL-style update) and stores the unresolvable WebAcl ARN.
Why: the tenant reports a WAF association that protects nothing; a client verifying its security posture by reading the tenant back is told it is protected.
Fix: CreateDistributionTenant/UpdateDistributionTenant validation — resolve Customizations.WebAcl.Arn and its scope.
Traces: cloudfront.create-and-update-distribution-tenant-invalid-web-acl-rejected.

Cause 10 of 10: TagResource accepts a tag key in the reserved "aws:" namespace.
AWS: Smithy: TagResource binds InvalidTagging (httpError 400). Recorded refusal for key "aws:owner": 400, Code `InvalidTagging`, Message "Tag key cannot start with 'aws:'." — entirely fixed text — and ListTagsForResource shows the resource's tags unchanged.
Emulator: answers 204 and attaches the reserved key.
Why: reserved-namespace tags that AWS reserves for itself become part of the resource's tag set, so tag-based policy and cost allocation read state AWS would never produce.
Fix: TagResource's tag validation (shared with CreateDistributionWithTags/TagResource key checks).
Traces: cloudfront.tag-resource-rejects-reserved-aws-prefix-key.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, 526/702 passed; the ten checks below are listed under `### cloudfront` and are neither ALREADY FILED nor OWNED.

Cause 1 — create-distribution-nosuch-response-headers-policy-rejected: expected (NoSuchResponseHeadersPolicy, 'The specified response headers policy does not exist.'); got HTTP 201 with Location /2020-05-31/distribution/<id> and the bogus ResponseHeadersPolicyId echoed back.
Cause 2 — distribution-origin-count-quota-boundary-rejected: expected (TooManyOrigins, 'Your request contains more Origins than are allowed per distribution.') at limit+1 origins; got HTTP 201 with Origins.Quantity = limit+1.
Cause 3 — create-distribution-rejects-cache-tag-header-with-illegal-characters: expected (InvalidArgument, 'The parameter CacheTagConfig:headerName contains <bad header> that has illegal characters.'); got HTTP 201.
Cause 4 — distribution-origin-custom-header-reserved-name-rejected: expected (InvalidArgument, 'The parameter HeaderName : Host is not allowed.'); got HTTP 200 with CustomHeaders Items [{HeaderName: 'Host', HeaderValue: ...}] stored.
Cause 5 — distribution-oversized-comment-without-cache-policy-rejected: expected 400 (InvalidArgument, 'The parameter Comment is too big.') for a 129-character Comment; got HTTP 201.
Cause 6 — distribution-tenant-required-default-parameter-schema-rejected: expected (InvalidArgument, "One or more parameter definition schemas aren't valid. Review the parameters for the multi-tenant distribution and try again."); got HTTP 201.
Cause 7 — origin-path-more-than-two-parameters-rejected: expected (InvalidArgument, 'Your request exceeds the maximum number of parameters per field. Specify fewer parameters and try again.') for OriginPath '/{{a}}/{{b}}/{{c}}'; got HTTP 201 with that OriginPath stored.
Cause 8 — tenant-config-on-direct-distribution-rejected: expected (InvalidArgument, 'The parameter TenantConfig cannot be set for a direct distribution.'); the call succeeded, so no error code was produced.
Cause 9 — create-and-update-distribution-tenant-invalid-web-acl-rejected: expected (InvalidArgument, "The specified web ACL isn't valid. Specify a web ACL that was previously validated and try again."); got HTTP 201 with Customizations.WebAcl {Action: 'override', Arn: 'arn:aws:wafv2:...:global/webacl/<missing>/...'}.
Cause 10 — tag-resource-rejects-reserved-aws-prefix-key: expected (InvalidTagging, "Tag key cannot start with 'aws:'."); got HTTP 204.

## Steps to Reproduce
1. git clone the repo and let the behavioral suite build and boot the emulator.
2. Replay the simparity-behavioral-aws-v1 suite.
3. Re-run exactly the checks in trace_ids; each one issues the call described in its cause against the emulator's CloudFront endpoint and asserts the modeled refusal.
4. For each cause, confirm the emulator answers the recorded status/code/message and that the refused call leaves no resource behind (the checks assert this with ListDistributions / ListTagsForResource / ListDistributionTenants read-backs).
