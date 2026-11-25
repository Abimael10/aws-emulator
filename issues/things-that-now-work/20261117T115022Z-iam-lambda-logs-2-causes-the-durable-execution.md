Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__logs.durable-chained-invoke-of-a-never-created-function-fails iam__lambda__logs.durable-chained-invoke-payload-matches-target-log-and-history iam__lambda__logs.put-concurrency-refused-on-unknown-function-preserves-real-reservation
Issue-Key: iam__lambda__logs.durable-chained-invoke-of-a-never-created-function-fails|run#13|AssertionError|assert refused_error.get("ErrorMessage") == expected_message, (
Issue-Key: iam__lambda__logs.durable-chained-invoke-payload-matches-target-log-and-history|run#18|AssertionError|raise AssertionError(f"durable execution ended {final['Status']!r} unexpectedly: {final.get('Error')!r}")
Issue-Key: lambda|PutFunctionConcurrency|ResourceNotFoundException
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# iam__lambda__logs: 2 causes — the durable-execution context exposes no invoke() (chained invoke dies with AttributeError), and PutFunctionConcurrency's not-found message omits the :$LATEST qualifier

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutFunctionConcurrency

## Description
Two net-new root causes in the Lambda surface.

Cause 1 of 2: a durable execution's chained invoke is impossible — the runtime's context object has no invoke member.
AWS: recorded behavior only — in a durable execution the handler's context exposes the durable primitives, including an invoke of another function, and a chained invoke of a function that was never created makes the EXECUTION fail with the service's own function-not-found error, surfaced as the execution's Error with ErrorMessage 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>:$LATEST' (fixed prefix 'Function not found: ' plus the fully qualified ARN of the requested target, including the :$LATEST qualifier). A chained invoke of a function that DOES exist must deliver the caller's payload and appear in the target's log and the execution history.
Emulator: every chained invoke dies inside the runtime with ErrorMessage "'LambdaContext' object has no attribute 'invoke'" and ErrorType AttributeError — the execution ends FAILED for a reason that has nothing to do with the scenario, both for the never-created target (where the absent-function error is required) and for the valid target (where the invoke must succeed).
Why: durable chained invocation does not exist at all; a client gets a runtime AttributeError instead of either a successful chain or AWS's function-not-found error, so neither the payload/log/history contract nor the absent-target refusal can be relied on.
Fix: the Lambda execution environment's durable-execution context object (the invoke primitive) and the error mapping that turns an unresolvable target into the service's Function not found error on the execution.
Failing checks: iam__lambda__logs.durable-chained-invoke-of-a-never-created-function-fails, iam__lambda__logs.durable-chained-invoke-payload-matches-target-log-and-history

Cause 2 of 2: PutFunctionConcurrency's ResourceNotFoundException message drops the version qualifier.
AWS: Smithy lambda PutFunctionConcurrency models ResourceNotFoundException (client, HTTP 404) and its FunctionName accepts a name, partial ARN or full ARN optionally qualified; the refusal for an unknown function reads 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>:$LATEST' — AWS resolves the unqualified request to the $LATEST qualifier and echoes the FULL qualified ARN after the fixed prefix.
Emulator: the same call is correctly refused ResourceNotFoundException but the message stops at the unqualified ARN: 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>' (no ':$LATEST').
Why: the resolved qualifier is the part of the message that tells a caller WHICH version the service looked for; clients and tests that compare the message (or parse the ARN out of it) see a different string, and the companion assertion that the live function's real reservation is untouched is never reached.
Fix: the PutFunctionConcurrency handler's not-found refusal — format the message from the fully qualified (version-resolved) function ARN.
Failing check: iam__lambda__logs.put-concurrency-refused-on-unknown-function-preserves-real-reservation

## Evidence
Cause 1: [mismatch] iam__lambda__logs.durable-chained-invoke-of-a-never-created-function-fails — AssertionError: expected the execution's error to name the never-created target as 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>:$LATEST', got "'LambdaContext' object has no attribute 'invoke'". [mismatch] iam__lambda__logs.durable-chained-invoke-payload-matches-target-log-and-history — AssertionError: durable execution ended 'FAILED' unexpectedly: {'ErrorMessage': "'LambdaContext' object has no attribute 'invoke'", 'ErrorType': 'AttributeError'}.

Cause 2: [mismatch] iam__lambda__logs.put-concurrency-refused-on-unknown-function-preserves-real-reservation — AssertionError: expected error message 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>:$LATEST', got 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>' (the region/account/name are request-derived; the missing ':$LATEST' qualifier is the divergence).

## Steps to Reproduce
1. Start a durable execution whose handler calls context.invoke(...) against (a) a function that was never created and (b) a real target function; read the execution's Status/Error, the target's log and the execution history.
2. Call PutFunctionConcurrency(FunctionName=<never-created name>, ReservedConcurrentExecutions=1) and compare the ResourceNotFoundException message with AWS's qualified-ARN form; then confirm an existing function's reservation is unchanged.
