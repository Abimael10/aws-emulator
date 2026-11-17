Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__iam__lambda.create-distribution-rejects-an-oac-lambda-type-on-a-non-lambda-custom-origin
Issue-Key: cloudfront__iam__lambda.create-distribution-rejects-an-oac-lambda-type-on-a-non-lambda-custom-origin|expect_error#34|AssertionError|raise AssertionError(f"expected the call to be refused with {code!r}, but it succeeded: {result!r}")
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# CreateDistribution accepts an OriginAccessControl of OriginAccessControlOriginType 'lambda' on a non-Lambda custom origin instead of refusing IllegalOriginAccessConfiguration

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: an origin-access-control whose origin type is `lambda` is accepted when attached to an origin that is not a Lambda function URL.

AWS: an origin access control's `OriginAccessControlConfig.OriginAccessControlOriginType` declares which kind of origin it may sign for (`s3`, `mediastore`, `mediapackagev2`, `lambda`). Attaching an OAC whose type is `lambda` to a custom origin whose domain is not a Lambda function URL is an illegal pairing, and cloudfront CreateDistribution refuses it with the modelled error `IllegalOriginAccessConfiguration` at HTTP 400. (Smithy: cloudfront CreateDistribution errors include `IllegalOriginAccessConfiguration`, errorKind client, httpError 400 — alongside `InvalidOriginAccessControl` and `InvalidDomainNameForOriginAccessControl`.)

Emulator: the call succeeds and mints a distribution. The error code is correct AWS vocabulary for this refusal; the emulator simply never raises it.

Why: a client validating its OAC wiring gets a distribution that AWS would never create. The distribution then signs (or fails to sign) requests to an origin that cannot accept that signature, so the misconfiguration surfaces as an opaque origin error on the data plane instead of a precise, actionable 400 at create time.

Fix: the cloudfront CreateDistribution handler's origin-access-control validation — the cross-check between each origin's kind (S3 vs custom vs Lambda function URL) and the `OriginAccessControlOriginType` of the OAC its `OriginAccessControlId` names, which must raise IllegalOriginAccessConfiguration (400) on a mismatch. UpdateDistribution needs the same check.

Traces: cloudfront__iam__lambda.create-distribution-rejects-an-oac-lambda-type-on-a-non-lambda-custom-origin

## Evidence
Expected: HTTP 400 with error code `IllegalOriginAccessConfiguration` and no distribution created.
Actual, verbatim:
  AssertionError: expected the call to be refused with 'IllegalOriginAccessConfiguration', but it succeeded: 'EJZ50Z92T9DP30'
The returned value is the minted distribution id (a per-run value); the divergence is that CreateDistribution answered success (201) at all instead of the modelled 400.

## Steps to Reproduce
1. cloudfront CreateOriginAccessControl with `OriginAccessControlConfig.OriginAccessControlOriginType: lambda` and SigningBehavior/SigningProtocol set.
2. cloudfront CreateDistribution with a custom origin whose DomainName is NOT a Lambda function URL, and set that origin's `OriginAccessControlId` to the OAC from step 1.
3. Observe: 201 and a distribution id. Expected: HTTP 400 with error code `IllegalOriginAccessConfiguration` and no distribution created.
