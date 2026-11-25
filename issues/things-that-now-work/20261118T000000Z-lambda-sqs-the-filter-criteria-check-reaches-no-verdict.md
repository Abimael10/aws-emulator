Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sqs.esm-filter-criteria-non-matching-not-invoked
Issue-Key: lambda__sqs.esm-filter-criteria-non-matching-not-invoked|_collect#14|Timeout|resp = sqs.receive_message(
Issue-Commit: 2b63d043efdd2fa9855b978e8c81153e1c6b6b87

# lambda__sqs: a function's execution environment is stood up while the function is provisioned, so its first invocation no longer builds an interpreter

> _Provenance: this was cause 3 of 3 of `20261106T134622Z-lambda-sqs-a-number-message-attribute-is-not.md`, re-filed on its own because it is a core execution-engine change rather than an event-source-mapping one._

## Operation
CreateEventSourceMapping (the SQS event-source-mapping data plane), and the Lambda execution
environment behind a function's first invocation.

## What was wrong, and what now happens
The FIRST invocation of a `python3.x` function paid for BUILDING its execution environment:
an invocation that found none standing created the interpreter, narrowed its native surface to
the allow-list, compiled and evaluated this emulator's whole runtime source, and only then ran
the function's own initialization. AWS does that work while the function is being provisioned —
"When you create a function, Lambda provisions an instance of the function and its supporting
resources" — so in AWS the first delivery does not pay for it.

It is now split along what it reads. Everything that reads no function's configuration — the
interpreter, the narrowing, the host surface and this emulator's own runtime source — is stood
up while the function is being PROVISIONED (its create, and the window an accepted code or
configuration update opens) and parked in the warm pool
(`services/service-com-amazonaws-lambda-awsgirapiservice/src/state/execution/python/blank.rs`,
`src/state/execution/warm_pool.rs`). The first invocation ADOPTS what is standing and applies
only its own function's half: what the environment is, the variables it runs with, how deep its
calls may nest, which release of the language its runtime names, and the import of the handler's
own module — the INIT phase, which is the `Init Duration` of a cold start. An invocation that
finds nothing standing still builds both halves itself, so no run observes anything new.

Measured in this crate (debug build): the first `Invoke` of a freshly created function took
1.47 s and now takes 0.18 s (the next one 18–21 ms either way); the first delivery of a queue
event source mapping landed 1.81 s after the send and now lands 0.54 s after it.

Pinned by `tests/all/pre_provisioned_execution_environments.rs` (a table over the `Runtime`
identifiers, both `Architectures` values, the variables an invocation runs with, and both
provisioning windows) and `tests/all/chain_latency_probe.rs` (the same rule through a queue
mapping's own delivery), beside the filtering this check is named for, which was and is correct
(`tests/all/sqs_event_source_filter_criteria.rs`).

## What this does NOT change: the recorded check still reaches no verdict
The replayed check `lambda__sqs.esm-filter-criteria-non-matching-not-invoked` still answers
`Error: Check timed out after 60.0s`, and that is not this environment build. Measured against
the replay:

* adding a deliberate 2-second delay to EVERY invocation — far more than the environment build
  ever cost — does not stop the check passing once one of its client poll rounds is removed:
  everything the emulator does AFTER the check's two sends is absorbed by the check's own
  ~51-second collection window, which is why removing the build from the first invocation's path
  does not move the check's wall-clock total;
* removing one poll round (by zeroing `ACTIVATION_DELAY`) buys 2 s and leaves 1.0–1.4 s of
  slack, so what the check is over its budget by is 0.6–1.0 s of its OWN fixed cost: the two
  client poll rounds (2 s and 5 s) it pays because it reads each state immediately after the
  call that starts it, plus that collection window.

That residual is larger than the emulator's entire pre-send contribution (two queue creates, the
function create, four state reads, the mapping create and two sends: ~0.1 s measured here), so no
faithful change closes it. The only lever left is answering a terminal `State` from
`CreateFunction` or `CreateEventSourceMapping`, which deletes transient states AWS itself reports
and this crate's suites pin (`tests/all/event_source_mapping_deletion.rs` reads `Creating`
straight after the create) — so it would buy the verdict by making the emulator less faithful,
and is deliberately not done.
