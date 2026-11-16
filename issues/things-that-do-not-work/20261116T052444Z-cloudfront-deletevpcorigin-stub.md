Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.op-contract-getVpcOrigin
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|DeleteVpcOrigin|NotImplemented
Issue-Commit: 73ee5db70c54544c27a351cd5968d4682ba0e095

# Implement cloudfront DeleteVpcOrigin: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `73ee5db70c54544c27a351cd5968d4682ba0e095`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteVpcOrigin

## Description
`DeleteVpcOrigin` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/ops/delete_vpc_origin.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `DeleteVpcOrigin` call, so each one unblocks only when `DeleteVpcOrigin` works: `cloudfront.op-contract-getVpcOrigin`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/ops/delete_vpc_origin.rs`. Replaying the corpus, the emulator refused the `DeleteVpcOrigin` call: File "check_cloudfront_op_contract_getVpcOrigin.py", line 54, in run
    _delete_with_retry(cf, vo_id, created["ETag"], deadline=time.monotonic() + 600.0, interval_s=10.0)
  File "check_cloudfront_op_contract_getVpcOrigin.py", line 135, in _delete_with_retry
    raise AssertionError(
AssertionError: expected DeleteVpcOrigin to eventually succeed once Deployed, got NotImplemented: 'operation DeleteVpcOrigin is scaffolded but not implemented in this emulator'

## Steps to Reproduce
Boot the cloudfront emulator
Call DeleteVpcOrigin and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
