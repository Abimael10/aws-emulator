Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue.connection-type-unknown-name-rejected
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue.connection-type-unknown-name-rejected|run#10|AssertionError|assert code == "EntityNotFoundException", f"expected EntityNotFoundException, got {code}: {message!r}"
Issue-Commit: 6daf257af7fbb3a766577fc65424a8d09ee1c660

# Implement glue DeleteConnectionType: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `6daf257af7fbb3a766577fc65424a8d09ee1c660`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteConnectionType

## Description
`DeleteConnectionType` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-glue-awsglue/src/ops/delete_connection_type.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `DeleteConnectionType` call, so each one unblocks only when `DeleteConnectionType` works: `glue.connection-type-unknown-name-rejected`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-glue-awsglue/src/ops/delete_connection_type.rs`. Replaying the corpus, the emulator refused the `DeleteConnectionType` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_glue_connection_type_unknown_name_rejected.py", line 46, in run
    assert code == "EntityNotFoundException", f"expected EntityNotFoundException, got {code}: {message!r}"
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: expected EntityNotFoundException, got NotImplemented: 'operation DeleteConnectionType is scaffolded but not implemented in this emulator'

## Steps to Reproduce
Boot the glue emulator
Call DeleteConnectionType and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
