Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: globalaccelerator.endpoint-membership-survives-duplicate-add-and-absent-remove
Issue-Shape-Id: com.amazonaws.globalaccelerator#GlobalAccelerator_V20180706
Issue-Key: globalaccelerator.endpoint-membership-survives-duplicate-add-and-absent-remove|expect_error#12|AssertionError|assert got_code == code, f"expected {code}, got {got_code}: {got_message!r}"
Issue-Commit: 4ff3210f42e9a1596cde6d86bb6d66809acacb1e

# Implement globalaccelerator AddEndpoints: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `4ff3210f42e9a1596cde6d86bb6d66809acacb1e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AddEndpoints

## Description
`AddEndpoints` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-globalaccelerator-globalaccelerator-v20180706/src/ops/add_endpoints.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `AddEndpoints` call, so each one unblocks only when `AddEndpoints` works: `globalaccelerator.endpoint-membership-survives-duplicate-add-and-absent-remove`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-globalaccelerator-globalaccelerator-v20180706/src/ops/add_endpoints.rs`. Replaying the corpus, the emulator refused the `AddEndpoints` call: File "check_globalaccelerator_endpoint_membership_survives_duplicate_add_and_absent_remove.py", line 142, in run
    expect_error(
  File "check_globalaccelerator_endpoint_membership_survives_duplicate_add_and_absent_remove.py", line 30, in expect_error
    assert got_code == code, f"expected {code}, got {got_code}: {got_message!r}"
           ^^^^^^^^^^^^^^^^
AssertionError: expected InvalidArgumentException, got NotImplemented: 'operation AddEndpoints is scaffolded but not implemented in this emulator'

## Steps to Reproduce
Boot the globalaccelerator emulator
Call AddEndpoints and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
