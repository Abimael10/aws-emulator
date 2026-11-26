Issue-Source: trace-sweep-divergence
Issue-Trace: iam-identity-providers-saml-and-oidc-batch6
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iam-identity-providers-saml-and-oidc-batch6
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam|GetSAMLProvider|2xx->2xx|$.GetSAMLProviderResponse.GetSAMLProviderResult.SAMLProviderUUID
Issue-Commit: 3a71fbf666403ef651a16a3f891bc8ef488168a9

# iam: GetSAMLProvider publishes a hex-digest SAMLProviderUUID instead of an IAM-minted identifier

> _Note: this issue was identified against the code base at commit `3a71fbf666403ef651a16a3f891bc8ef488168a9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSAMLProvider

## Description
ONE net-new cause in the iam service (one unowned wire cluster, stub_path "", owned_by []).

=== Cause 1 of 1: GetSAMLProvider's SAMLProviderUUID is a locally computed hex digest, not an identifier of the shape IAM mints ===

AWS: Smithy com.amazonaws.iam#GetSAMLProvider -> input SAMLProviderArn (required, arnType @length 20-2048); output SAMLProviderUUID (privateKeyIdType, @length 22-64, @pattern ^[A-Z0-9]+$), SAMLMetadataDocument (@length 1000-10000000), CreateDate, ValidUntil, Tags, AssertionEncryptionMode, PrivateKeyList; errors InvalidInput (400, awsQueryError InvalidInput), NoSuchEntity (404, awsQueryError NoSuchEntity), ServiceFailure (500, awsQueryError ServiceFailure). SAMLProviderUUID is the opaque identifier IAM ASSIGNS to the provider when it is created: IAM mints it, it is never derived from anything the caller sent, and it is drawn from the full uppercase-alphanumeric alphabet the pattern allows. Recorded behavior only for the exact alphabet and width: the corpus does not assert the literal value - it records it as '<shaped-not-asserted>' and asserts the member's SHAPE - and the emulator's value fails that shape assertion while still satisfying the model's @length/@pattern. So the requirement here is the identifier's shape: an IAM-minted opaque id over [A-Z0-9], not a digest.
Emulator: status 200 and the rest of the GetSAMLProvider body matches, but SAMLProviderUUID is served as a 32-character uppercase HEXADECIMAL string - 'B469BA82EE1522FFB2E6ADE332E5B2AC', i.e. characters restricted to 0-9 and A-F, the shape of an MD5-style digest. The replay verdict is value_mismatch on $.GetSAMLProviderResponse.GetSAMLProviderResult.SAMLProviderUUID (expected '<shaped-not-asserted>', got 'B469BA82EE1522FFB2E6ADE332E5B2AC'): the harness is not comparing the recorded id to the emulator's, it is rejecting the shape. Hypothesis (neither the model nor the recording states it): the value is a digest computed over the provider's name or its metadata document rather than drawn from the service's id mint, which is also why it is restricted to the hex alphabet.
Why: SAMLProviderUUID is the provider's stable identity for everything that is keyed off it rather than off the ARN - correlating a provider across reads, naming it in audit and drift records, and matching the ids IAM reports for the same provider elsewhere. A client that validates the id against the shape IAM issues (or that only accepts an id its own catalog of IAM-minted ids could contain) rejects a well-formed provider, and a client that treats the id as independent of the provider's inputs sees it move whenever a name or metadata document it was derived from changes - two providers created with the same inputs collide on one id, and one provider re-created with different metadata changes identity. Either way the id stops being the opaque per-provider handle AWS guarantees.
Fix: the SAMLProviderUUID the SAML-provider record carries, minted where the provider is created in the IAM service crate (CreateSAMLProvider) and merely republished by this read path - the id must come from the service's own identifier mint over the full [A-Z0-9] alphabet the model's privateKeyIdType pattern allows and be stored on the provider record, not recomputed per read from the provider's name or metadata. The same single stored value must back every read that reports the provider, so GetSAMLProvider and ListSAMLProviders cannot disagree about one provider's id.
Related open issue, which carries NEITHER this replay key nor this operation: 20261125T143516Z-iam-minted-access-key-ids-carry-an-emulator.md is the same FAMILY of fault (an id this emulator mints does not have the shape AWS mints) on ListAccessKeys/AccessKeyId; it does not cover GetSAMLProvider, and the cluster here is reported owned_by [].
Traces: iam-identity-providers-saml-and-oidc-batch6#task#3.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout: summary trace_set_id simparity-wire-aws-v1, emulator_up true, bootstrap_signal false, steps_total 1799, steps_passed 1749, steps_mismatched 49, steps_errored 0, steps_blocked 1, score_pct 97.2, divergence_count 30. This cluster: service iam, operation GetSAMLProvider, verdict mismatch, expected_status 200 / actual_status 200, affected_count 1, stub_path "", owned_by [].

Cluster detail, verbatim:
- $.GetSAMLProviderResponse.GetSAMLProviderResult.SAMLProviderUUID: value_mismatch (expected '<shaped-not-asserted>', got 'B469BA82EE1522FFB2E6ADE332E5B2AC')

divergence_key: service=iam|GetSAMLProvider|status=200->200|keys=$.GetSAMLProviderResponse.GetSAMLProviderResult.SAMLProviderUUID

The status (200) and every other member of the GetSAMLProviderResult body - SAMLMetadataDocument, CreateDate, ValidUntil, Tags - did not diverge; this one member is the whole divergence. The literal string 'B469BA82EE1522FFB2E6ADE332E5B2AC' is this run's value and is not itself the requirement; what the divergence shows is its CLASS: 32 characters confined to the hex alphabet 0-9A-F. The expected side is '<shaped-not-asserted>', i.e. the corpus deliberately does not assert the recorded id's literal text - only its shape - so this is not an assertion on an account-specific or live-run-only value.

Model grounding - smithy_query on services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json:
- GetSAMLProvider: input SAMLProviderArn required (arnType @length 20-2048); output SAMLProviderUUID (privateKeyIdType, @length 22-64, @pattern ^[A-Z0-9]+$), SAMLMetadataDocument (@length 1000-10000000), CreateDate (timestamp), ValidUntil (timestamp), Tags (tagListType @length 0-50), AssertionEncryptionMode (enum), PrivateKeyList (@length 0-2); errors InvalidInputException (client, 400, awsQueryError InvalidInput/400), NoSuchEntityException (client, 404, awsQueryError NoSuchEntity/404), ServiceFailureException (server, 500, awsQueryError ServiceFailure/500).
- The served value satisfies @length 22-64 and @pattern ^[A-Z0-9]+$, so the model alone does not catch it: the divergence is that the id is a digest over the hex subset rather than an IAM-minted opaque id over the modelled alphabet. No message text is modelled for this operation, so the shape of the identifier is recorded behavior.

## Steps to Reproduce
1. git_clone the repo, boot the emulator built from that checkout and replay simparity-wire-aws-v1 (replay_wire_corpus).
2. Fixture iam-identity-providers-saml-and-oidc-batch6, step task#3 - GetSAMLProvider on the SAML provider the fixture created. Observed SAMLProviderUUID 'B469BA82EE1522FFB2E6ADE332E5B2AC': 32 characters over 0-9A-F only. The step's assertion is on the member's shape, not on the recorded literal ('<shaped-not-asserted>').
3. Reproduce by hand against the emulator: CreateSAMLProvider(Name=<name>, SAMLMetadataDocument=<metadata>), then GetSAMLProvider(SAMLProviderArn=<returned arn>). Inspect SAMLProviderUUID - it must be an identifier the service MINTED, drawn from the full [A-Z0-9] alphabet privateKeyIdType allows, not a hex digest.
4. Show it is derived rather than minted: create two SAML providers with DIFFERENT names but the SAME metadata document, and two with the same name in different accounts/paths, and compare their SAMLProviderUUIDs; then delete and re-create one provider with a CHANGED metadata document and compare again. A minted id is independent of those inputs and is unique per provider; a digest is not.
5. Confirm consistency across read paths: ListSAMLProviders and GetSAMLProvider must report the same stored id for one provider, and the id must not change between two successive GetSAMLProvider calls.
6. Re-replay iam-identity-providers-saml-and-oidc-batch6#task#3 and confirm it passes before submitting.
