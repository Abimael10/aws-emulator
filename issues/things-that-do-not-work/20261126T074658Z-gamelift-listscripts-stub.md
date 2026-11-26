Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__gamelift__iam__s3.deploy-gamelift-script-missing-nodejs-version-rejected-and-rolled-back cloudformation__gamelift__iam__s3.deploy-gamelift-script-round-trip-and-delete
Issue-Key: gamelift|ListScripts|NotImplemented
Issue-Commit: 3fa8c42c1df735de25d8beda9af18b43438c14a4

# Implement gamelift ListScripts: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `3fa8c42c1df735de25d8beda9af18b43438c14a4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListScripts

## Description
`ListScripts` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-gamelift-gamelift/src/ops/list_scripts.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

2 recorded unit(s) fail on their `ListScripts` call, so each one unblocks only when `ListScripts` works: `cloudformation__gamelift__iam__s3.deploy-gamelift-script-missing-nodejs-version-rejected-and-rolled-back`, `cloudformation__gamelift__iam__s3.deploy-gamelift-script-round-trip-and-delete`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-gamelift-gamelift/src/ops/list_scripts.rs`. Replaying the corpus, the emulator refused the `ListScripts` call: File "context.py", line 123, in wrapper
    return func(*args, **kwargs)
           ^^^^^^^^^^^^^^^^^^^^^
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the ListScripts operation: operation ListScripts is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the gamelift emulator
Call ListScripts and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
