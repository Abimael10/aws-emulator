Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eventbridgev2.revoke-resource-marks-subscriber-terminal-and-outlives-bus-deletion
Issue-Shape-Id: com.amazonaws.eventbridgev2#AWSEventsV2
Issue-Key: eventbridgev2|UpdateSubscriber|NotImplemented
Issue-Commit: 366da4842fbf9ceddd9497ec8aca896c38ee89fa

# Implement eventbridgev2 UpdateSubscriber: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `366da4842fbf9ceddd9497ec8aca896c38ee89fa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateSubscriber

## Description
`UpdateSubscriber` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-eventbridgev2-awseventsv2/src/ops/update_subscriber.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `UpdateSubscriber` call, so each one unblocks only when `UpdateSubscriber` works: `eventbridgev2.revoke-resource-marks-subscriber-terminal-and-outlives-bus-deletion`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-eventbridgev2-awseventsv2/src/ops/update_subscriber.rs`. Replaying the corpus, the emulator refused the `UpdateSubscriber` call: File "check_eventbridgev2_revoke_resource_marks_subscriber_terminal_and_outlives_bus_deletion.py", line 114, in run
    expect_error(
  File "check_eventbridgev2_revoke_resource_marks_subscriber_terminal_and_outlives_bus_deletion.py", line 67, in expect_error
    assert got == (code, message), f"expected {(code, message)!r}, got {got!r}"
           ^^^^^^^^^^^^^^^^^^^^^^
AssertionError: expected ('InvalidStateException', 'Subscriber is revoked and cannot be updated'), got ('NotImplemented', 'operation UpdateSubscriber is scaffolded but not implemented in this emulator')

## Steps to Reproduce
Boot the eventbridgev2 emulator
Call UpdateSubscriber and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
