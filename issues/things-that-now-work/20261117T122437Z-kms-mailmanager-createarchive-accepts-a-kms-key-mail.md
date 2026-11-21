Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms__mailmanager.create-archive-rejects-a-kms-key-mail-manager-cannot-use
Issue-Key: kms__mailmanager.create-archive-rejects-a-kms-key-mail-manager-cannot-use|_client_error#4|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# kms__mailmanager: CreateArchive accepts a KMS key Mail Manager cannot use instead of refusing with AccessDeniedException

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateArchive

## Description
Cause 1 of 1: the archive's KmsKeyArn is never resolved or authorization-checked.
  AWS: recorded behavior only — CreateArchive with a KmsKeyArn Mail Manager cannot use is refused with AccessDeniedException and the exact message 'Unable to use KMS key' (fixed text, no variable parts), for both a well-formed but nonexistent key ARN and an existing key whose policy does not let the Mail Manager service principal use it. No archive is created: ListArchives (all pages) never shows the requested ArchiveName.
  Emulator: the call returns 200 with an ArchiveId, creating an archive under a key that does not exist or cannot be used.
  Why: a client relying on the create call to validate its key gets an archive whose mail could never be encrypted/decrypted, and the AccessDeniedException its error handling is written against never arrives.
  Fix: the CreateArchive handler's KmsKeyArn validation — resolve the key in KMS state, evaluate the Mail Manager service principal's access, and refuse before writing the archive.
  Traces: kms__mailmanager.create-archive-rejects-a-kms-key-mail-manager-cannot-use.

## Evidence
create-archive-rejects-a-kms-key-mail-manager-cannot-use — AssertionError: expected the call to be refused, but it succeeded: {'ArchiveId': '804e9373583c4b7f910a2dec89025cc1', 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}. Expected ('AccessDeniedException', 'Unable to use KMS key') and the name absent from ListArchives, for both a nonexistent key ARN (arn:aws:kms:<region>:<acct>:key/00000000-0000-0000-0000-000000000000) and an existing key that does not grant Mail Manager.

## Steps to Reproduce
1. mailmanager.create_archive(ArchiveName=<name>, KmsKeyArn='arn:aws:kms:<region>:<account>:key/00000000-0000-0000-0000-000000000000') -> expect AccessDeniedException 'Unable to use KMS key'; ListArchives must not contain <name>.
2. Create a CMK whose policy does not grant the Mail Manager service principal and repeat with its ARN -> same refusal.
