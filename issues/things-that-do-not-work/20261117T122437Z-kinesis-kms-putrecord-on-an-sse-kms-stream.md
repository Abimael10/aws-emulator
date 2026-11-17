Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesis__kms.put-record-on-sse-kms-stream-denied-without-grant-then-decrypts
Issue-Key: kinesis__kms.put-record-on-sse-kms-stream-denied-without-grant-then-decrypts|_refusal#16|AssertionError|raise AssertionError(f"expected this call to be refused, but it succeeded: {result!r}")
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# kinesis__kms: PutRecord on an SSE-KMS stream succeeds for a role with no kms:GenerateDataKey permission instead of KMSAccessDeniedException

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRecord

## Description
Cause 1 of 1: Kinesis does not evaluate the caller's KMS permissions when writing to a stream encrypted under a customer managed key.
  AWS: recorded behavior only — once StartStreamEncryption puts a stream under a CMK, a PutRecord by a principal whose policies allow only kinesis:PutRecord/GetRecords is refused with KMSAccessDeniedException, and the message is KMS's authorization wording: 'User: <callerArn> is not authorized to perform: kms:GenerateDataKey on resource: <keyArn> because no identity-based policy allows the kms:GenerateDataKey action' — fixed text with the assumed-role ARN of the caller and the key ARN as variables. Nothing is appended to the shard. Granting that principal kms:GenerateDataKey and kms:Decrypt on the key makes the identical call succeed and the record readable.
  Emulator: PutRecord SUCCEEDS and returns a sequence number, so the un-permissioned write lands.
  Why: a client (or a security test) that depends on KMS enforcing stream encryption sees writes accepted that AWS refuses — the permission boundary it is validating does not exist, and the KMSAccessDeniedException path is never exercised.
  Fix: the Kinesis PutRecord/PutRecords data path must authorize the caller against the stream's KMS key (kms:GenerateDataKey) via KMS policy evaluation and map the denial to KMSAccessDeniedException with KMS's message.
  Traces: kinesis__kms.put-record-on-sse-kms-stream-denied-without-grant-then-decrypts.

## Evidence
put-record-on-sse-kms-stream-denied-without-grant-then-decrypts — AssertionError: expected this call to be refused, but it succeeded: '491794917328784000000000000000001' (a sequence number). Expected ('KMSAccessDeniedException', 'User: arn:aws:sts::<acct>:assumed-role/<role>/<session> is not authorized to perform: kms:GenerateDataKey on resource: <key arn> because no identity-based policy allows the kms:GenerateDataKey action').

## Steps to Reproduce
1. Create a stream and an IAM role whose policy allows only kinesis:PutRecord/GetRecords; assume it via STS.
2. Create a CMK and kinesis.start_stream_encryption(StreamName=<s>, EncryptionType='KMS', KeyId=<key arn>).
3. With the assumed-role client call kinesis.put_record(...) -> expect KMSAccessDeniedException with the message above and no new record visible to an admin read.
4. Add kms:GenerateDataKey and kms:Decrypt on that key to the role and repeat -> expect success and the record readable.
