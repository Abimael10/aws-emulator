Issue-Source: trace-sweep-divergence
Issue-Trace: iam-saml-provider-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iam-saml-provider-lifecycle iam-saml-provider-update iam-identity-providers-saml-and-oidc-batch6
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam|GetSAMLProvider|2xx->2xx|$.GetSAMLProviderResponse.GetSAMLProviderResult.SAMLKeyHash.member.KeyHashes.member
Issue-Key: iam|GetSAMLProvider|2xx->2xx|$.GetSAMLProviderResponse.GetSAMLProviderResult.SAMLProviderUUID
Issue-Commit: 602156962253e64136e9b0eeef8fba5e51605e54

# IAM GetSAMLProvider reports a 40-hex-digit SAML key hash and a non-conforming provider UUID

> _Note: this issue was identified against the code base at commit `602156962253e64136e9b0eeef8fba5e51605e54`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSAMLProvider

## Description
GetSAMLProvider answers 200 with the right response envelope, but the key-hash projection it derives from the uploaded SAML metadata document is computed with the wrong digest family, and the generated provider UUID does not satisfy the shape the recording asserts. Both causes sit in the GetSAMLProvider response projection.

Cause 1 of 2: the SAMLKeyHash key-hash digest is 40 hex digits (a 160-bit digest) where AWS reports 96 hex digits (a 384-bit digest). AWS: recorded behavior only — the Smithy IAM model's GetSAMLProvider output binds SAMLProviderUUID, SAMLMetadataDocument, CreateDate, ValidUntil, Tags, AssertionEncryptionMode and PrivateKeyList, and does not document the SAMLKeyHash/KeyHashes element, so the requirement here is what the recordings show: for every SAML provider, each KeyHashes member is a lowercase hex string of 96 characters, i.e. a 384-bit digest over the signing key material in the provider's metadata document. The value itself is content-derived, not per-run: the same metadata document must always hash to the same string, and its LENGTH and digest family are the part that is a requirement. Emulator: returns a 40-character lowercase hex string per KeyHashes member — a 160-bit (SHA-1-width) digest. Hypothesis: the projection hashes the certificate bytes with SHA-1 instead of the 384-bit digest AWS uses. Why: a client that pins or compares a SAML signing-key fingerprint against the value IAM reports (key-rotation checks, drift detection, console parity) compares strings of different lengths and can never match, and a fingerprint of the wrong digest family is not usable as a trust anchor. Fix: the GetSAMLProvider response serializer/projection that derives SAMLKeyHash.KeyHashes from the stored SAMLMetadataDocument (the same code path feeds the provider-update read-back). Traces: iam-saml-provider-lifecycle#task#2, and the cluster's other member iam-saml-provider-update#task#3.

Cause 2 of 2: SAMLProviderUUID is minted in a shape the recording's shape assertion rejects. AWS: Smithy GetSAMLProvider output member SAMLProviderUUID targets privateKeyIdType, @length min 22 max 64, @pattern ^[A-Z0-9]+$ — an opaque uppercase-alphanumeric identifier. The recording asserts this member's SHAPE rather than a literal value (expected '<shaped-not-asserted>'), because the value is account-specific; the requirement is therefore the identifier family, not any one run's string. Emulator: emits a 22-character value built from a fixed 'SAMLP' prefix plus a per-object suffix, which fails the recorded shape assertion for this member. Why: the shape IAM guarantees for this identifier is what clients key storage and equality on; an id drawn from a different family (service-prefixed rather than opaque) is not interchangeable with the real one and is distinguishable from an AWS-minted id. Fix: the id minting used by the SAML-provider state/serializer for SAMLProviderUUID — it must produce an opaque privateKeyIdType-shaped value, not a prefixed one. Traces: iam-identity-providers-saml-and-oidc-batch6#task#3.

Note: this cluster is NOT covered by the open issue 20261109T134638Z-iam-query-responses-omit-or-mis-encode-modeled.md, which owns other GetOpenIDConnectProvider encoding divergences and does not carry these replay keys.

## Evidence
Replay of simparity-wire-aws-v1 against the booted emulator.

Cause 1 — fixture iam-saml-provider-lifecycle step task#2, GetSAMLProvider, expected 200 / actual 200:
  $.GetSAMLProviderResponse.GetSAMLProviderResult.SAMLKeyHash.member.KeyHashes.member: value_mismatch (expected 'ab074c1a09e4a3c9fe5fe1c5d08a74f5ab7d8407c6795d47570dceed1404433d1ba330558584394ff4f953f0ffb5e623', got '83b8dc179d0460eef0ed74342152bbb1232d78b7')
  Expected: 96 lowercase hex characters (384-bit digest) derived from the uploaded metadata document. Actual: 40 hex characters (160-bit digest). The literal digests above are the recorded document's values; the requirement is the digest family/width and content-derivation, not these strings. Same cluster: iam-saml-provider-update#task#3.

Cause 2 — fixture iam-identity-providers-saml-and-oidc-batch6 step task#3, GetSAMLProvider, expected 200 / actual 200:
  $.GetSAMLProviderResponse.GetSAMLProviderResult.SAMLProviderUUID: value_mismatch (expected '<shaped-not-asserted>', got 'SAMLPBGXEQES5NRCCJJWAN')
  $.GetSAMLProviderResponse.GetSAMLProviderResult.SAMLKeyHash.member.KeyHashes.member: value_mismatch (expected '3cba5db9e9e8ce9426debd64d007fa933a92b5f42f4e7274a053aef3d7fe8ca0464d31d13459b0863fbcfb21fafbee7c', got 'f69e9bfb2430b9ca6267ff13a76f456cb02df4ec')
  The UUID line is a shape assertion, not a value assertion: the emulator's 'SAMLP'-prefixed id fails it. Smithy: SAMLProviderUUID -> privateKeyIdType, @length 22..64, @pattern ^[A-Z0-9]+$.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Cause 1: replay fixture iam-saml-provider-lifecycle step task#2 — CreateSAMLProvider with the fixture's metadata document, then GetSAMLProvider on the returned ARN; measure the length of SAMLKeyHash/member/KeyHashes/member. It is 40 hex characters; AWS reports 96.
3. Cause 2: replay fixture iam-identity-providers-saml-and-oidc-batch6 step task#3 — GetSAMLProvider and inspect SAMLProviderUUID. It is a 'SAMLP'-prefixed 22-character string, which fails the recorded shape assertion for privateKeyIdType.
4. Both steps answer HTTP 200 with the correct element structure; only these values diverge.
