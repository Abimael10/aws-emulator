Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.nodejs-child-process-exec-sync
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.nodejs-child-process-exec-sync|run#4|AssertionError|assert "error" not in out, f"execSync threw: {out!r}"
Issue-Commit: aa86e9e75ce0346509f1e7a26ee54199b6f59f45

# lambda: the nodejs22.x managed runtime cannot spawn a child process — child_process.execSync fails with `spawnSync /bin/sh ENOSYS` instead of returning the shell's stdout

> _Note: this issue was identified against the code base at commit `aa86e9e75ce0346509f1e7a26ee54199b6f59f45`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: the Node.js managed-runtime sandbox offers no process-creation primitive and no shell, so `child_process.execSync` (and by extension spawnSync/execFileSync and the async child_process API) throws instead of running a command. Failing check id for this cause: lambda.nodejs-child-process-exec-sync.

AWS: recorded behavior only — the Smithy model for Lambda describes Invoke's wire shape, not the execution environment, so the requirement comes from the recording and from the documented runtime contract (https://docs.aws.amazon.com/lambda/latest/dg/lambda-runtimes.html, https://nodejs.org/docs/latest-v22.x/api/child_process.html#child_processexecsynccommand-options). The nodejs22.x managed runtime is real Node.js on a real Amazon Linux 2023 sandbox that includes `/bin/sh`. `child_process.execSync("echo $((6*7))")` therefore forks `/bin/sh -c`, the shell performs the arithmetic expansion and writes `42\n` to stdout, and execSync returns that as a Buffer. The recorded Invoke answers HTTP 200 with payload `{"stdout": "42"}` and no `error` key, and no FunctionError header.

Emulator: Invoke answers HTTP 200, but the handler's catch block runs: the payload is `{"error": "Error", "message": "spawnSync /bin/sh ENOSYS"}`. `ENOSYS` ("function not implemented") is the kernel/stub error for a syscall that does not exist, so the sandbox's child_process is a stub: no subprocess is created and `/bin/sh` is never executed. The fixed text of that message is Node's own `spawnSync <file> <errno>` template; the variable parts are the executable the runtime tried to spawn (`/bin/sh`, chosen by execSync) and the errno. AWS produces no such message at all for this call — the correct observable is the shell's stdout.

Why: a client relying on AWS's behavior gets silent, wrong answers from any handler that shells out. Shelling out is a normal Lambda pattern — invoking a bundled binary from a layer, running `ffmpeg`/`sharp`-style tooling, calling `tar`/`unzip`, reading `uname`, or using any npm package that internally calls child_process. All of those become a caught `Error: spawnSync ... ENOSYS` at runtime rather than a result, and because Invoke still answers 200 with no FunctionError the failure is indistinguishable from a successful invocation at the API level: retry and alarming logic keyed on FunctionError never fires. Code that works on AWS cannot be developed or tested against the emulator at all.

Fix: the Lambda execution-environment component that provides the managed Node.js interpreter — its child_process implementation (spawnSync/spawn/execSync/execFileSync must actually create a process and return its exit status, stdout and stderr) and the sandbox image it runs in, which must contain a POSIX shell at `/bin/sh` for execSync's `-c` form to resolve.

Related open issue that does NOT cover this check: 20261111T081112Z-lambda-managed-runtimes-are-stub-interpreters-10-node.md carries the other Node.js/Python execution-environment checks (core-module surface, crypto, worker_threads, python-subprocess, …) but not this one's replay key, so this check must be closed on its own evidence.

Traces: lambda.nodejs-child-process-exec-sync.

## Evidence
Expected (AWS, from the recording): a nodejs22.x function whose handler is `const { execSync } = require("child_process"); exports.handler = async () => ({ stdout: execSync("echo $((6*7))").toString().trim() });` is Invoked and answers HTTP 200 with payload `{"stdout": "42"}` — no `error` key, no FunctionError header.

Actual (emulator), check lambda.nodejs-child-process-exec-sync [mismatch]:
  AssertionError: execSync threw: {'error': 'Error', 'message': 'spawnSync /bin/sh ENOSYS'}
  Invoke still answers 200 with no FunctionError; the divergence is entirely in the response payload. `spawnSync /bin/sh ENOSYS` is Node's own `spawnSync <file> <errno>` text, with `/bin/sh` the executable execSync asked for and `ENOSYS` the errno the stub returned. The per-run function name, role name and request id are not part of the requirement.

## Steps to Reproduce
1. CreateFunction with Runtime=nodejs22.x, Handler=index.handler and a zip containing index.js:
     const { execSync } = require("child_process");
     exports.handler = async () => {
       try { return { stdout: execSync("echo $((6*7))").toString().trim() }; }
       catch (e) { return { error: e.name, message: e.message, status: e.status }; }
     };
2. Wait for the function to reach Active, then Invoke it with an empty payload.
3. AWS: the response payload is `{"stdout": "42"}`.
4. Emulator: the response payload is `{"error": "Error", "message": "spawnSync /bin/sh ENOSYS"}` — no child process was created and `/bin/sh` was never run.
5. The same stub shows up for `execFileSync('/bin/sh', ['-c', 'echo hi'])` and for the async `spawn`/`exec` forms.
