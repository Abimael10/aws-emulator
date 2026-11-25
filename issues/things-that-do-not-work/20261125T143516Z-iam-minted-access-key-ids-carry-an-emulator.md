Issue-Source: trace-sweep-divergence
Issue-Trace: iam-list-access-keys-lifecycle
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iam-list-access-keys-lifecycle iam-access-key-error-paths iam-read-only-inspection-of-operator-identity iam-self-user-access-key-not-found
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam|CreateAccessKey|2xx->2xx|$.CreateAccessKeyResponse.CreateAccessKeyResult.AccessKey.AccessKeyId
Issue-Key: iam|ListAccessKeys|2xx->2xx|$.ListAccessKeysResponse.ListAccessKeysResult.AccessKeyMetadata.member.AccessKeyId
Issue-Commit: ff9a8ef7ac2eee2eef81f4aebd41ca244dae0c42

# iam: minted access-key ids carry an EMULATOR prefix instead of AWS's AKIA access-key-id form

> _Note: this issue was identified against the code base at commit `ff9a8ef7ac2eee2eef81f4aebd41ca244dae0c42`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAccessKey

## Description
ONE net-new root cause, observed on TWO recorded steps that must BOTH pass before this issue is submitted: the mint path (CreateAccessKey) and the read-back projection (ListAccessKeys). Both are reported unowned by any open issue and blocked by no stub.

=== Cause 1 of 1 (two steps: iam-list-access-keys-lifecycle#task#3, iam-access-key-error-paths#task#2) ===
AWS: Smithy iam#CreateAccessKey -> output AccessKey (required); AccessKey.AccessKeyId -> accessKeyIdType (@length 16-128, @pattern ^[\w]+$), documented "The ID for this access key"; iam#ListAccessKeys -> AccessKeyMetadata (required, list of AccessKeyMetadata{UserName, AccessKeyId -> the same accessKeyIdType, Status, CreateDate}); errors for both are NoSuchEntity(404)/LimitExceeded(409)/ServiceFailure(500), so a 200 body is the whole contract. The model constrains only length and charset, so the id CLASS is recorded/documented behavior: AWS's IAM unique-identifier prefixes are fixed and load-bearing — a long-term IAM user access key id is 20 characters beginning with the literal prefix 'AKIA' (a temporary STS credential begins 'ASIA'). The requirement is that FORM — 'AKIA' followed by 16 further characters — never any one run's literal id, which is per-run state; that is why the recorded value is captured as the redaction class '[REDACTED:aws-access-key]': the recording's value was recognizable as an AWS access-key id, and the emulator's is not. ListAccessKeys must project the identical form for the same key.
Emulator: both steps answer 200 and every other member matches (UserName, Status, CreateDate and the surrounding lifecycle all pass), but the id is 'EMULATOR'-prefixed: CreateAccessKey minted 'EMULATORAKQTNQ7AAFF6' and ListAccessKeys projected 'EMULATORE91CE3BAA56A' for the keys in these fixtures. The ids are 20 characters and satisfy the Smithy pattern, so the serializer is right — the allocator is producing ids outside AWS's access-key-id class.
Why: a client relying on AWS's behavior classifies a credential by its prefix — long-term vs temporary, secret scanners, policy linters, presign helpers and credential-provider chains all branch on 'AKIA'/'ASIA'. Every key the emulator mints is misclassified, and a client that validates the documented prefix rejects a credential the emulator says it just created, so the whole access-key lifecycle (rotate, deactivate, delete by id) cannot be driven by such a client.
Fix: the access-key-id allocator behind the IAM user access-key state — the mint reached by CreateAccessKey, whose value is then projected by ListAccessKeys, GetAccessKeyLastUsed and UpdateAccessKey — not the response serializers, which are already correct. Related open issues describe the same EMULATOR prefix for OTHER recorded units and carry neither of these wire replay keys: 20261124T000913Z-iam-4-net-new-causes-access-key-ids.md (behavioral, cause 1, grounded in iam.iam-access-key-crud and iam.user-access-key-round-trips-status-and-never-used) and 20261118T021609Z-iam-4-net-new-causes-denials-append-an.md (behavioral). The earlier wire issue that covered it, 20261116T105641Z-iam-7-net-new-causes-minted-access-key.md, is RESOLVED while both clusters below still diverge. Closing this cause must make all of them pass together.
Traces: iam-list-access-keys-lifecycle#task#3 (CreateAccessKey, the mint) and iam-access-key-error-paths#task#2 (ListAccessKeys, the projection; the same cluster also covers iam-list-access-keys-lifecycle#task#4, iam-read-only-inspection-of-operator-identity#task#4 and iam-self-user-access-key-not-found#task#3).

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout: 1742/1799 steps passed (96.8%), 51 mismatched, bootstrap_signal false, emulator_up true. Both clusters report stub_path "" and owned_by [].

Step 1 — iam-list-access-keys-lifecycle#task#3, CreateAccessKey, expected 200 / got 200 (verdict mismatch, affected_count 1):
- $.CreateAccessKeyResponse.CreateAccessKeyResult.AccessKey.AccessKeyId: value_mismatch (expected '[REDACTED:aws-access-key]', got 'EMULATORAKQTNQ7AAFF6')

Step 2 — iam-access-key-error-paths#task#2, ListAccessKeys, expected 200 / got 200 (verdict mismatch, affected_count 4; other members of the cluster: iam-list-access-keys-lifecycle#task#4, iam-read-only-inspection-of-operator-identity#task#4, iam-self-user-access-key-not-found#task#3):
- $.ListAccessKeysResponse.ListAccessKeysResult.AccessKeyMetadata.member.AccessKeyId: value_mismatch (expected '[REDACTED:aws-access-key]', got 'EMULATORE91CE3BAA56A')

The two 'EMULATOR...' literals are this run's ids and are not themselves the requirement; the requirement is the AKIA+16 form. No other member of either response diverged, and the surrounding steps of both fixtures (user creation, key status change, delete, and the NoSuchEntity error paths) passed.

Model grounding — smithy_query on services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json: CreateAccessKey input UserName (existingUserNameType @length 1-128 @pattern ^[\w+=,.@-]+$), output AccessKey required, errors LimitExceeded(409)/NoSuchEntity(404)/ServiceFailure(500); AccessKey.AccessKeyId required -> accessKeyIdType @length 16-128 @pattern ^[\w]+$; ListAccessKeys output AccessKeyMetadata required (accessKeyMetadataListType) with AccessKeyMetadata.AccessKeyId -> the same accessKeyIdType; no prefix is modelled, so the AKIA class is documented/recorded behavior.

## Steps to Reproduce
1. git_clone the repo, boot the emulator built from that checkout and replay simparity-wire-aws-v1.
2. Step 1: fixture iam-list-access-keys-lifecycle, step task#3 — CreateUser then CreateAccessKey for that user. AccessKey.AccessKeyId must be 20 characters beginning with the literal 'AKIA'; observed 'EMULATOR'-prefixed. Confirm UserName/Status/SecretAccessKey stay correct.
3. Step 2: fixture iam-access-key-error-paths, step task#2 — ListAccessKeys for the same user. Every AccessKeyMetadata.AccessKeyId must carry the identical AKIA form and must equal the id CreateAccessKey returned.
4. Check the sibling projections of the same stored id: GetAccessKeyLastUsed and UpdateAccessKey for that key must accept and echo the same id.
5. Re-replay iam-list-access-keys-lifecycle#task#3 and iam-access-key-error-paths#task#2 (and the cluster's other members iam-list-access-keys-lifecycle#task#4, iam-read-only-inspection-of-operator-identity#task#4, iam-self-user-access-key-not-found#task#3) and confirm all pass.
