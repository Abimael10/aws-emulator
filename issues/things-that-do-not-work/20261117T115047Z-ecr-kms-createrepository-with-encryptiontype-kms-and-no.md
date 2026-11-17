Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__kms.kms-encrypted-repository-defaults-to-the-aws-managed-ecr-key
Issue-Key: ecr__kms.kms-encrypted-repository-defaults-to-the-aws-managed-ecr-key|run#10|KeyError|assert created["encryptionConfiguration"]["kmsKey"] == managed_arn, (
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# ecr__kms: CreateRepository with encryptionType KMS and no kmsKey omits kmsKey from the returned encryptionConfiguration instead of defaulting it to the AWS-managed ECR key ARN

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
Cause 1 of 1: the KMS default key is never resolved into the repository's encryption configuration.
AWS: Smithy com.amazonaws.ecr#EncryptionConfiguration has a required encryptionType and an optional kmsKey (@length 1..2048), documented: "If you use the KMS encryption type, specify the KMS key to use for encryption. The alias, key ID, or full ARN of the KMS key can be specified. The key must exist in the same Region as the repository. If no key is specified, the default Amazon Web Services managed KMS key for Amazon ECR will be used." The EncryptionType doc repeats it: with KMS "you can either use the default Amazon Web Services managed KMS key for Amazon ECR, or specify your own KMS key". So a CreateRepository with encryptionConfiguration={encryptionType:'KMS'} and no kmsKey succeeds and the returned repository's encryptionConfiguration.kmsKey is PRESENT and set to the ARN of the account's AWS-managed ECR key — the key KMS DescribeKey resolves for alias/aws/ecr in that Region. The ARN itself is an environment value (account and Region come from the caller's context, the key id is whatever KMS assigned); the requirement is that the member is present and equals the ARN the KMS-managed aws/ecr key reports, not any literal value.
Emulator: the CreateRepository response's encryptionConfiguration has NO kmsKey member at all — reading created['encryptionConfiguration']['kmsKey'] raises KeyError('kmsKey'). The emulator accepts encryptionType KMS but leaves the key unresolved, so the repository is reported as KMS-encrypted with no key.
Why: a client cannot discover which CMK protects a repository. Anything that audits encryption at rest, builds a key policy or grant around the repository's key, or re-creates the repository from a describe (DescribeRepositories/CreateRepository round-trip) sees a KMS configuration with no key and either crashes on the missing member or silently drops the encryption setting. It also makes the AWS-managed-key default — the documented behavior for an omitted kmsKey — unobservable.
Fix: the ECR CreateRepository handler's encryption-configuration resolution, plus the repository state it persists and the DescribeRepositories serializer that reads it back: when encryptionType is KMS (or KMS_DSSE) and kmsKey is omitted, resolve the account/Region's AWS-managed aws/ecr key and store and serialize its ARN as encryptionConfiguration.kmsKey.
Traces: ecr__kms.kms-encrypted-repository-defaults-to-the-aws-managed-ecr-key.
Related: open issue 20261024T182018Z-ecr-kms-ecr-createrepository-with-encryptiontype-kms-creates-no-kms.md reports that the same call creates no KMS GRANT; it does not cover the missing kmsKey member in the response.

## Evidence
ecr__kms.kms-encrypted-repository-defaults-to-the-aws-managed-ecr-key:
  File check_ecr__kms_kms_encrypted_repository_defaults_to_the_aws_managed_ecr_key.py, line 76, in run
    assert created["encryptionConfiguration"]["kmsKey"] == managed_arn
  KeyError: 'kmsKey'
Expected: CreateRepository(encryptionConfiguration={'encryptionType':'KMS'}) returns repository.encryptionConfiguration = {'encryptionType':'KMS', 'kmsKey': <ARN of the account/Region AWS-managed aws/ecr key>}.
Actual: repository.encryptionConfiguration carries encryptionType only — the kmsKey member is absent from the response, so the lookup raises KeyError rather than mismatching a value.

## Steps to Reproduce
1. ecr.create_repository(repositoryName=R, encryptionConfiguration={'encryptionType': 'KMS'}) — omit kmsKey.
2. Read response['repository']['encryptionConfiguration'].
Expected: it contains kmsKey equal to the ARN kms.describe_key(KeyId='alias/aws/ecr') reports for the caller's account and Region. Actual: no kmsKey member is present.
3. ecr.describe_repositories(repositoryNames=[R]) shows the same omission on the read path.
