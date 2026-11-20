Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.daemon-create-active-lifecycle ecs.daemon-describe-after-delete-not-found ecs.daemon-lifecycle-create-to-delete
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|DescribeDaemon|NotImplemented
Issue-Commit: b64c3d45927e2833db57dde0eaeae2a33a5b497f

# Implement ecs DescribeDaemon: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `b64c3d45927e2833db57dde0eaeae2a33a5b497f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

> _Resolution: `DescribeDaemon` is IMPLEMENTED and was already implemented when this report was picked up — the code base had drifted past the commit it was identified against. `services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/describe_daemon.rs` carries no `//! SCAFFOLDED, NOT IMPLEMENTED:` header line (that crate's stub census now lists only its `*_gateway*` operations), and its handler reads the service's store: it resolves `daemonArn` as the whole identifier it is, materialises what the clock already owes the read, and answers the stored daemon's `DaemonDetail` or `DaemonNotFoundException`. It landed with the managed-daemon surface of `20261104T062603Z-ecs-daemon-scheduling-never-progresses-and-createcapacityprovider-rejects.md`. All three recorded units above replay as passing and the key `ecs|DescribeDaemon|NotImplemented` is fixed, so nothing under `src/` needed changing. What this change adds is the test that pins the read's rule: `tests/all/daemon_scheduling.rs` (`a_daemon_read_reports_a_daemon_whose_cluster_holds_no_container_instance` and its three siblings, one per row) asserts, over rows spanning how much capacity the daemon's cluster holds, how many capacity providers it deploys onto and whether an update has moved it onto a second revision, that every member of the reported `DaemonDetail` is what the writes stored, that the `currentRevisions` per-provider counts add up to the daemon tasks `ListTasks`/`DescribeTasks` report, and that repeating the read changes nothing a later caller sees._

## Operation
DescribeDaemon

## Description
`DescribeDaemon` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/describe_daemon.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

3 recorded unit(s) fail on their `DescribeDaemon` call, so each one unblocks only when `DescribeDaemon` works: `ecs.daemon-create-active-lifecycle`, `ecs.daemon-describe-after-delete-not-found`, `ecs.daemon-lifecycle-create-to-delete`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/describe_daemon.rs`. Replaying the corpus, the emulator refused the `DescribeDaemon` call: File "context.py", line 123, in wrapper
    return func(*args, **kwargs)
           ^^^^^^^^^^^^^^^^^^^^^
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the DescribeDaemon operation: operation DescribeDaemon is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the ecs emulator
Call DescribeDaemon and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
