Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue__s3tables.update-catalog-unknown-catalog-id-rejected
Issue-Key: glue__s3tables.update-catalog-unknown-catalog-id-rejected|run#13|AssertionError|assert code == "EntityNotFoundException", f"expected EntityNotFoundException, got {code}: {message!r}"
Issue-Commit: cea05121548b3a16dc11942b4ef340d67b81162b

# Implement glue UpdateCatalog: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `cea05121548b3a16dc11942b4ef340d67b81162b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateCatalog

## Description
`UpdateCatalog` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-glue-awsglue/src/ops/update_catalog.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `UpdateCatalog` call, so each one unblocks only when `UpdateCatalog` works: `glue__s3tables.update-catalog-unknown-catalog-id-rejected`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-glue-awsglue/src/ops/update_catalog.rs`. Replaying the corpus, the emulator refused the `UpdateCatalog` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_glue__s3tables_update_catalog_unknown_catalog_id_rejected.py", line 63, in run
    assert code == "EntityNotFoundException", f"expected EntityNotFoundException, got {code}: {message!r}"
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: expected EntityNotFoundException, got NotImplemented: 'operation UpdateCatalog is scaffolded but not implemented in this emulator'

## Steps to Reproduce
Boot the glue emulator
Call UpdateCatalog and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
