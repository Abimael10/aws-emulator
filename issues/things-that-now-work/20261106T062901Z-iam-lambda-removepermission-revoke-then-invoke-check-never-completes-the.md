Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda.xsvc-iam-2-lambda-remove-permission-revokes-invoke-denied
Issue-Key: iam__lambda.xsvc-iam-2-lambda-remove-permission-revokes-invoke-denied|call#16|Timeout|resp = client.invoke(FunctionName=fn, Payload=json.dumps({"marker": marker}).encode())
Issue-Commit: 5b7d66e77a060af6cd000fd4d7e09192c88c4502

# RemovePermission revoke-then-invoke check never completes: the invoke after the resource-policy statement is removed hangs past 60s

> _Note: this issue was identified against the code base at commit `5b7d66e77a060af6cd000fd4d7e09192c88c4502`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RemovePermission

## Description
Cause 1 of 1: after RemovePermission deletes the statement that authorised a caller, the follow-up invoke neither succeeds nor is refused inside the check's deadline.
AWS: RemovePermission deletes the named statement from the function's resource policy, and the next Invoke by the de-authorised principal is refused promptly with AccessDeniedException (403); the positive control (before the removal) returns 200.
Emulator: "Error: Check timed out after 60.0s" - the sequence add-permission -> invoke -> remove-permission -> invoke never settles, so neither the grant nor the revocation can be observed. Recorded behavior only: the replay records no response for the hanging call.
Why: a client cannot verify that revoking an invoke grant takes effect - the most basic authorization property of a function's resource policy - and a hang is indistinguishable from an outage.
Fix: the RemovePermission handler and the resource-policy authorization path of the Invoke handler (the revoked state must produce a prompt refusal).
Traces: iam__lambda.xsvc-iam-2-lambda-remove-permission-revokes-invoke-denied.

## Evidence
Expected (AWS): Invoke succeeds while the AddPermission statement exists; after RemovePermission the same Invoke is refused with AccessDeniedException (HTTP 403) - both answers return promptly.
Actual (emulator): Error: Check timed out after 60.0s (no answer to the post-revocation invoke).

## Steps to Reproduce
1. Replay the behavioral suite and re-run iam__lambda.xsvc-iam-2-lambda-remove-permission-revokes-invoke-denied.
2. CreateFunction, AddPermission for a principal, Invoke as that principal, RemovePermission(StatementId=...), then Invoke again as the same principal.

## Resolution
Resolved: the recorded sequence replays clean, and the rule it reads is now pinned by
`core/router/tests/all/lambda_invoke_grant_revocation_scope.rs` (a removal takes out exactly the
statement its `StatementId` names: that principal is refused 403 AccessDeniedException on the next
call, every other statement of the same document still authorizes its own principal, GetPolicy
reports exactly the statements that still stand, and a statement written again under the same id
authorizes again).

No handler needed changing: the code base drifted after the commit this report was filed against.
The invocation decision now lives in one place,
`services/service-com-amazonaws-lambda-awsgirapiservice/src/state/invoke_authorization.rs`, and is
asked before anything is resolved, queued, metered or run by every invoking entry point
(`src/ops/invoke.rs:428`, `src/ops/invoke_async.rs:266`,
`src/ops/invoke_with_response_stream.rs:290`), so a revoked caller is refused without the function
being reached; `src/ops/remove_permission.rs:268-291` retains all but the named statement and drops
the document only when its last statement goes.

No call on this path waits without a bound: a dry run answers above any run at all
(`src/ops/invoke.rs:573`), an admitted run is bounded by the function's own `Timeout` and ends as
the modelled function error (`src/state/execution.rs:1103`, `:1974`), an invocation that finds no
capacity is answered the modelled throttle rather than made to wait
(`src/state/execution_slot.rs:203-238`, surfaced at `src/ops/invoke.rs:843`), and the pool of
inflated archives is a bounded semaphore (`src/state/execution.rs:1716-1721`). The 60 s in the
report above was the check's own convergence loop rather than one hung request: the sibling report
for the same trace records that each call answered promptly, with the successful invocation instead
of the refusal (`issues/things-that-now-work/20261105T085348Z-iam-lambda-2-net-new-root-causes-invokefunction.md`).
