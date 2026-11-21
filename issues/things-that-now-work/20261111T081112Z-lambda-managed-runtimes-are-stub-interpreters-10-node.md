Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.nodejs-core-module-surface lambda.nodejs-worker-threads-run lambda.nodejs-zlib-gzip-round-trip lambda.nodejs-web-globals-present lambda.nodejs-crypto-cipher-round-trip lambda.nodejs-resolves-package-exports-map lambda.nodejs-handler-reads-bundled-config-file
Issue-Key: lambda.nodejs-core-module-surface|run#4|AssertionError|assert out["failed"] == [], f"core modules failed to load: {out['failed']!r}"
Issue-Key: lambda.nodejs-worker-threads-run|_invoke_ok#4|AssertionError|assert err is None, f"the function failed: {err!r} {out!r}"
Issue-Key: lambda.nodejs-zlib-gzip-round-trip|_invoke_ok#5|AssertionError|assert err is None, f"the function failed: {err!r} {out!r}"
Issue-Key: lambda.nodejs-web-globals-present|_invoke_ok#3|AssertionError|assert err is None, f"the function failed: {err!r} {out!r}"
Issue-Key: lambda.nodejs-crypto-cipher-round-trip|_invoke_ok#3|AssertionError|assert err is None, f"the function failed: {err!r} {out!r}"
Issue-Key: lambda.nodejs-resolves-package-exports-map|_invoke_ok#3|AssertionError|assert err is None, f"the function failed: {err!r} {out!r}"
Issue-Key: lambda.nodejs-handler-reads-bundled-config-file|run#5|AssertionError|assert out["dirname"] == "/var/task", f"the handler's __dirname was {out['dirname']!r}"
Issue-Commit: f4372f081be041f1c06808452ec2531466fafcb7

# Lambda managed runtimes are stub interpreters: the five Node.js execution-environment causes (FIXED)

> _Note: this issue was identified against the code base at commit `f4372f081be041f1c06808452ec2531466fafcb7`. The report as filed carried TEN causes; the five Node.js ones below are fixed and their seven replay keys pass. The other five causes — the npm-bundle one and the four Python ones — do NOT pass and are re-filed, with their own replay keys and the blocker each rests on, as `issues/things-that-do-not-work/20261111T081112Z-lambda-the-python-execution-environment-and-the-npm-bundle-path.md`, so that nothing unfixed is recorded as working._

## Operation
Invoke

## Description
The emulator's managed Lambda execution environment for nodejs*.x was not Node.js: its module
and global surface was a small subset of the real runtime, and the paths it reported were not
paths. Five causes, each with its own failing checks.

Cause 1 of 5: the Node.js runtime provides no Node core modules.
AWS: recorded behavior only - a nodejs managed runtime is Node.js, so `require(...)`/`import` of the built-in modules (http, https, net, dns, tls, zlib, child_process, worker_threads, perf_hooks, async_hooks, readline, v8, vm, ...) resolves and the handler runs. The model says nothing about the runtime's module set; Smithy: Invoke returns StatusCode 200 with no X-Amz-Function-Error when the handler succeeds.
Emulator: every core-module require failed and the invoke came back with X-Amz-Function-Error: Unhandled and errorType `Runtime.ImportModuleError`, errorMessage `Error: Cannot find module '<module>'\nRequire stack:\n- /var/task/index.js\n- /var/runtime/index.mjs`.
Fixed: the runtime now supplies every module of the standard library it states, under both the
bare and the `node:` name — `net` and `tls` (`node/core_modules/sockets.rs`), `dns` and
`dns/promises` (`node/core_modules/dns.rs`, answered by the execution environment's own
resolver), `zlib` (`node/core_modules/zlib.rs` over the host's own deflate, brotli and CRC-32
in `node/compress.rs`), `worker_threads` (which RUNS the worker's entry point),
`perf_hooks`/`async_hooks`/`v8` (`node/core_modules/diagnostics.rs`) and `vm`/`readline`
(`node/core_modules/scripting.rs`). A module whose own capability this runtime has none of
imports and refuses the CALL, which is the decision `child_process` already states.
Failing checks, now passing: lambda.nodejs-core-module-surface, lambda.nodejs-worker-threads-run, lambda.nodejs-zlib-gzip-round-trip (lambda.nodejs-child-process-exec-sync imports now too, and its remaining failure — spawning a host process — is re-filed; see the note above).

Cause 2 of 5: the Node.js runtime is missing standard web globals.
AWS: recorded behavior only - `structuredClone` (and the other WHATWG globals Node 18+ exposes) is defined in a nodejs managed runtime.
Emulator: the handler failed with X-Amz-Function-Error: Unhandled, errorType `ReferenceError`, errorMessage `structuredClone is not defined`.
Fixed: `node/web_globals.rs` installs the release's global surface before the first module of
the deployment package is evaluated.
Failing checks, now passing: lambda.nodejs-web-globals-present

Cause 3 of 5: the Node.js runtime's crypto cipher API is not callable.
AWS: recorded behavior only - a nodejs runtime's `crypto.createCipheriv`/`createDecipheriv` round-trips a payload.
Emulator: the handler failed with errorType `TypeError`, errorMessage `crypto.pbkdf2Sync is not a function`.
Fixed: the password-based derivation the check derives its key with is computed by the host
(`node/digest.rs`'s `pbkdf2`, pinned by RFC 6070's and RFC 7914's published vectors).
Failing checks, now passing: lambda.nodejs-crypto-cipher-round-trip

Cause 4 of 5: the Node.js module resolver ignores a package.json "exports" subpath map.
AWS: recorded behavior only - Node resolves `require('pkg/sub')`/`import 'pkg/sub'` through the dependency's package.json `exports` map.
Emulator: the handler failed with errorType `Error` and errorMessage `Cannot find module 'expkg/sub'` plus a require stack.
Fixed: the resolver resolves a bare specifier through the dependency's own map — the subpath, the
conditions per KIND of load, and the pattern keys — and refuses an unexported path with node's
own `ERR_PACKAGE_PATH_NOT_EXPORTED` wording.
Failing checks, now passing: lambda.nodejs-resolves-package-exports-map

Cause 5 of 5: `__dirname` in a Node handler carries a trailing slash.
AWS: recorded behavior only - in a CommonJS handler at /var/task/index.js, `__dirname` is the directory path with no trailing separator, i.e. `/var/task`.
Emulator: `__dirname` was reported as `/var/task/`.
Fixed: a mount root's own path is the mount, in the one place the runtime builds a path from an
id (`node/runtime.rs`'s `filenameOf`).
Failing checks, now passing: lambda.nodejs-handler-reads-bundled-config-file

## Evidence
Cause 1: expected handler success; got X-Amz-Function-Error 'Unhandled', errorType 'Runtime.ImportModuleError', e.g. "Error: Cannot find module 'zlib'\nRequire stack:\n- /var/task/index.js\n- /var/runtime/index.mjs".
Cause 2: expected defined global; got errorType 'ReferenceError', errorMessage 'structuredClone is not defined'.
Cause 3: expected a cipher round trip; got errorType 'TypeError', errorMessage 'crypto.pbkdf2Sync is not a function'.
Cause 4: expected 'expkg/sub' to resolve; got errorType 'Error', errorMessage "Cannot find module 'expkg/sub'".
Cause 5: expected __dirname == '/var/task'; got '/var/task/'.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite.
2. CreateFunction with a nodejs managed runtime and a handler that requires a Node core module /
   uses structuredClone / crypto.pbkdf2Sync + createCipheriv / a package.json exports subpath /
   reads __dirname-relative files, then Invoke it and read X-Amz-Function-Error and the payload.
3. Re-replay exactly the check ids in trace_ids to confirm.
