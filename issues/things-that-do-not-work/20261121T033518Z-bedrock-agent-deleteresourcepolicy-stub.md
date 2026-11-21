Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agent.resource-policy-round-trip-enforced
Issue-Key: bedrock-agent.resource-policy-round-trip-enforced|run#17|AssertionError|assert delete_code == "ValidationException", (
Issue-Commit: ddedefe2ba7626fc434a3b439556791987e3d078

# Implement bedrock-agent DeleteResourcePolicy: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `ddedefe2ba7626fc434a3b439556791987e3d078`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteResourcePolicy

## Description
`DeleteResourcePolicy` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/src/ops/delete_resource_policy.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `DeleteResourcePolicy` call, so each one unblocks only when `DeleteResourcePolicy` works: `bedrock-agent.resource-policy-round-trip-enforced`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/src/ops/delete_resource_policy.rs`. Replaying the corpus, the emulator refused the `DeleteResourcePolicy` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_bedrock_agent_resource_policy_round_trip_enforced.py", line 79, in run
    assert delete_code == "ValidationException", (
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: expected DeleteResourcePolicy on a VECTOR-type knowledge base to be refused the same way, got NotImplemented: operation DeleteResourcePolicy is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the bedrock-agent emulator
Call DeleteResourcePolicy and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
