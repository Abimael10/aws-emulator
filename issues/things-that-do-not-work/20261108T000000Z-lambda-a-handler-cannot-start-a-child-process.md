Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.deep-lambda-runtime-truth-layer-files-byte-exact-at-opt-and-opt-write-rejected
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.deep-lambda-runtime-truth-layer-files-byte-exact-at-opt-and-opt-write-rejected|_invoke#10|AssertionError|assert not resp.get("FunctionError"), (
Issue-Commit: 677b6e35c5226b67b9bfea5e554d250e3c084a9c

# lambda: a handler cannot start a child process, so an invocation that runs a program its layer ships fails (1 cause)

> _Note: this cause was separated out of
> `issues/things-that-now-work/20261107T091747Z-lambda-the-execution-environment-has-no-filesystem-or.md`,
> whose other causes are fixed. The trace above gets past the import and the `/opt` reads it
> was originally filed for and now fails on this one capability alone._

## Operation
Invoke

## Description
Cause 1 of 1: an invocation starts no child process, so `subprocess` and `os.popen` refuse
every call.

AWS: recorded behavior only; a managed runtime's execution environment is an ordinary Linux
sandbox with a shell and the usual utilities, and `/opt/bin` of a layer is on its `PATH`, so a
handler that runs a program — a utility of the sandbox, or an executable its own layer ships —
gets that program's output and exit status.

Emulator: `Invoke` answers 200 with `X-Amz-Function-Error: Unhandled` and payload errorType
`OSError`, errorMessage "[Errno 38] a Lambda function in this emulator starts no child
process", raised where the handler made the call
(`services/service-com-amazonaws-lambda-awsgirapiservice/src/state/execution/guest_process.rs`,
which supplies the `subprocess` module of this runtime and refuses the calls that would start
a child: `Popen`, `run`, `call`, `check_call`, `check_output`, `getstatusoutput`, `getoutput`,
and `os.popen` built on them).

Why: an invocation of this emulator runs IN the emulator's own process. There is no host
process to fork and no host program to execute — the platform provides no containers and no
external processes — so nothing here can execute a program the caller's package or layer
ships, and nothing invents an exit status, an output or a process id for work nothing ran.

Fix: an in-process answer for the programs a function really runs, rather than a refusal. The
shape to look at is the POSIX shell this repository already serves caller-supplied shell
programs with in other services (`services/common/src/posix_shell_session.rs`, which ECS, SSM
and EMR run shell text through in-process, with their own in-memory filesystem): a handler's
`subprocess` call could resolve its program against the execution environment's own mounts
(`/opt/bin`, `/var/task`) and the shell's own utilities, run a shell program through that
engine against the environment's filesystem (the read-only mounts and the function's ephemeral
storage), and answer the exit status and the output that engine really produced. A program that
is not text that engine can run — a native executable a layer ships — stays a capability this
emulator does not have.

Traces: lambda.deep-lambda-runtime-truth-layer-files-byte-exact-at-opt-and-opt-write-rejected.

## Evidence
Expected vs actual, from the replay:
- deep-lambda-runtime-truth-layer-files-byte-exact-at-opt-and-opt-write-rejected: expected the
  handler's own report; got FunctionError=Unhandled
  `{"errorMessage":"[Errno 38] a Lambda function in this emulator starts no child process","errorType":"OSError"}`
  with the handler's own frame at the top of the stack trace. The layer-file readings and the
  refusal of a write under `/opt` that the same trace covers are served.

## Steps to Reproduce
1. `PublishLayerVersion` with a layer that ships a program under `python/` and `bin/`.
2. `CreateFunction` (python3.x, Zip) with that layer attached, and a handler that reads the
   layer's files and runs a program.
3. `Invoke` it.
4. Observe 200 + `X-Amz-Function-Error: Unhandled` with the `OSError` above, where the file
   readings in the same handler succeed.
