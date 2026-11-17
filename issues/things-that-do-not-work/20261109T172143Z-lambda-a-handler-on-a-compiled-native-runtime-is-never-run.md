Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__lambda.go-handler-ddb-stream-new-and-old-image-delivered-to-the-handler
Issue-Key: dynamodb__lambda.go-handler-ddb-stream-new-and-old-image-delivered-to-the-handler|_poll#20|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Commit: 545841f07feb57b92e392d55db412e521f655a7b

# lambda: a handler on a compiled native runtime (Go on `provided.al2`) is never run, so no stream record reaches it

> _Note: this cause was filed as cause 4 of 4 of
> `20261109T172142Z-dynamodb-lambda-4-net-new-causes-esm-maximumrecordageinseconds.md`. The other
> three causes of that bundle are resolved (the event source mapping's record age and its tumbling
> windows now hold, and `GetFunction` already publishes a `Code.Location` on this emulator's own
> endpoint), and this one is carried forward here because it is a CAPABILITY of its own rather than
> a defect in the mapping's delivery path: it needs an execution substrate this emulator does not
> have. It stays open so no scan reports it as working._

## Operation
Invoke

## Description
AWS: a function on the `provided.al2`/`provided.al2023` runtime receives the records an event source
mapping reads (INSERT, MODIFY, REMOVE with their `NewImage`/`OldImage`) exactly as a Python handler
does — the deployment package ships a compiled `bootstrap` program, Lambda runs it, and that program
drives the runtime API loop.

Emulator: an invocation of such a function runs nothing and is answered with the modelled
`InvalidRuntimeException` ("The runtime … is not supported"), so the mapped function's audit log
never shows the records. The engine for a package is resolved per LANGUAGE this emulator can
interpret in-process — `state/execution.rs::engine_for` resolves the `python…` and `nodejs…` runtime
families and `src/ops/invoke.rs::unrunnable` answers the modelled error for every other — and a
compiled program image is not one of them.

Why: any non-Python, non-node consumer of a stream is dead: the mapping reports healthy and the
handler is never fed.

What this needs, and why it is not a fix in the mapping's poller: the delivery path is
runtime-agnostic (the poller invokes the function through Lambda's own `Invoke`), so what is missing
is the ability to RUN a native program image — the Go runtime's own scheduler and the `clone`,
`futex`, `mmap` and `epoll` calls it makes at startup, plus the runtime API loop that program then
drives. In-process that is a whole-program native emulator rather than the single-call shared-object
path this emulator's execution guidance scopes; out of process it needs a process or container
runtime the platform does not provide. Whoever picks this up should expect to exit it with
`mark_wont_fix(platform_blocker)` citing
`services/service-com-amazonaws-lambda-awsgirapiservice/src/state/execution.rs` unless such a
runtime has become available.

## Evidence
TimeoutError: INSERT, MODIFY and REMOVE records for the item reach the handler's audit log: the run
budget ended first.

## Steps to Reproduce
Deploy a Go-runtime handler (`provided.al2`, a compiled `bootstrap` in the package) behind a
DynamoDB-stream event source mapping; PutItem/UpdateItem/DeleteItem on the table and look for
INSERT/MODIFY/REMOVE in the handler's log.
