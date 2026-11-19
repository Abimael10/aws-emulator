Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.layer-lib-dir-on-ld-library-path-loads-shared-object-by-bare-name lambda.layers-bin-on-path-executes-layer-binary lambda.layers-combined-unzipped-size-over-250mb-rejected
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.layer-lib-dir-on-ld-library-path-loads-shared-object-by-bare-name|_invoke_ok#10|AssertionError|assert error is None, f"{fn} raised ({error}) instead of returning: {str(body)[:600]}"
Issue-Key: lambda.layers-bin-on-path-executes-layer-binary|_invoke_json#11|AssertionError|assert "FunctionError" not in resp, (
Issue-Key: lambda.layers-combined-unzipped-size-over-250mb-rejected|_invoke_ok#10|AssertionError|assert error is None, f"{fn} raised ({error}) instead of returning: {str(body)[:600]}"
Issue-Commit: 677b6e35c5226b67b9bfea5e554d250e3c084a9c

# lambda: a layer's native file is not loaded or run, and a mount entry past one invocation's read budget is held truncated (3 causes)

> _Note: these three causes were split out of
> `20261107T091747Z-lambda-lambda-layers-are-never-materialized-into-the.md`, whose other
> causes — layer extraction, the import path, site processing and the filesystem surface of
> the mounts — are resolved. Each cause below needs a capability of its own, which is why it
> is reported apart rather than bundled with that fix._

## Operation
Invoke

## Description

Cause 1 of 3: a shared object a layer ships is not loaded, and the runtime has no `_ctypes`
extension module for a handler to ask for one with.
AWS: a managed `python3.x` runtime ships the whole CPython extension set, `_ctypes` among
them, and puts `/opt/lib` on `LD_LIBRARY_PATH`, so `ctypes.CDLL('<libname>.so')` loads a
library a layer ships under `lib/` by its bare name.
Emulator: the invoke answers 200 with `X-Amz-Function-Error: Unhandled` and a payload whose
`errorType` is `Runtime.ImportModuleError` and whose `errorMessage` is "Unable to import module
'<handler module>': No module named '_ctypes'". `/opt/lib` IS on `LD_LIBRARY_PATH`
(`state/execution.rs`), and a layer's own files are readable files of `/opt`
(`state/execution/guest_mount.rs`), so what is missing is the LOAD.
Why: a native-dependency layer — the main reason to ship one — is unusable, and the failure is
reported as the customer's import error.
Fix: hold an archive entry that is not UTF-8 as BYTES (`state/execution/package.rs` keeps an
entry's source text, so a shared object is dropped by the read today), then load the ELF in
process without the host dynamic loader, mapping the image into an emulated address space and
bounding each call by an instruction count and the run's remaining deadline — the native path
the project's code-execution guidance lays out. No host library load.
Traces: lambda.layer-lib-dir-on-ld-library-path-loads-shared-object-by-bare-name.

Cause 2 of 3: a program a layer ships under `bin/` does not run.
AWS: `/opt/bin` is on `PATH`, so a handler that shells out to an executable a layer ships runs
it by its bare name and reads what it wrote.
Emulator: `/opt/bin` is on the `PATH` the environment reports and the program is a readable
file of `/opt`, so a handler resolves it through `PATH` — and then the call that would start it
answers `OSError` carrying `ENOSYS`, "a Lambda function in this emulator starts no child
process" (`state/execution/guest_process.rs`), which is the honest answer for a runtime with
no process to fork.
Why: a handler that runs a layer-shipped tool cannot do its work.
Fix: run the program IN PROCESS — a program the mounts hold, executed by an engine this
emulator already has (the interpreter for a script whose interpreter line names one) or by the
native path of cause 1, under the run's own bounds, with its output captured and capped. No
host process.
Traces: lambda.layers-bin-on-path-executes-layer-binary.

Cause 3 of 3: a mount entry larger than one invocation's inflated-byte budget is held
truncated, so it reports the size it holds rather than the size it has.
AWS: a layer's content is extracted whole, up to the 250 MB unzipped ceiling the function and
its layers share, so `os.path.getsize` of a large file a layer ships answers that file's own
size.
Emulator: each mount's entries are read into this process's memory under
`state/execution/package.rs`'s per-entry (`MAX_MODULE_BYTES`) and per-invocation
(`MAX_UNZIPPED_BYTES`) ceilings, so a larger entry is held cut short and the filesystem reports
what it really holds — a 140 MiB file answers 8388608 bytes. Reporting the archive's declared
size while holding less would be a filesystem that lies about what a read returns.
Why: the control of the combined-unzipped-size check — a single layer under the ceiling whose
file must read back whole — cannot be reproduced, and a handler that reads a large data file
from a layer sees a short file. The quota refusal itself is in force
(`state/layer_store.rs`'s `combined_unzipped_refusal`).
Fix: hand the mounts to the run out of band of the JSON document the runtime is initialised
with (the node runtime's `filePaths`/`fileSource` shape, `state/execution/node/runtime.rs`), so
an entry is held once by the host and read on demand instead of being copied through that
document, and spend the read against the service's own unzipped ceiling rather than a smaller
one. The per-invocation bound on what this process holds has to survive the change.
Traces: lambda.layers-combined-unzipped-size-over-250mb-rejected.

## Evidence
Expected (AWS): `ctypes.CDLL('<libname>.so')` loads a layer's shared object by bare name; a
layer's `bin/` program runs and its output reaches the handler; a layer's large data file
reports its own size.

Actual (emulator), from the replay:
- lambda.layer-lib-dir-on-ld-library-path-loads-shared-object-by-bare-name: FunctionError=Unhandled, `Runtime.ImportModuleError` "Unable to import module 'index': No module named '_ctypes'".
- lambda.layers-bin-on-path-executes-layer-binary: the handler resolves the program through `PATH` and reports it did not run (`OSError`).
- lambda.layers-combined-unzipped-size-over-250mb-rejected: the one-layer control reads 8388608 bytes for a 146800640-byte file.

## Steps to Reproduce
1. PublishLayerVersion with a zip holding `lib/<libname>.so`, `bin/<tool>` and a data file far
   larger than one invocation's inflated-byte budget.
2. CreateFunction (python3.x, Zip) with Layers=[<that layer version ARN>] and a handler that
   dlopens the library by bare name, execs the program by bare name, and
   `os.path.getsize`s the data file.
3. Invoke the function.
4. Observe the import error, the refusal to start a process, and the truncated size.
5. Re-replay the check ids in trace_ids to confirm.
