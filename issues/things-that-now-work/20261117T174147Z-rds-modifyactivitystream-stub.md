Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.modify-activity-stream-resource-reference-rejected
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.modify-activity-stream-resource-reference-rejected|run#7|AssertionError|assert err_instance.get("Code") == "DBInstanceNotFound", (
Issue-Commit: cd00ce55523bfa99c95686848d372910b65cd86b

# Implement rds ModifyActivityStream: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `cd00ce55523bfa99c95686848d372910b65cd86b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyActivityStream

## Description
`ModifyActivityStream` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-rds-amazonrdsv19/src/ops/modify_activity_stream.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `ModifyActivityStream` call, so each one unblocks only when `ModifyActivityStream` works: `rds.modify-activity-stream-resource-reference-rejected`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-rds-amazonrdsv19/src/ops/modify_activity_stream.rs`. Replaying the corpus, the emulator refused the `ModifyActivityStream` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_rds_modify_activity_stream_resource_reference_rejected.py", line 41, in run
    assert err_instance.get("Code") == "DBInstanceNotFound", (
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: ModifyActivityStream on a nonexistent DB instance ARN: expected DBInstanceNotFound, got {'Type': 'Receiver', 'Code': 'NotImplemented', 'Message': 'operation ModifyActivityStream is scaffolded but not implemented in this emulator'}

## Steps to Reproduce
Boot the rds emulator
Call ModifyActivityStream and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
