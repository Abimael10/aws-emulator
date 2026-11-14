Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena.athena-concurrent-named-query-creation-unique-ids athena.batch-get-named-query-partial-unprocessed athena.op-contract-batchGetNamedQuery
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena|BatchGetNamedQuery|NotImplemented
Issue-Commit: a6c2e348e539d87c7f8cd286303d9f9234b5e1b8

# Implement athena BatchGetNamedQuery: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `a6c2e348e539d87c7f8cd286303d9f9234b5e1b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetNamedQuery

## Description
`BatchGetNamedQuery` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-athena-amazonathena/src/ops/batch_get_named_query.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

3 recorded unit(s) fail on their `BatchGetNamedQuery` call, so each one unblocks only when `BatchGetNamedQuery` works: `athena.athena-concurrent-named-query-creation-unique-ids`, `athena.batch-get-named-query-partial-unprocessed`, `athena.op-contract-batchGetNamedQuery`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-athena-amazonathena/src/ops/batch_get_named_query.rs`. Replaying the corpus, the emulator refused the `BatchGetNamedQuery` call: File "context.py", line 123, in wrapper
    return func(*args, **kwargs)
           ^^^^^^^^^^^^^^^^^^^^^
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the BatchGetNamedQuery operation: operation BatchGetNamedQuery is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the athena emulator
Call BatchGetNamedQuery and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
