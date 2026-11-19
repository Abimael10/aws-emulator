Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codecommit.repository-encryption-key-rotation-moves-the-dependency-not-just-the-field
Issue-Shape-Id: com.amazonaws.codecommit#CodeCommit_20150413
Issue-Key: codecommit.repository-encryption-key-rotation-moves-the-dependency-not-just-the-field|_error_code#43|AssertionError|raise AssertionError(
Issue-Commit: 7bfda40add59a57de4c47a21257187f7eb42eb8e

# CodeCommit UpdateRepositoryEncryptionKey accepts a rotation onto a Disabled CMK instead of refusing with EncryptionKeyDisabledException (1 cause)

> _Note: this issue was identified against the code base at commit `7bfda40add59a57de4c47a21257187f7eb42eb8e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateRepositoryEncryptionKey

## Description
One net-new root cause in the codecommit crate, grounded in a failing behavioral check the replay report left unmarked (not ALREADY FILED, not OWNED by any open issue).

Cause 1 of 1: UpdateRepositoryEncryptionKey does not consult the target KMS key's state, so a rotation onto a CMK whose KeyState is 'Disabled' is accepted (HTTP 200) where AWS refuses it.

AWS: Smithy UpdateRepositoryEncryptionKey takes repositoryName (@required, RepositoryName, @length 1..100, @pattern ^[\w\.-]+$) and kmsKeyId (@required, KmsKeyId, @pattern ^[a-zA-Z0-9:/_-]+$), and models EncryptionKeyDisabledException (errorKind client, httpError 400, doc 'The encryption key is disabled.') among its errors alongside EncryptionKeyNotFoundException / …UnavailableException / …AccessDeniedException / …InvalidUsageException. Pointing a repository at a CMK requires CodeCommit to use that key, so a kmsKeyId that resolves to a key in KeyState 'Disabled' is refused HTTP 400 with error code EncryptionKeyDisabledException and the repository is left on the key it already had. Message: the fixed text is the 'KMS key … is disabled' sentence; recorded behavior shows AWS answering this call with either 'KMS key <kmsKeyId> is disabled' or 'KMS key The current KMS key of this repository or <kmsKeyId> is disabled' for the same request, the only variable part being the key selector/ARN taken from the request (and the repository's current key from state) — which is why the check grades only the error code and not the sentence. Status 400 and the code EncryptionKeyDisabledException are the requirement here.

Emulator: the call SUCCEEDS. The check disables CMK A (the key the repository was created under and has already been rotated away from), then calls UpdateRepositoryEncryptionKey(repositoryName=R, kmsKeyId=<arn of disabled A>) and the harness reports AssertionError 'the call was expected to be refused because the key it names is Disabled, but it SUCCEEDED -- the target is not consulting KMS key state at all'. The handler says so in its own words: services/service-com-amazonaws-codecommit-codecommit-20150413/src/ops/update_repository_encryption_key.rs documents (lines 15-28) that it reports only RepositoryDoesNotExistException and EncryptionKeyInvalidIdException and that 'a rotation onto a key that is disabled is accepted'; handle() checks crate::state::is_kms_key_selector, resolves the selector via crate::state::resolve_encryption_key and writes it with rotate_encryption_key without ever reading the resolved key's KeyState. Note the earlier legs of this same check PASS — GetBlob is refused EncryptionKeyDisabledException while the repository's current key is disabled and serves the original bytes again after enable_key — so the data-plane key-state enforcement described as Cause 2 of the open issue 20261022T213415Z-codecommit-putcommentreaction-is-a-501-stub-cmk-disablement.md now holds; that issue covers the read/write paths (get_blob/get_file/get_branch/get_commit/put_file) and the check id codecommit.reads-blocked-when-the-repository-kms-key-is-disabled, and neither it nor any other open codecommit issue lists this check id or this operation, so this control-plane cause is recorded separately.

Why: a client that guards a key rotation by disabling the candidate CMK (or that re-points a repository at a key an operator has just revoked) gets a 200 and believes the rotation landed, while against AWS the same call is refused and the repository stays on its working key. Every runbook and test that depends on CodeCommit validating the key it is handed — break-glass revocation, staged rotation, CMK lifecycle checks — passes against the emulator and fails against AWS; code written to handle EncryptionKeyDisabledException on this operation never sees that path exercised, and the emulator can be driven into a state where the repository depends on a key it would refuse to accept.

Fix: the KMS key-state resolution in the UpdateRepositoryEncryptionKey handler's validation step — after the selector is resolved to a concrete key, consult that key's state in the kms service state and refuse with EncryptionKeyDisabledException (HTTP 400) before the store is written, leaving the repository's kmsKeyId unchanged (the check re-reads GetRepository after the refusal and expects the pre-call key). The resolution helper in the crate's state module is the shared place the data-plane paths already use for this judgement.

Traces: codecommit.repository-encryption-key-rotation-moves-the-dependency-not-just-the-field (the check's assertion at the UpdateRepositoryEncryptionKey-onto-disabled-key leg).

## Evidence
Cause 1 — codecommit.repository-encryption-key-rotation-moves-the-dependency-not-just-the-field [mismatch]: AssertionError raised by the check's _error_code() helper: 'the call was expected to be refused because the key it names is Disabled, but it SUCCEEDED -- the target is not consulting KMS key state at all'. Expected: HTTP 400 with error code EncryptionKeyDisabledException from cc.update_repository_encryption_key(repositoryName=R, kmsKeyId=<ARN of the Disabled CMK>), followed by GetRepository still reporting the key in effect before the refused call. Actual: HTTP 200 — the rotation is accepted and the repository is re-pointed at the disabled key. All preceding legs of the check pass (CreateRepository reports the old CMK; UpdateRepositoryEncryptionKey reports kmsKeyId=new and originalKmsKeyId=old; blobId/treeId are stable across the rotation; GetBlob is refused EncryptionKeyDisabledException while the NEW key is Disabled and serves the original bytes after enable_key; reads stay stable while the OLD key is Disabled), so the data-plane enforcement is present and only this operation's key-state validation is missing. Handler evidence: services/service-com-amazonaws-codecommit-codecommit-20150413/src/ops/update_repository_encryption_key.rs lines 21-28 ('This emulator's CodeCommit does not resolve key state at all, so it reports none of them') and lines 211-279 (selector check, resolve, write — no state read). Key ARNs, repository names and commit/blob ids vary per run; the requirement is the status and error code, not any recorded literal.

## Steps to Reproduce
1. Boot the emulator and point botocore at it (region/credentials as the harness uses).
2. kA = kms.create_key(...)['KeyMetadata']; kB = kms.create_key(...)['KeyMetadata'] (two distinct CMKs, both KeyState 'Enabled').
3. cc.create_repository(repositoryName=R, kmsKeyId=kA['Arn']); cc.put_file(repositoryName=R, branchName='main', filePath='payload.txt', fileContent=b'...', commitMessage='...').
4. cc.update_repository_encryption_key(repositoryName=R, kmsKeyId=kB['Arn']) -> 200, kmsKeyId=kB, originalKmsKeyId=kA; cc.get_repository(repositoryName=R) reports kmsKeyId=kB.
5. kms.disable_key(KeyId=kA['KeyId']); poll kms.describe_key until KeyMetadata.KeyState == 'Disabled'.
6. cc.update_repository_encryption_key(repositoryName=R, kmsKeyId=kA['Arn']) -> EXPECT a ClientError with HTTP 400 and Error.Code == 'EncryptionKeyDisabledException' (message is the 'KMS key … is disabled' sentence, its key selector taken from the request); OBSERVE HTTP 200 and an accepted rotation.
7. cc.get_repository(repositoryName=R) -> must still report kmsKeyId == kB['Arn'] (the refusal must leave the repository on the key it had).
8. Re-replay codecommit.repository-encryption-key-rotation-moves-the-dependency-not-just-the-field.
