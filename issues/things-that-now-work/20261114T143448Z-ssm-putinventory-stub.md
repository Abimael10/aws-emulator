Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.delete-inventory-invalid-parameters-rejected ssm.delete-inventory-then-list-deletions ssm.inventory-custom-schema-numeric-precision-preserved ssm.inventory-custom-schema-server-side-validation ssm.inventory-deletion-lifecycle ssm.inventory-empty-vs-absent-attribute-preserved ssm.put-inventory-malformed-instance-id-blocked ssm.put-inventory-numeric-precision-preserved ssm.put-inventory-then-delete-inventory-lifecycle
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|PutInventory|NotImplemented
Issue-Commit: 09a4de48f58372094bfcb9a0bb2c4d192074b72e

# Implement ssm PutInventory: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `09a4de48f58372094bfcb9a0bb2c4d192074b72e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutInventory

## Description
`PutInventory` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-ssm-amazonssm/src/ops/put_inventory.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

9 recorded unit(s) fail on their `PutInventory` call, so each one unblocks only when `PutInventory` works: `ssm.delete-inventory-invalid-parameters-rejected`, `ssm.delete-inventory-then-list-deletions`, `ssm.inventory-custom-schema-numeric-precision-preserved`, `ssm.inventory-custom-schema-server-side-validation`, `ssm.inventory-deletion-lifecycle`, `ssm.inventory-empty-vs-absent-attribute-preserved`, `ssm.put-inventory-malformed-instance-id-blocked`, `ssm.put-inventory-numeric-precision-preserved`, `ssm.put-inventory-then-delete-inventory-lifecycle`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-ssm-amazonssm/src/ops/put_inventory.rs`. Replaying the corpus, the emulator refused the `PutInventory` call: File "context.py", line 123, in wrapper
    return func(*args, **kwargs)
           ^^^^^^^^^^^^^^^^^^^^^
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the PutInventory operation: operation PutInventory is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the ssm emulator
Call PutInventory and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
