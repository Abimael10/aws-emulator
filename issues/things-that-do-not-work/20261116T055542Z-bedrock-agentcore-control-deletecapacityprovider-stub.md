Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore-control.delete-capacity-provider-in-use-by-runtime-blocked
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control.delete-capacity-provider-in-use-by-runtime-blocked|run#19|AssertionError|assert code == "ValidationException", f"expected ValidationException, got {code!r}: {message}"
Issue-Commit: 0c5548d176139ac2898f8496ebd5636c0a57c822

# Implement bedrock-agentcore-control DeleteCapacityProvider: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `0c5548d176139ac2898f8496ebd5636c0a57c822`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteCapacityProvider

## Description
`DeleteCapacityProvider` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/delete_capacity_provider.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `DeleteCapacityProvider` call, so each one unblocks only when `DeleteCapacityProvider` works: `bedrock-agentcore-control.delete-capacity-provider-in-use-by-runtime-blocked`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/delete_capacity_provider.rs`. Replaying the corpus, the emulator refused the `DeleteCapacityProvider` call: File "run.py", line 445, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_bedrock_agentcore_control_delete_capacity_provider_in_use_by_runtime_blocked.py", line 74, in run
    assert code == "ValidationException", f"expected ValidationException, got {code!r}: {message}"
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: expected ValidationException, got 'NotImplemented': operation DeleteCapacityProvider is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the bedrock-agentcore-control emulator
Call DeleteCapacityProvider and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
