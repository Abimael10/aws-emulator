Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms__opensearch.asymmetric-cmk-create-domain-rejected
Issue-Key: kms__opensearch.asymmetric-cmk-create-domain-rejected|run#9|AssertionError|raise AssertionError(
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# kms__opensearch: CreateDomain accepts an asymmetric CMK for encryption at rest instead of refusing with ValidationException

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomain

## Description
Cause 1 of 1: the domain's EncryptionAtRestOptions.KmsKeyId is not checked for symmetry.
  AWS: recorded behavior only — OpenSearch resolves the key in KMS and refuses an asymmetric key with 400 ValidationException whose message is 'Error in Accessing KmsKeyID with details:KmsKeyID:<kmsKeyId> is not a Symmetric CMK. Only Symmetric CMKs may be used for encryption at rest.' — fixed text with the key ARN taken from the request. No domain is created (DescribeDomain on the name is a not-found refusal), and the identical request with a symmetric key is accepted.
  Emulator: CreateDomain SUCCEEDS with an RSA_2048 SIGN_VERIFY key and returns the domain ARN ('arn:aws:es:<region>:<account>:domain/<name>').
  Why: a client relying on the service to reject an unusable key gets a domain whose at-rest encryption could never work, and the ValidationException its deployment gate matches on never arrives.
  Fix: the CreateDomain handler's EncryptionAtRestOptions validation — resolve the key's KeySpec/KeyUsage in KMS state and refuse a non-symmetric key before creating the domain.
  Traces: kms__opensearch.asymmetric-cmk-create-domain-rejected.

## Evidence
asymmetric-cmk-create-domain-rejected — AssertionError: CreateDomain with the asymmetric key 'arn:aws:kms:us-west-2:<acct>:key/<id>' was accepted: 'arn:aws:es:us-west-2:<acct>:domain/sim2real-xka-<id>'. Expected ('ValidationException', 'Error in Accessing KmsKeyID with details:KmsKeyID:<key arn> is not a Symmetric CMK. Only Symmetric CMKs may be used for encryption at rest.') and no domain created.

## Steps to Reproduce
1. kms.create_key(KeySpec='RSA_2048', KeyUsage='SIGN_VERIFY') -> note Arn.
2. opensearch.create_domain(DomainName=<name>, EncryptionAtRestOptions={'Enabled': True, 'KmsKeyId': <that arn>}, ...) -> expect 400 ValidationException with the message above and DescribeDomain(<name>) to be not-found.
3. Repeat with a SYMMETRIC_DEFAULT key, changing nothing else -> expect success.
