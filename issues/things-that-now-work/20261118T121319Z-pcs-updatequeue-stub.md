Issue-Severity: HIGH
Issue-Gap-Class: stub
Issue-Commit: ee24c053d2f664e1aece2bc382274e6b27f60b27

# Implement pcs UpdateQueue: unimplemented scaffold stub

_Filed: 2026-11-18T12:13:19Z_
_Operation: UpdateQueue | Class: stub_

> _Note: this issue was identified against the code base at commit `ee24c053d2f664e1aece2bc382274e6b27f60b27`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateQueue` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/update_queue.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

## Evidence

The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/update_queue.rs`.

## Steps to Reproduce

1. Boot the pcs emulator
2. Call UpdateQueue and observe HTTP 501 with no state persisted
