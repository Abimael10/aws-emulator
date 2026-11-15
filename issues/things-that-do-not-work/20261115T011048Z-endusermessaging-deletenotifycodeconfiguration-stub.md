Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: endusermessaging.notify-code-configuration-deletion-protection-blocks-then-allows-delete
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging.notify-code-configuration-deletion-protection-blocks-then-allows-delete|run#8|AssertionError|assert code == "ConflictException", f"expected ConflictException while protected, got {code}: {message!r}"
Issue-Commit: 2f00469c6b1c115e8270b5c2778d03323e9ec131

# Implement endusermessaging DeleteNotifyCodeConfiguration: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `2f00469c6b1c115e8270b5c2778d03323e9ec131`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteNotifyCodeConfiguration

## Description
`DeleteNotifyCodeConfiguration` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/delete_notify_code_configuration.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `DeleteNotifyCodeConfiguration` call, so each one unblocks only when `DeleteNotifyCodeConfiguration` works: `endusermessaging.notify-code-configuration-deletion-protection-blocks-then-allows-delete`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/delete_notify_code_configuration.rs`. Replaying the corpus, the emulator refused the `DeleteNotifyCodeConfiguration` call: File "run.py", line 445, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_endusermessaging_notify_code_configuration_deletion_protection_blocks_then_allows_delete.py", line 60, in run
    assert code == "ConflictException", f"expected ConflictException while protected, got {code}: {message!r}"
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: expected ConflictException while protected, got NotImplemented: 'operation DeleteNotifyCodeConfiguration is scaffolded but not implemented in this emulator'

## Steps to Reproduce
Boot the endusermessaging emulator
Call DeleteNotifyCodeConfiguration and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
