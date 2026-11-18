Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sqs.esm-filter-criteria-non-matching-not-invoked
Issue-Key: lambda__sqs.esm-filter-criteria-non-matching-not-invoked|_collect#14|Timeout|resp = sqs.receive_message(
Issue-Commit: 2b63d043efdd2fa9855b978e8c81153e1c6b6b87

# lambda__sqs: the filter-criteria check reaches no verdict inside the 60-second budget, because the first invocation of a function still builds its execution environment

> _Note: this cause was identified against the code base at commit `2b63d043efdd2fa9855b978e8c81153e1c6b6b87`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

> _Provenance: this was cause 3 of 3 of `20261106T134622Z-lambda-sqs-a-number-message-attribute-is-not.md`. Causes 1 and 2 of that issue (a `Number` message attribute delivered uncanonicalised, and an in-function `SendMessage` the execution role does not permit being accepted) are fixed and that report now sits under `issues/things-that-now-work/`; this cause is unfixed and is re-filed here so it is not recorded as working. Its `Issue-Key` and trace id moved with it._

## Operation
CreateEventSourceMapping (the SQS event-source-mapping data plane), and the Lambda execution environment behind its first invocation

## Description
The FILTERING this check is named for is correct, and is pinned end to end by
`services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/sqs_event_source_filter_criteria.rs`:
a message at least one `FilterCriteria` pattern matches is handed to the function, one none of
them matches is not handed over at all, and either way the message leaves the queue. What this
check cannot do is reach its assertions inside the replay harness's 60-second budget.

Measured, by instrumenting the emulator and replaying the check (seconds from its first call):
the queues and the function are created at t=0 and the function reports `Active` at t=2; the
mapping is created at t=2 and reports `Enabled` at t=7; the check sends its two messages (one
non-matching, one matching) at t=7; ONE invocation is made at t=7 — the non-matching message is
deleted from the queue without any invocation — the handler's forward of the matching record
lands at t=8, and the check's own `ReceiveMessage` takes it at t=9. Everything the emulator owes
is done 9 seconds in. The check then polls the now-empty sink queue for the remaining ~50
seconds of its own collection window and is killed at 60 s, about a second over.

That second is accounted for, and the parts are known:

* ~1.16 s is the PYTHON EXECUTION ENVIRONMENT, which this emulator builds on a function's FIRST
  invocation. Measured in this crate: 1.183 s for the first invocation of a function, 20 ms for
  the next. AWS builds that environment while the function is `Pending` — it is what `Pending`
  IS — so in AWS the first delivery does not pay for it.
* the rest is what a polling client waits out for the two modelled transient states: both
  `CreateFunction` (`Pending` -> `Active`) and `CreateEventSourceMapping` (`Creating` ->
  `Enabled`) settle after 500 ms, and because this check reads each state immediately after the
  call that starts it and then sleeps, each costs it a whole poll round — 2 s and 5 s here.

## The ONE blocker, for whoever picks this up
The environment build has to come OFF the first invocation's path, which means PRE-PROVISIONED
execution environments: an environment is a thread that builds a RustPython interpreter,
compiles this emulator's runtime source into it and imports the handler's module
(`services/service-com-amazonaws-lambda-awsgirapiservice/src/state/execution/python.rs`,
`Prepared::start`), and it is built today by the invocation that finds none standing
(`services/service-com-amazonaws-lambda-awsgirapiservice/src/state/execution/warm_pool.rs`,
`serve`). Fixing this cause means splitting that build in two — the part that is the same for
every environment, stood up while the function is `Pending`, and the per-function part applied
when an invocation adopts it — and parking the result in the warm pool so the first invocation
is the handler's own run. That is a core execution-engine change in two large files, not an
event-source-mapping one, which is why it is filed on its own.

What must NOT be done for it: answering a terminal `State` from `CreateFunction` or
`CreateEventSourceMapping`. Zeroing either 500 ms span alone does make all three checks of the
parent issue pass, which is how the budget above was measured — but it deletes transient states
AWS itself reports and this crate's suites pin (`tests/all/event_source_mapping_deletion.rs`
reads `Creating` straight after the create), so it buys the verdict by making the emulator less
faithful.

Traces: lambda__sqs.esm-filter-criteria-non-matching-not-invoked.

## Evidence
Expected: a verdict — the non-matching message never invokes the function while the matching one
does.
Actual: `Error: Check timed out after 60.0s`, with the emulator's own work complete at t=9 s and
the check's remaining time spent in its own collection loop over an empty queue.

Already done, so a fixer does not repeat it: the queue mapping's poller no longer waits out its
own cadence before it sees a message — the Amazon SQS store signals every send and the poll loop
waits on that signal, bounded by the same interval
(`services/service-com-amazonaws-sqs-amazonsqs/src/state/message_arrivals.rs`,
`services/service-com-amazonaws-lambda-awsgirapiservice/src/state/sqs_poller.rs`). That removed
up to 500 ms per delivery and was not enough on its own.

## Steps to Reproduce
1. Create a queue, a sink queue and a python function whose handler forwards every record it is
   handed to the sink.
2. `CreateEventSourceMapping` on the queue with `FilterCriteria` admitting one body shape; poll
   `GetEventSourceMapping` until `State` is `Enabled`.
3. Send a non-matching message and a matching one; collect from the sink.
4. Observe the correct outcome (only the matching record arrives, both messages leave the queue)
   about 2 s after the sends, with the first invocation's ~1.2 s of environment build inside it.
5. Replay lambda__sqs.esm-filter-criteria-non-matching-not-invoked: the check is killed at 60 s.
