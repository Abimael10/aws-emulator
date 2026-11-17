Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms__medical-imaging.create-datastore-with-nonexistent-kms-key-rejected
Issue-Key: kms__medical-imaging.create-datastore-with-nonexistent-kms-key-rejected|run#16|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# kms__medical-imaging: CreateDatastore accepts a kmsKeyArn naming a nonexistent key instead of refusing with ValidationException

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDatastore

## Description
Cause 1 of 1: the data store's kmsKeyArn is not checked against KMS.
  AWS: recorded behavior only — CreateDatastore with a well-formed kmsKeyArn whose key KMS reports NotFoundException is refused with ValidationException and the exact message 'The provided KMS key for datastore cannot be located.' (fixed text, no variable parts). No data store is created: ListDatastores never shows the requested datastoreName. The identical call with no kmsKeyArn succeeds and reaches ACTIVE.
  Emulator: the call returns 200 with {'datastoreId': '<id>', 'datastoreStatus': 'CREATING'}, creating a data store under a key that does not exist.
  Why: a client relying on the create call to validate its CMK gets a data store that in AWS could never be created, and the ValidationException its error path handles never arrives; the invalid key is only discovered (if ever) on later image I/O.
  Fix: the CreateDatastore handler's kmsKeyArn validation — resolve the key in KMS state and refuse before creating the data store.
  Traces: kms__medical-imaging.create-datastore-with-nonexistent-kms-key-rejected.

## Evidence
create-datastore-with-nonexistent-kms-key-rejected — AssertionError: expected the call to be refused, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'datastoreId': '975835de1c9756cebdf14e3acdd9906f', 'datastoreStatus': 'CREATING'}. Expected ('ValidationException', 'The provided KMS key for datastore cannot be located.') and no data store with that name in ListDatastores.

## Steps to Reproduce
1. Build a nonexistent key ARN arn:aws:kms:<region>:<account>:key/<random uuid> and confirm kms.describe_key on it raises NotFoundException.
2. medical-imaging create_datastore(datastoreName=<name>, clientToken=<tok>, kmsKeyArn=<that arn>) -> expect ValidationException 'The provided KMS key for datastore cannot be located.' and no data store named <name> in ListDatastores.
3. The same call without kmsKeyArn -> CREATING then ACTIVE (control).
