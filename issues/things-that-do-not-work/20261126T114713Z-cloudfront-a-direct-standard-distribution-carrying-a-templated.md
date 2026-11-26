Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.direct-distribution-with-tenant-parameters-rejected
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.direct-distribution-with-tenant-parameters-rejected|run#11|AssertionError|assert message == expected, f"the templated origin path refusal says {message!r}, expected {expected!r}"
Issue-Commit: 2aaadc19d9353b323ddc8d1ceaf32931386905fe

# cloudfront: a direct (standard) distribution carrying a templated OriginPath is refused with the TenantConfig message instead of "The parameter originPath contains one or more parameters that are not valid."

> _Note: this issue was identified against the code base at commit `2aaadc19d9353b323ddc8d1ceaf32931386905fe`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
One net-new root cause (Cause 1 of 1), found by the behavioral suite's multi-tenant-distribution rejection check.

Cause 1 of 1: CreateDistribution orders the ConnectionMode/TenantConfig refusal ahead of the tenant-parameter-syntax refusal, so a standard (ConnectionMode="direct") distribution whose origin carries a tenant-parameter template in OriginPath is refused with the wrong message.
Failing check ids: cloudfront.direct-distribution-with-tenant-parameters-rejected

AWS: CreateDistribution refuses a DistributionConfig with ConnectionMode="direct" whose Origins[].OriginPath contains tenant-parameter template syntax (e.g. "/{{TenantPath}}") with HTTP 400, error code InvalidArgument (Smithy: com.amazonaws.cloudfront#InvalidArgument, errorKind=client, httpError 400, member Message), and the message
    "The parameter originPath contains one or more parameters that are not valid."
Every word of that message is FIXED text except the member name it names — "originPath" — which is the config member of the refused request that carries the template; the limit/parameter name is taken from the request, nothing else in it is per-run. The model states why the parameter syntax is invalid here: TenantConfig "only supports multi-tenant distributions. You can't specify this field for standard distributions", and tenant parameters are substituted only for a multi-tenant distribution's tenants — so for a direct distribution the templated OriginPath references parameters that cannot exist. Recorded behavior establishes the PRECEDENCE: of the two things wrong with this request (a TenantConfig on a direct distribution, and a templated originPath on a direct distribution), AWS answers the originPath one. The suite's control proves the request is otherwise well formed: the identical config with ConnectionMode="tenant-only" is accepted and stores OriginPath "/{{TenantPath}}" verbatim, so ConnectionMode is the only difference between the refused request and the accepted one.

Emulator: the status (400) and the error code (InvalidArgument) are right, but the message is
    "The parameter TenantConfig cannot be set for a direct distribution."
It reports the TenantConfig-on-a-direct-distribution rule and never reaches the tenant-parameter check on OriginPath, i.e. the refusal is decided by the wrong rule.

Why: a client that reads Error.Message to decide what to fix — the common pattern for CloudFront's InvalidArgument, which carries no per-member field in the shape and so can only be discriminated by its text — is pointed at the wrong member. It is told to drop TenantConfig, which is the correct member to carry for a multi-tenant distribution, instead of being told that the origin path's parameters are not valid for a standard distribution; a client that strips TenantConfig and retries then gets a second, different refusal from real AWS, so a retry loop keyed on this message does not converge. Tests and tooling that assert the documented refusal text for this class fail against the emulator even though they pass against AWS.

Fix: the shared create-path configuration validation reached from the CreateDistribution handler — `crate::state::config_refusal_without_state(&input.distribution_config)`, called at services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/ops/create_distribution.rs:467-471 (same path serves CreateDistributionWithTags). The tenant-parameter-syntax rule on Origins[].OriginPath must be evaluated for a ConnectionMode="direct" config and must take precedence over the "TenantConfig cannot be set for a direct distribution" rule, and must emit the originPath message quoted above; the refusal must remain InvalidArgument/400 and must leave no distribution behind (the check also asserts the refused create is absent from both the "direct" and "tenant-only" partitions of ListDistributionsByConnectionMode).

Traces: cloudfront.direct-distribution-with-tenant-parameters-rejected.

## Evidence
Check: cloudfront.direct-distribution-with-tenant-parameters-rejected (verdict: mismatch)

Request: CreateDistribution (us-east-1) with DistributionConfig = a tenant-only-shaped config (one S3 origin, OriginPath "/{{TenantPath}}", OriginAccessControlId set, CachePolicyId 658327ea-f89d-4fab-a63d-7e88639e58f6, TenantConfig.ParameterDefinitions = [{Name: "TenantPath", Definition.StringSchema.Required: true}]) but with ConnectionMode overridden to "direct".

Expected (AWS): HTTP 400, Error.Code "InvalidArgument", Error.Message
  "The parameter originPath contains one or more parameters that are not valid."

Actual (emulator): HTTP 400, Error.Code "InvalidArgument" (code matches), Error.Message
  "The parameter TenantConfig cannot be set for a direct distribution."

Assertion from the replay:
  AssertionError: the templated origin path refusal says 'The parameter TenantConfig cannot be set for a direct distribution.', expected 'The parameter originPath contains one or more parameters that are not valid.'
  (check_cloudfront_direct_distribution_with_tenant_parameters_rejected.py line 45)

Control in the same check (passes only after the refusal assertion): the byte-identical config with ConnectionMode="tenant-only" is created successfully and GetDistribution reports Origins.Items[0].OriginPath == "/{{TenantPath}}", so the template itself is accepted for a multi-tenant distribution and ConnectionMode is the sole difference.

Model citations: com.amazonaws.cloudfront#InvalidArgument — errorKind client, httpError 400, single member Message. com.amazonaws.cloudfront#TenantConfig — "This field only supports multi-tenant distributions. You can't specify this field for standard distributions." com.amazonaws.cloudfront#Origin$OriginPath — "An optional path that CloudFront appends to the origin domain name..." (no modeled pattern constraint, so the parameter-syntax rule is behavioral).

## Steps to Reproduce
1. Point a CloudFront client at the emulator in us-east-1 (CloudFront's control plane is global and answers only there).
2. CreateOriginAccessControl with OriginAccessControlOriginType="s3", SigningBehavior="always", SigningProtocol="sigv4"; keep its Id.
3. Call CreateDistribution with a DistributionConfig that has:
   - ConnectionMode = "direct"
   - Enabled = false, HttpVersion = "http2", ViewerCertificate.CloudFrontDefaultCertificate = true
   - Origins.Items[0] = {Id: "S3Origin", DomainName: "<name>.s3.<region>.amazonaws.com", OriginPath: "/{{TenantPath}}", OriginAccessControlId: <from step 2>, S3OriginConfig.OriginAccessIdentity: ""}
   - DefaultCacheBehavior targeting "S3Origin" with CachePolicyId 658327ea-f89d-4fab-a63d-7e88639e58f6
   - TenantConfig.ParameterDefinitions = [{Name: "TenantPath", Definition.StringSchema: {Required: true}}]
4. Observe the ClientError: the code is InvalidArgument (correct) but the message is "The parameter TenantConfig cannot be set for a direct distribution." instead of "The parameter originPath contains one or more parameters that are not valid."
5. Re-run the same create with ConnectionMode = "tenant-only" to confirm the config is otherwise valid: it succeeds and stores OriginPath "/{{TenantPath}}".
6. Re-replay check cloudfront.direct-distribution-with-tenant-parameters-rejected to confirm the fix.
