Issue-Source: trace-sweep-divergence
Issue-Trace: codeartifact-domain-lifecycle
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codeartifact-domain-lifecycle codeartifact-domain-repository-lifecycle codeartifact-domain-tag-lifecycle codeartifact-get-authorization-token-workflow codeartifact-list-repositories-prefix-and-pagination codeartifact-package-group-error-paths codeartifact-package-group-lifecycle-and-hierarchy codeartifact-repository-error-paths codeartifact-repository-tag-lifecycle
Issue-Shape-Id: com.amazonaws.codeartifact#CodeArtifactControlPlaneService
Issue-Key: codeartifact|CreateDomain|2xx->2xx|$.domain.s3BucketArn
Issue-Key: codeartifact|DescribeDomain|2xx->2xx|$.domain.s3BucketArn
Issue-Key: codeartifact|DeleteDomain|2xx->2xx|$.domain.s3BucketArn
Issue-Commit: 69e9858202f2bcdead1dc90355ba2555eebc8e9a

# CodeArtifact domain s3BucketArn names the wrong asset-bucket account on create, describe and delete

> _Note: this issue was identified against the code base at commit `69e9858202f2bcdead1dc90355ba2555eebc8e9a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomain

## Description
Every CodeArtifact operation that returns a DomainDescription emits an `s3BucketArn` whose account-id component is not the CodeArtifact asset-bucket account for the region. The ARN shape (`arn:aws:s3:::assets-<asset-bucket-account>-<region>`) is right, but the account component is a value of the emulator's own invention, so the field is wrong on every domain in every fixture, on all three operations that return the record. All three causes below are the same defect surfacing through the three code paths that build or echo the domain record; the asset-bucket account is region-scoped and identical in every recording, it is not the caller's account (the corpus's caller account is 000000000000) and it does not vary per domain in either the recording or the emulator.

Cause 1 of 3: CreateDomain returns the wrong s3BucketArn.
  AWS: Smithy: CreateDomain -> CreateDomainResult.domain : DomainDescription; DomainDescription.s3BucketArn documented as "The Amazon Resource Name (ARN) of the Amazon S3 bucket that is used to store package assets in the domain." The model states no pattern for the value, so: recorded behavior only — the recording shows HTTP 200 with `$.domain.s3BucketArn` = `arn:aws:s3:::assets-787052242323-us-west-2`, i.e. `arn:aws:s3:::assets-<asset-bucket-account>-<region>` where the region is the request's region and the asset-bucket account is a service-owned, region-stable value (787052242323 for us-west-2 in this corpus); it is NOT the calling account, which is 000000000000 throughout this corpus.
  Emulator: HTTP 200 with `$.domain.s3BucketArn` = `arn:aws:s3:::assets-000000006729-us-west-2` — correct ARN template and correct region, but a fabricated account component that matches neither the recorded asset-bucket account nor the emulator's own caller account (000000000000). The same wrong value is returned for all nine domains created across the corpus.
  Why: a client that stores, compares or parses the domain's asset-bucket ARN — IAM/bucket-policy generation for the asset bucket, cross-account asset access setup, or any fixture/golden comparison of the domain record — reads an account that does not exist, and policy or assertion built from it never matches what AWS returns.
  Fix: the CreateDomain handler's domain-record construction in the codeartifact service crate, where the `s3BucketArn` is composed: the account component must come from a region-keyed CodeArtifact asset-bucket account constant rather than being derived or generated, and the stored value must be the one later reads echo.
  Traces: codeartifact-domain-lifecycle#task#1 (representative; the cluster also covers codeartifact-domain-repository-lifecycle#task#1, codeartifact-domain-tag-lifecycle#setup#1, codeartifact-get-authorization-token-workflow#setup#1, codeartifact-list-repositories-prefix-and-pagination#setup#1, codeartifact-package-group-error-paths#setup#1, codeartifact-package-group-lifecycle-and-hierarchy#setup#1, codeartifact-repository-error-paths#setup#1, codeartifact-repository-tag-lifecycle#setup#1).

Cause 2 of 3: DescribeDomain echoes the wrong s3BucketArn.
  AWS: Smithy: DescribeDomain -> DescribeDomainResult.domain : DomainDescription, same `s3BucketArn` member. Recorded behavior only — HTTP 200 with `$.domain.s3BucketArn` = `arn:aws:s3:::assets-787052242323-us-west-2`, identical to what CreateDomain returned for the same domain: a read of a domain returns the asset-bucket ARN unchanged.
  Emulator: HTTP 200 with `$.domain.s3BucketArn` = `arn:aws:s3:::assets-000000006729-us-west-2`.
  Why: a client that creates a domain and then re-reads it to discover the asset bucket gets the same wrong account on the read path, so there is no way to recover the correct value.
  Fix: the DescribeDomain handler/serializer in the codeartifact service crate — it must echo the stored domain record, so this cause closes with Cause 1 once the stored value is right; verify it is not recomputing the ARN independently.
  Traces: codeartifact-domain-lifecycle#task#2.

Cause 3 of 3: DeleteDomain's returned domain record carries the wrong s3BucketArn.
  AWS: Smithy: DeleteDomain -> DeleteDomainResult.domain : DomainDescription, same `s3BucketArn` member. Recorded behavior only — HTTP 200, and the deleted domain's record still reports `$.domain.s3BucketArn` = `arn:aws:s3:::assets-787052242323-us-west-2`.
  Emulator: HTTP 200 with `$.domain.s3BucketArn` = `arn:aws:s3:::assets-000000006729-us-west-2`.
  Why: a client that deletes a domain and uses the returned record to clean up or audit the asset bucket is pointed at a non-existent account.
  Fix: the DeleteDomain handler in the codeartifact service crate, where the removed domain record is serialized into the response.
  Traces: codeartifact-domain-lifecycle#teardown#4 (the cluster also covers codeartifact-domain-repository-lifecycle#teardown#10, codeartifact-domain-tag-lifecycle#teardown#7, codeartifact-get-authorization-token-workflow#teardown#4, codeartifact-list-repositories-prefix-and-pagination#teardown#10, codeartifact-package-group-error-paths#teardown#8, codeartifact-package-group-lifecycle-and-hierarchy#teardown#23, codeartifact-repository-error-paths#teardown#9, codeartifact-repository-tag-lifecycle#teardown#9).

No open issue covers this: the open codeartifact issues concern external-connection ingestion (20261113T223143Z, 20261027T083906Z, 20261029T095746Z), PublishPackageVersion digest-mismatch envelopes (20261101T084500Z, 20261103T235912Z, 20261119T162524Z), batch failedVersions errorMessage (20261108T214908Z) and the AssociateExternalConnection 404 message (20261113T232022Z). None of them mentions the domain record's s3BucketArn.

## Evidence
Replay of simparity-wire-aws-v1 (34 fixtures, 344 steps, 312 passed, 32 mismatched).

Cause 1 — CreateDomain, fixture codeartifact-domain-lifecycle step task#1, expected status 200, actual status 200:
  $.domain.s3BucketArn: value_mismatch (expected 'arn:aws:s3:::assets-787052242323-us-west-2', got 'arn:aws:s3:::assets-000000006729-us-west-2')
  divergence_key: service=codeartifact|CreateDomain|status=200->200|keys=$.domain.s3BucketArn; 9 affected steps.

Cause 2 — DescribeDomain, fixture codeartifact-domain-lifecycle step task#2, expected 200, actual 200:
  $.domain.s3BucketArn: value_mismatch (expected 'arn:aws:s3:::assets-787052242323-us-west-2', got 'arn:aws:s3:::assets-000000006729-us-west-2')
  divergence_key: service=codeartifact|DescribeDomain|status=200->200|keys=$.domain.s3BucketArn; 1 affected step.

Cause 3 — DeleteDomain, fixture codeartifact-domain-lifecycle step teardown#4, expected 200, actual 200:
  $.domain.s3BucketArn: value_mismatch (expected 'arn:aws:s3:::assets-787052242323-us-west-2', got 'arn:aws:s3:::assets-000000006729-us-west-2')
  divergence_key: service=codeartifact|DeleteDomain|status=200->200|keys=$.domain.s3BucketArn; 9 affected steps.

Fixed vs variable parts of the value: `arn:aws:s3:::assets-` and the trailing `-<region>` separator are fixed text; `<region>` comes from the request's endpoint region (us-west-2 here) and the emulator already gets that right; `<asset-bucket-account>` is a region-scoped CodeArtifact-owned account, stable across every fixture and every domain in the corpus and independent of the caller account (000000000000 in this corpus, as visible in the recorded repositoryEndpoint host of codeartifact-authorization-token-and-repository-endpoint). The emulator's 000000006729 is neither. Status (200) and response shape are otherwise correct on all three operations.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture codeartifact-domain-lifecycle step task#1 — CreateDomain for a domain in us-west-2 — and read $.domain.s3BucketArn from the 200 response; its account component is the emulator's fabricated value instead of the region's CodeArtifact asset-bucket account.
3. Cause 2: continue to step task#2 of the same fixture — DescribeDomain on that domain — and read $.domain.s3BucketArn; the same wrong account is echoed on the read path.
4. Cause 3: continue to step teardown#4 of the same fixture — DeleteDomain on that domain — and read $.domain.s3BucketArn in the 200 response; the returned record carries the same wrong account.
5. Any of the other eight domain-creating fixtures (e.g. codeartifact-repository-error-paths#setup#1 and its teardown#9) reproduces causes 1 and 3 identically, confirming the value is stable and not per-domain.
