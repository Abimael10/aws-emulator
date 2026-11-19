Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.package-non-ascii-file-names-preserved-byte-exact lambda.package-symlinks-extracted-as-symlinks-and-followed lambda.python-handler-in-nested-package-resolves-relative-imports-and-files lambda.python-module-state-is-per-version-not-shared-with-latest lambda.python-runtime-family-every-supported-minor-runs-identically lambda.task-root-and-runtime-dir-read-only-while-tmp-writable-rejected lambda.tmp-not-shared-between-two-distinct-functions lambda.tmp-persists-across-warm lambda.tmp-writable lambda.outbound-dns-resolution-failure-for-unresolvable-hostname-rejected lambda.package-file-modes-normalized-so-every-file-is-readable-and-executable lambda.recursion-config-terminate-stops-a-self-invocation-chain lambda.snapstart-python-version-restores-with-before-snapshot-state-and-a-restore-report lambda.snapstart-restored-version-reuses-the-snapshot-init lambda.two-layers-sharing-a-path-resolve-to-the-last-layer-at-runtime lambda.nodejs-process-exit-reports-exit-error-and-reinitializes-rejected lambda.nodejs-warm-environment-reuses-module-scope-and-tmp lambda.out-of-memory-kill-discards-the-execution-environment lambda.provisioned-concurrency-invocation-reports-no-init-duration lambda.python-sys-exit-reports-runtime-exit-error-and-resets-state lambda.stream-provisioned-environment-reports-provisioned-initialization-type lambda.warm-start-reuse lambda.deep-lambda-runtime-truth-oversize-request-rejected-before-handler-oversize-response-after lambda.nodejs-v8-heap-exhaustion-aborts-distinct-from-os-level-out-of-memory-rejected lambda.out-of-memory-invocation-releases-its-reserved-concurrency-slot lambda.published-version-keeps-its-own-memory-limit-rejected-past-it lambda.python-runtime-defined-env-vars-match-the-runtime-identity lambda.python-user-env-vars-and-tz-override-reach-the-process lambda.ruby-reserved-env-vars-override-attempt lambda.runtime-update-reflected-in-sys-version lambda.stdlib-imports lambda.python-return-bytes-and-non-finite-floats-pass-through-unvalidated lambda.python-return-values-serialize-to-exact-json-bytes lambda.nodejs-async-handler-never-settling-rejected-as-nodejs-exit lambda.nodejs-unknown-node-options-flag-rejected-as-exit-error
Issue-Key: lambda.package-non-ascii-file-names-preserved-byte-exact|run#14|AssertionError|assert found == expected, (
Issue-Key: lambda.package-symlinks-extracted-as-symlinks-and-followed|_invoke_ok#10|AssertionError|assert error is None, f"{fn} raised ({error}) instead of returning: {str(body)[:600]}"
Issue-Key: lambda.python-handler-in-nested-package-resolves-relative-imports-and-files|_ok#6|AssertionError|assert r["status"] == 200 and r["error"] is None, (
Issue-Key: lambda.python-module-state-is-per-version-not-shared-with-latest|_ok#3|AssertionError|assert r["status"] == 200 and r["error"] is None, (
Issue-Key: lambda.python-runtime-family-every-supported-minor-runs-identically|_invoke_sync#5|AssertionError|raise AssertionError(
Issue-Key: lambda.task-root-and-runtime-dir-read-only-while-tmp-writable-rejected|_invoke_ok#9|AssertionError|assert error is None, f"{fn} raised ({error}) instead of returning: {str(body)[:600]}"
Issue-Key: lambda.tmp-not-shared-between-two-distinct-functions|_invoke_sync#9|AssertionError|raise AssertionError(
Issue-Key: lambda.tmp-persists-across-warm|_invoke_sync#5|AssertionError|raise AssertionError(
Issue-Key: lambda.tmp-writable|_invoke_sync#4|AssertionError|raise AssertionError(
Issue-Key: lambda.outbound-dns-resolution-failure-for-unresolvable-hostname-rejected|_invoke#6|AssertionError|assert "FunctionError" not in resp, f"handler failed with {resp.get('FunctionError')!r}: {raw!r}"
Issue-Key: lambda.package-file-modes-normalized-so-every-file-is-readable-and-executable|_invoke_ok#8|AssertionError|assert error is None, f"{fn} raised ({error}) instead of returning: {str(body)[:600]}"
Issue-Key: lambda.recursion-config-terminate-stops-a-self-invocation-chain|_run_chain#10|AssertionError|assert "FunctionError" not in resp, (
Issue-Key: lambda.snapstart-python-version-restores-with-before-snapshot-state-and-a-restore-report|run#11|AssertionError|assert r["error"] is None, f"version invoke failed: {r['error']!r} {r['raw'][:400]!r}"
Issue-Key: lambda.snapstart-restored-version-reuses-the-snapshot-init|run#16|AssertionError|assert all(b["hook_import"] == "ok" for b in ver_bodies), (
Issue-Key: lambda.two-layers-sharing-a-path-resolve-to-the-last-layer-at-runtime|_invoke_json#12|AssertionError|assert "FunctionError" not in resp, (
Issue-Key: lambda.nodejs-process-exit-reports-exit-error-and-reinitializes-rejected|run#9|AssertionError|assert kept["initId"] == first["initId"] and kept["calls"] == first["calls"] + 2, (
Issue-Key: lambda.nodejs-warm-environment-reuses-module-scope-and-tmp|run#7|AssertionError|assert max(len(c) for c in by_env.values()) >= 2, (
Issue-Key: lambda.out-of-memory-kill-discards-the-execution-environment|run#8|AssertionError|raise AssertionError(f"five invokes never reached environment {doomed!r}: {body!r}")
Issue-Key: lambda.provisioned-concurrency-invocation-reports-no-init-duration|_invoke_tail#6|AssertionError|assert "FunctionError" not in resp, (
Issue-Key: lambda.python-sys-exit-reports-runtime-exit-error-and-resets-state|run#6|AssertionError|assert warm == {"calls": 2}, (
Issue-Key: lambda.stream-provisioned-environment-reports-provisioned-initialization-type|run#26|AssertionError|assert warm2["env_id"] == warm["env_id"], (
Issue-Key: lambda.warm-start-reuse|run#9|AssertionError|assert any(c > 1 for c in counts[1:]), (
Issue-Key: lambda.deep-lambda-runtime-truth-oversize-request-rejected-before-handler-oversize-response-after|run#10|AssertionError|assert not ctl.get("FunctionError") and ctl_body == {"label": "control", "tagged": True}, (
Issue-Key: lambda.nodejs-v8-heap-exhaustion-aborts-distinct-from-os-level-out-of-memory-rejected|run#7|AssertionError|assert isinstance(limit, int) and limit < 2 * _OLD_SPACE_MB * 1024 * 1024, (
Issue-Key: lambda.out-of-memory-invocation-releases-its-reserved-concurrency-slot|run#12|AssertionError|assert detail[1].get("errorType") == "Runtime.OutOfMemory", f"400 MB body: {detail[1]!r}"
Issue-Key: lambda.published-version-keeps-its-own-memory-limit-rejected-past-it|_assert_out_of_memory#12|AssertionError|assert resp.get("FunctionError") == "Unhandled", (
Issue-Key: lambda.python-runtime-defined-env-vars-match-the-runtime-identity|run#3|AssertionError|assert got["env"] == want_env, (
Issue-Key: lambda.python-user-env-vars-and-tz-override-reach-the-process|run#9|AssertionError|assert got["tzname"] == ["EST", "EDT"], (
Issue-Key: lambda|CreateFunction|InvalidParameterValueException
Issue-Key: lambda.runtime-update-reflected-in-sys-version|run#5|AssertionError|assert _invoke(lam, fn) == ("$LATEST", {"python": "3.12"}), "python3.12 must run 3.12"
Issue-Key: lambda.stdlib-imports|run#7|AssertionError|assert resp["python_version"] == expected_version, (
Issue-Key: lambda.python-return-bytes-and-non-finite-floats-pass-through-unvalidated|run#7|AssertionError|assert raw["status"] == 200 and raw["error"] is None, (
Issue-Key: lambda.python-return-values-serialize-to-exact-json-bytes|run#3|AssertionError|assert r["error"] is None, (
Issue-Key: lambda.nodejs-async-handler-never-settling-rejected-as-nodejs-exit|run#8|AssertionError|assert body["errorMessage"] == _MESSAGE, f"errorMessage was {body['errorMessage']!r}"
Issue-Key: lambda.nodejs-unknown-node-options-flag-rejected-as-exit-error|run#15|AssertionError|assert body == {
Issue-Commit: 9c8eccc9df8ca2a50600c982a439e42600f4f44a

# lambda: 7 of the parent report's 10 causes are still open — no sandbox filesystem, an incomplete runtime standard library, no environment reuse under load, MemorySize unenforced, the runtime identity not following the configured Runtime, the response serializer refusing values AWS passes through, and the node exit envelope

> _Split out of `issues/things-that-now-work/20261107T135301Z-lambda-10-net-new-root-causes-in-the--r2.md`, whose causes 6, 7 and 9 are fixed. This report carries the replay keys that do NOT pass, re-filed here so they are not recorded as working. Everything below is what that fix established about them; a later fixer starts from here rather than from scratch._

## Operation
Invoke

## Description
Seven of the parent report's ten causes are open, and each is a capability of its own rather
than a wording or shape fix. Two of them are also the subject of their own open reports, named
under the cause.

What the parent fix DID land, so a later fixer does not redo it: the context object hands the
handler its identity and its client context as objects carrying their own members
(`CognitoIdentity`, `ClientContext`, `Client` in `state::execution::bootstrap`), the client
context travels to the runtime as the TEXT the caller sent and the runtime parses it,
`context.invoked_function_arn` carries exactly the qualifier the call named
(`state::invoked_function_arn`), the payload is unmarshalled and the context built in the BIND
phase so the handler is entered with its whole `Timeout` left, an initialization failure is
classified by what failed (`Runtime.BuiltInModuleConflict`, `Runtime.UserCodeSyntaxError`, the
package's own exception class) and names no invocation because the ENGINE reports the phase
(`Completion::Raised { initialization }`) rather than the error's class, an error document past
the response quota is answered as a response past it, and a payload whose first JSON value is
complete reaches the runtime, which answers `Runtime.UnmarshalError` for what follows it.

Cause 1 of 7: the invocation sandbox has no filesystem at all.
AWS: a Lambda invocation runs in a sandbox with /var/task (the unzipped package, read-only),
/var/runtime (read-only) and a writable 512 MB /tmp that survives within one execution
environment; os.listdir, open(), os.statvfs, os.readlink, os.path.getsize and
platform.machine() all work. Smithy: CreateFunction.EphemeralStorage models the /tmp size.
Emulator: every such call fails the invocation with FunctionError='Unhandled' and errorType
'OSError', errorMessage either "a Lambda function in this emulator has no host filesystem or
process to reach" or "a Lambda function in this emulator cannot open a file of the host" — both
strings name the emulator and cannot occur on AWS. The refusals are deliberate containment:
`state::execution::python::refuse_host_file_access` replaces `builtins.open`, `io.open` and
`_io.FileIO`, and `state::execution::host_posix` is a `posix` module with no file call in it at
all.
Fix: an in-memory filesystem for the sandbox, which the `posix` module above answers from and
`open` is built on — the package's entries materialised under /var/task (today only the TEXT
entries are read, see `state::execution::package`, so byte-exact and non-UTF-8 entries need the
reader widened too), the layers' content under /opt, and a writable /tmp held per execution
environment (`state::execution::warm_pool`) so it survives a warm start and is not shared
between two functions.
Also the subject of `issues/things-that-do-not-work/20261107T091747Z-lambda-the-execution-environment-has-no-filesystem-or.md`.
Traces: lambda.package-non-ascii-file-names-preserved-byte-exact,
lambda.package-symlinks-extracted-as-symlinks-and-followed,
lambda.python-handler-in-nested-package-resolves-relative-imports-and-files,
lambda.python-module-state-is-per-version-not-shared-with-latest,
lambda.python-runtime-family-every-supported-minor-runs-identically,
lambda.task-root-and-runtime-dir-read-only-while-tmp-writable-rejected,
lambda.tmp-not-shared-between-two-distinct-functions, lambda.tmp-persists-across-warm,
lambda.tmp-writable.

Cause 2 of 7: the Python runtime's standard library and bundled packages are incomplete, so
imports AWS resolves raise ImportModuleError/ModuleNotFoundError.
AWS: the managed python3.x runtimes ship a complete CPython stdlib (including the _signal,
_socket and _ctypes extension modules), a bundled boto3/botocore (botocore.config importable),
the snapshot_restore_py module on SnapStart-capable runtimes, and resolve modules supplied by
attached layers from /opt/python.
Emulator: the handler is refused at import with 'Runtime.ImportModuleError'/'ModuleNotFoundError'
and messages "No module named '_signal'", "No module named '_socket'", "No module named
'botocore.config'", "No module named 'snapshot_restore_py'" and "No module named 'only_a'" (a
module published in an attached layer). The native allow-list is
`state::execution::python::ALLOWED_NATIVE_MODULES` and the modules the runtime supplies in
Python are `state::execution::python::runtime_stdlib` / `runtime_modules`.
Fix: the runtime's module resolution — the extension modules a program expects of its operating
system, the bundled botocore surface, snapshot_restore_py, and /opt/python on the import path
(the layer finder exists, `bootstrap::_LayerFinder`, but a layer's content is only read as TEXT
modules today).
Also the subject of `issues/things-that-do-not-work/20261105T083215Z-ec2-lambda-logs-route53resolver-the-lambda-python-runtime-has-a-crippled-stdlib.md`.
Traces: lambda.outbound-dns-resolution-failure-for-unresolvable-hostname-rejected,
lambda.package-file-modes-normalized-so-every-file-is-readable-and-executable,
lambda.recursion-config-terminate-stops-a-self-invocation-chain,
lambda.snapstart-python-version-restores-with-before-snapshot-state-and-a-restore-report,
lambda.snapstart-restored-version-reuses-the-snapshot-init,
lambda.two-layers-sharing-a-path-resolve-to-the-last-layer-at-runtime.

Cause 3 of 7: no execution environment is reused when the behavioural suite exercises it.
AWS: consecutive invocations of an unchanged function reuse one execution environment: module
state and the init-time environment id persist, a cold start happens only after a
configuration/code change, a Runtime.ExitError, or an out-of-memory kill, and provisioned
concurrency serves pre-warmed environments.
Emulator: back-to-back invokes each report a fresh environment id and a module-level counter of
1 ('four sequential invokes never reused an environment: counters per environment [1, 1, 1, 1]'),
and a provisioned-concurrency invoke returns a second new id. The pool itself EXISTS and is
exercised in-process (`state::execution::warm_pool`, `tests/all/invocation_environment_reuse.rs`
passes), so the open part is why it does not hold under the suite's load: the bounds on what the
standing environments may hold (`MAX_IDLE_ENVIRONMENTS`, which is the run envelope's count, and
`MAX_STANDING_BYTES`) retire an environment as soon as several functions are being invoked at
once, and `grown <= budget.memory_bytes` retires one whose interpreter grew past the function's
own `MemorySize`.
Fix: make the keep/retire decision survive a client invoking several functions in a burst —
measure what an environment really holds rather than the pages its thread faulted, and give the
pool a budget that does not collapse to one environment per function version.
Traces: lambda.nodejs-process-exit-reports-exit-error-and-reinitializes-rejected,
lambda.nodejs-warm-environment-reuses-module-scope-and-tmp,
lambda.out-of-memory-kill-discards-the-execution-environment,
lambda.provisioned-concurrency-invocation-reports-no-init-duration,
lambda.python-sys-exit-reports-runtime-exit-error-and-resets-state,
lambda.stream-provisioned-environment-reports-provisioned-initialization-type,
lambda.warm-start-reuse.

Cause 4 of 7: MemorySize is reported but not enforced as an out-of-memory kill the handler can
reach.
AWS: Smithy models CreateFunction.MemorySize @range min 128 max 32768; the sandbox is capped at
that value, so a handler that allocates past it is killed and Invoke answers StatusCode 200 with
FunctionError 'Unhandled' and an errorMessage naming the memory limit; a node runtime's V8 heap
is bounded by it (and by --max-old-space-size in NODE_OPTIONS).
Emulator: a handler holding 400 MB under MemorySize=128 returns FunctionError=None while still
holding the block, or the invocation hits the function timeout instead;
v8.getHeapStatistics().heap_size_limit comes back None. The ceiling exists
(`state::execution::python::Bounds::past_run_ceiling`, measured as this thread's faulted pages)
but a large allocation the guest holds does not always fault enough pages to reach it, and the
node engine's heap limit is not reported to the guest at all.
Fix: memory accounting the guest's own allocations drive (the engine's allocation seam rather
than the thread's faulted pages), and the node heap flags.
Traces: lambda.deep-lambda-runtime-truth-oversize-request-rejected-before-handler-oversize-response-after,
lambda.nodejs-v8-heap-exhaustion-aborts-distinct-from-os-level-out-of-memory-rejected,
lambda.out-of-memory-invocation-releases-its-reserved-concurrency-slot,
lambda.published-version-keeps-its-own-memory-limit-rejected-past-it.

Cause 5 of 7: the runtime identity does not follow the configured Runtime, and the
reserved-key list at create time is not AWS's.
AWS: a function configured Runtime=python3.12 runs CPython 3.12 (sys.version_info[:2] == (3,
12)); the runtime-defined environment contains PYTHONPATH=/var/runtime; a user-supplied TZ (e.g.
America/New_York) reaches the process so time.tzname is ('EST', 'EDT'); and the reserved-key set
CreateFunction refuses is AWS's documented list — the recording shows a create that sets
AWS_EXECUTION_ENV being ACCEPTED.
Emulator: the handler reports sys.version_info [3, 14] whatever Runtime is configured,
PYTHONPATH is None, time.tzname stays ['UTC', 'UTC'] under TZ=America/New_York, and
CreateFunction refuses with InvalidParameterValueException "… Reserved keys used in this request:
AWS_EXECUTION_ENV" where the call must proceed.
Fix: the version the interpreter reports (the configured runtime's, installed before the
handler's own imports — note the frozen standard library of this interpreter reads
sys.version_info itself, so the install has to keep those imports working), PYTHONPATH in the
runtime-defined environment, a TZ the clock reads (chrono-tz is already a dependency of this
crate), and the reserved-key list in the CreateFunction handler narrowed to AWS's.
Traces: lambda.python-runtime-defined-env-vars-match-the-runtime-identity,
lambda.python-user-env-vars-and-tz-override-reach-the-process,
lambda.ruby-reserved-env-vars-override-attempt, lambda.runtime-update-reflected-in-sys-version,
lambda.stdlib-imports.

Cause 6 of 7: the response marshaller rejects values the managed Python runtime passes through.
AWS: the python runtime's serializer accepts bytes and non-finite floats and decimal values the
handler returns, producing exact JSON bytes; Runtime.MarshalError is reserved for values it
genuinely cannot encode.
Emulator: returning bytes answers FunctionError 'Unhandled' errorType 'Runtime.MarshalError'
message "Unable to marshal response: Object of type bytes is not JSON serializable", and
returning decimal.Decimal('1.10') answers the same error with "Object of type Decimal is not JSON
serializable". The serializer is `json.dumps(returned)` in `bootstrap::_invoke`.
Fix: the response serializer of the Python runtime. Note the hard part: a decimal has to be
written as its own literal rather than as a float, and the standard library's encoder renders a
float through `float.__repr__`, so emitting the digits the handler wrote needs an encoder of the
runtime's own rather than a `default=` hook.
Traces: lambda.python-return-bytes-and-non-finite-floats-pass-through-unvalidated,
lambda.python-return-values-serialize-to-exact-json-bytes.

Cause 7 of 7: the Node.js runtime's exit/flag failures are reported with the wrong envelope.
AWS: a node handler that never settles its promise is reported as a Runtime.ExitError-class
failure whose errorMessage is AWS's fixed text for that class, and an unknown NODE_OPTIONS flag
fails the INIT with the node bootstrap's own message rather than as an invocation-level exit
error.
Emulator: the never-settling handler answers errorMessage 'Node.js process exited before the
invocation was answered' (emulator-authored prose), and the unknown flag answers errorType
'Runtime.NodeJsExit' with errorMessage 'node: bad option: --sim2real-no-such-node-flag' (the flag
name is the variable part taken from the request; the surrounding classification is wrong).
Fix: the node runtime adapter's exit/init error mapping (`state::execution::node`,
`state::execution::node::runtime`).
Traces: lambda.nodejs-async-handler-never-settling-rejected-as-nodejs-exit,
lambda.nodejs-unknown-node-options-flag-rejected-as-exit-error.

## Evidence
Cause 1: FunctionError='Unhandled' {"errorType":"OSError","errorMessage":"a Lambda function in
this emulator has no host filesystem or process to reach"} for os.listdir('/tmp'),
os.statvfs('/tmp'), os.readlink, os.path.getsize('/opt/blob/zeros.bin') and platform.machine();
expected a clean 200 with the filesystem report. Non-ASCII package entries: expected
['données/résumé.txt', 'emoji/🚀-launch.txt', 'mixed/Ünïcødé ✓ name.txt', '日本語/ファイル.txt'],
guest saw [].
Cause 2: {"errorType":"Runtime.ImportModuleError","errorMessage":"Unable to import module
'index': No module named '_signal'"}; likewise '_socket', 'botocore.config',
'snapshot_restore_py', and ModuleNotFoundError "No module named 'only_a'" for a layer module.
Cause 3: 'four sequential invokes never reused an environment: counters per environment [1, 1,
1, 1]'; 'no warm-start reuse observed across 5 sequential invocations: counters=[1, 1, 1, 1, 1,
1]'; 'a second sequential invocation must reuse the one provisioned environment'.
Cause 4: '400 MB at 128 MB: FunctionError=None, expected Unhandled: {held_mb: 400,
memory_limit_in_mb: 128}'; '--max-old-space-size=256 must bound V8's heap far below
MemorySize=1536; heap_size_limit was None'.
Cause 5: 'runtime=python3.12 declares sys.version_info[:2]==[3, 12], the handler reported [3,
14]'; 'python3.11: sys.version_info[:2] was [3, 14], expected [3, 11]'; PYTHONPATH None vs
'/var/runtime'; 'with TZ=America/New_York time.tzname is [UTC, UTC], expected [EST, EDT]'.
Cause 6: {"errorType":"Runtime.MarshalError","errorMessage":"Unable to marshal response: Object
of type bytes is not JSON serializable"} and the same for Decimal.
Cause 7: errorMessage 'Node.js process exited before the invocation was answered'; errorType
'Runtime.NodeJsExit' errorMessage 'node: bad option: --sim2real-no-such-node-flag'.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (run_behavioral_suite).
2. Re-replay the check ids in trace_ids; they are all listed under '### lambda' in the report
   with verdict [mismatch] or [error].
3. Minimal repro for cause 1: CreateFunction(Runtime='python3.12', Handler='index.handler') with
   a handler that returns sorted(os.listdir('/tmp')), then Invoke — the emulator answers 200
   with FunctionError='Unhandled' and errorType 'OSError'.
4. Minimal repro for cause 5: Invoke a python3.12 function returning sys.version_info[:2] — it
   reports (3, 14).
