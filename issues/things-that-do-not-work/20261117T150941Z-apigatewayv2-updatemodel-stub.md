Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2.update-model-invalid-schema-rejected
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2.update-model-invalid-schema-rejected|run#7|AssertionError|assert code == "BadRequestException", (
Issue-Commit: 464916eef2b3c982c1baee9561b862edbea1b318

# Implement apigatewayv2 UpdateModel: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `464916eef2b3c982c1baee9561b862edbea1b318`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateModel

## Description
`UpdateModel` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/ops/update_model.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `UpdateModel` call, so each one unblocks only when `UpdateModel` works: `apigatewayv2.update-model-invalid-schema-rejected`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/ops/update_model.rs`. Replaying the corpus, the emulator refused the `UpdateModel` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_apigatewayv2_update_model_invalid_schema_rejected.py", line 51, in run
    assert code == "BadRequestException", (
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: UpdateModel with a malformed schema: expected BadRequestException, got NotImplemented: 'operation UpdateModel is scaffolded but not implemented in this emulator'

## Steps to Reproduce
Boot the apigatewayv2 emulator
Call UpdateModel and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
