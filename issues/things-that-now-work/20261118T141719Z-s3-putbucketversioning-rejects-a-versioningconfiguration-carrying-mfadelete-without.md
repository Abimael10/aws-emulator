Issue-Source: trace-sweep-divergence
Issue-Trace: s3-bucket-tags-at-create-generic-tagging-versioning-objects-by-version-suspend-and-not-found
Issue-Step: task#20
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: s3-bucket-tags-at-create-generic-tagging-versioning-objects-by-version-suspend-and-not-found
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3|PutBucketVersioning|2xx->4xx|$
Issue-Key: s3|PutBucketVersioning|2xx->4xx|status
Issue-Key: s3|GetBucketVersioning|2xx->2xx|$.VersioningConfiguration.MfaDelete
Issue-Key: s3|GetBucketVersioning|2xx->2xx|$.VersioningConfiguration.Status
Issue-Commit: 35e32963f89bfa2c86c6d7f7351ac2825588f64a

# s3 PutBucketVersioning rejects a VersioningConfiguration carrying MfaDelete without an x-amz-mfa header, so the suspend never lands

> _Note: this issue was identified against the code base at commit `35e32963f89bfa2c86c6d7f7351ac2825588f64a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketVersioning

## Description
Two net-new causes around bucket versioning, found by replaying the wire corpus (trace set simparity-wire-aws-v1): a legitimate PutBucketVersioning is refused because the emulator demands the MFA header for any configuration that mentions MfaDelete, and as a result the bucket's versioning state never changes.

Cause 1 of 2: PutBucketVersioning is refused 400 for a VersioningConfiguration that carries MfaDelete but no x-amz-mfa header.
AWS: the MFA serial/token header is only required when MFA Delete is actually in force for the bucket — a request that sets MfaDelete to Disabled (or leaves MFA Delete off) on a bucket not configured for MFA Delete is accepted and answers 200 with an empty body. Smithy (com.amazonaws.s3#AmazonS3): PutBucketVersioning.MFA is an OPTIONAL member bound to the x-amz-mfa header (only Bucket and VersioningConfiguration are @required), and VersioningConfiguration carries MFADelete (xmlName MfaDelete, enum Enabled|Disabled) alongside Status; the operation models no errors at all. The acceptance itself is recorded behavior: the recording's response is 200 with an empty body.
Emulator: answers 400 with code IllegalVersioningConfigurationException and the message 'VersioningConfiguration.MfaDelete requires the x-amz-mfa request header, which this request does not carry' — an emulator-authored sentence naming the internal member path, for a request AWS accepts.
Why: a client that suspends versioning while explicitly declaring MfaDelete Disabled (what the SDKs and IaC tools emit when the field is set) cannot change versioning at all; it sees a permanent 400 for a well-formed request.
Fix: PutBucketVersioning's MFA gate — require x-amz-mfa only when MFA Delete is being enabled or is already in force on the bucket, and let the configuration through otherwise.
Traces: s3-bucket-tags-at-create-generic-tagging-versioning-objects-by-version-suspend-and-not-found#task#20.

Cause 2 of 2: GetBucketVersioning still reports the old state and never reports MfaDelete.
AWS: after the accepted PutBucketVersioning, GetBucketVersioning (output xmlName VersioningConfiguration) reports Status 'Suspended' and, the bucket now having an MFA Delete setting, MfaDelete 'Disabled'. Smithy: GetBucketVersioningOutput.Status -> BucketVersioningStatus and MFADelete -> MFADeleteStatus serialized as MfaDelete.
Emulator: Status is still 'Enabled' and MfaDelete is absent, and it stays that way under polling (the replay re-read it 30 times) — the bucket's versioning state was never written, because the PUT in cause 1 was refused.
Why: a client that suspends versioning and reads back to confirm sees an enabled bucket, so every later version-scoped expectation (new PutObject writing a 'null' version, no new version ids) diverges; and the MfaDelete element it branches on is never emitted.
Fix: follows cause 1 — the bucket versioning state writer must apply Status and the MfaDelete setting, and the GetBucketVersioning serializer must emit MfaDelete once the bucket has one.
Traces: s3-bucket-tags-at-create-generic-tagging-versioning-objects-by-version-suspend-and-not-found#task#21.

## Evidence
Cause 1 — PutBucketVersioning with VersioningConfiguration {Status: Suspended, MfaDelete: Disabled} and no x-amz-mfa header: expected status 200 with an empty body; actual status 400 with body {'Error': {'Code': 'IllegalVersioningConfigurationException', 'Message': 'VersioningConfiguration.MfaDelete requires the x-amz-mfa request header, which this request does not carry', 'RequestId': <per-run>, 'HostId': <per-run>}}. The emulator's message is wholly emulator-authored (it names the internal member path 'VersioningConfiguration.MfaDelete'); AWS emits no error here at all.
Cause 2 — GetBucketVersioning on the same bucket: $.VersioningConfiguration.Status expected 'Suspended', got 'Enabled' (and still 'Enabled' after 30 polls); $.VersioningConfiguration.MfaDelete expected 'Disabled', got None (member absent).

## Steps to Reproduce
CreateBucket, PutBucketVersioning with VersioningConfiguration Status=Enabled, then PutBucketVersioning with VersioningConfiguration Status=Suspended and MfaDelete=Disabled and NO x-amz-mfa header — this must answer 200 (the emulator answers 400 IllegalVersioningConfigurationException). Then GetBucketVersioning: Status must be 'Suspended' and MfaDelete must be 'Disabled' (the emulator still reports Status 'Enabled' and omits MfaDelete). Replay fixture s3-bucket-tags-at-create-generic-tagging-versioning-objects-by-version-suspend-and-not-found, steps task#20 and task#21.
