Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__kms.kms-encrypted-repository-creates-exactly-one-ecr-grant-and-retires-it-on-delete
Issue-Key: ecr__kms.kms-encrypted-repository-creates-exactly-one-ecr-grant-and-retires-it-on-delete|run#55|AssertionError|assert foreign_message.startswith(kms_refusal) and foreign_message.endswith("; Proxy: null)"), (
Issue-Commit: 15eaad9fa74cff8f1dc14d7f5abd0b99014b7462

# ecr__kms: CreateRepository's KmsException for a cross-Region CMK is worded with the Java KMS exception class name instead of ECR's "The Amazon ECR action failed due to a KMS exception: ..." sentence

> _Note: this issue was identified against the code base at commit `15eaad9fa74cff8f1dc14d7f5abd0b99014b7462`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRepository

## Description
Cause 1 of 1: ECR's relayed KMS failure message is built from the Java SDK exception class name instead of ECR's own wrapper sentence.

AWS: CreateRepository with encryptionConfiguration={encryptionType: KMS, kmsKey: <ARN of a CMK in a DIFFERENT Region than the repository>} is refused HTTP 400 with error code KmsException (Smithy: ECR CreateRepository errors include KmsException, errorKind client, httpError 400; KmsException doc: "The operation failed due to a KMS exception."). The model does not state the message text, so for wording this is recorded behavior only: AWS wraps the underlying KMS failure in ECR's own sentence, i.e. the message is the FIXED text "The Amazon ECR action failed due to a KMS exception: " followed by the relayed KMS message and the AWS SDK diagnostic tail, as in
  The Amazon ECR action failed due to a KMS exception: Invalid arn <region> (Service: AWSKMS; Status Code: 400; Error Code: NotFoundException; Request ID: <kms request id>; Proxy: null)
Fixed text: "The Amazon ECR action failed due to a KMS exception: ", "Invalid arn ", and the tail "(Service: AWSKMS; Status Code: 400; Error Code: NotFoundException; Request ID: " ... "; Proxy: null)". Variable parts taken from the request and from KMS: <region> is the Region named in the kmsKey ARN that is not the repository's Region, and the Request ID is per request (never a fixed value).

Emulator: the status and the error code are right (400 / KmsException), and the diagnostic tail is right, but the leading sentence is replaced by the Java SDK exception class name. The replay recorded the message
  com.amazonaws.services.kms.model.NotFoundException: Invalid arn us-east-1 (Service: AWSKMS; Status Code: 400; Error Code: NotFoundException; Request ID: <uuid>; Proxy: null)
where AWS would have opened with "The Amazon ECR action failed due to a KMS exception: ". ("us-east-1" here is only the foreign Region the check happened to pick; the requirement is the Region from the request's kmsKey ARN.) Note the rest of this check's expectations passed on this run: the in-Region CMK repository, its single ECR grant carrying GenerateDataKey/GenerateDataKeyWithoutPlaintext, the AES256 control, and the grant retirement on delete — only the refusal wording diverges.

Why: a client that keys off ECR's documented refusal sentence (or strips the "The Amazon ECR action failed due to a KMS exception: " prefix to recover the KMS diagnostic, as console and CI tooling do) sees an unrecognised message and cannot tell an ECR-relayed KMS problem from an internal failure; the leaked `com.amazonaws.services.kms.model.*` class name is a Java-SDK artifact that real AWS never puts in the ECR message, so log/alert matching on it is built against emulator-only text.

Fix: the message construction on CreateRepository's KMS validation/relay path in the ECR service (the create_repository handler's KmsException construction): wrap the relayed KMS message in ECR's fixed sentence rather than prefixing the KMS exception type.

Related open issues do NOT cover this cause: 20261117T115047Z-ecr-kms-createrepository-with-encryptiontype-kms-and-no.md is about the default kmsKey when none is supplied, and 20261119T042800Z-cloudtrail-ecr-kms-a-kms-encrypted-ecr-repository.md is about grants not being minted at all (grants were minted on this run).

Traces: ecr__kms.kms-encrypted-repository-creates-exactly-one-ecr-grant-and-retires-it-on-delete (assertion on the cross-Region refusal message).

## Evidence
Check: ecr__kms.kms-encrypted-repository-creates-exactly-one-ecr-grant-and-retires-it-on-delete

Expected (AWS): error Code "KmsException", HTTP 400, Message starting with
  "The Amazon ECR action failed due to a KMS exception: Invalid arn <foreign-region> (Service: AWSKMS; Status Code: 400; Error Code: NotFoundException; Request ID: "
and ending with "; Proxy: null)".

Actual (emulator): Code "KmsException", HTTP 400, Message
  "com.amazonaws.services.kms.model.NotFoundException: Invalid arn us-east-1 (Service: AWSKMS; Status Code: 400; Error Code: NotFoundException; Request ID: 44d93d99-3e57-4f01-a609-83f00cf0599f; Proxy: null)"

Replay assertion:
  AssertionError: CreateRepository with the 'us-east-1' key was refused with 'com.amazonaws.services.kms.model.NotFoundException: Invalid arn us-east-1 (Service: AWSKMS; Status Code: 400; Error Code: NotFoundException; Request ID: ...; Proxy: null)'
(the request id and the concrete foreign Region are per-run values, not part of the requirement; the diverging part is the fixed leading sentence)

## Steps to Reproduce
1. kms:CreateKey in the caller's Region; ecr:CreateRepository with encryptionConfiguration={encryptionType: "KMS", kmsKey: <that key's ARN>} — succeeds.
2. kms:CreateKey in a DIFFERENT Region (e.g. us-east-1 when the caller is in us-west-2).
3. ecr:CreateRepository with encryptionConfiguration={encryptionType: "KMS", kmsKey: <the foreign-Region key ARN>}.
4. Inspect the ClientError: Code is KmsException (correct) but Message opens with "com.amazonaws.services.kms.model.NotFoundException: " instead of "The Amazon ECR action failed due to a KMS exception: ".
