Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.layer-code-importable lambda.layer-count-over-five-rejected lambda.layer-declared-incompatible-runtime-and-architecture-still-attaches-and-loads lambda.layer-published-version-keeps-its-layer-after-latest-repoints-and-layer-deleted lambda.layers-deleted-version-still-resolves-new-reference-rejected lambda.layers-detach-breaks-handler-import lambda.layers-version-pin-ignores-newer-version lambda.layers-toplevel-zip-entry-not-importable-rejected lambda.layer-unchanged-reattach-of-a-deleted-version-still-revalidated lambda.layer-python-site-packages-dir-importable-and-precedes-python-root lambda.layer-python-site-packages-for-other-minor-version-not-importable-rejected lambda.layer-pth-files-in-layer-site-dirs-extend-sys-path lambda.layers-function-package-shadows-layer-module
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.layer-code-importable|_invoke_sync#12|AssertionError|raise AssertionError(
Issue-Key: lambda.layer-count-over-five-rejected|run#11|AssertionError|assert "FunctionError" not in resp, resp["Payload"].read()
Issue-Key: lambda.layer-declared-incompatible-runtime-and-architecture-still-attaches-and-loads|_invoke_ok#13|AssertionError|assert error is None, f"{fn} raised ({error}) instead of returning: {str(body)[:600]}"
Issue-Key: lambda.layer-published-version-keeps-its-layer-after-latest-repoints-and-layer-deleted|_invoke_ok#20|AssertionError|assert error is None, f"{fn} raised ({error}) instead of returning: {str(body)[:600]}"
Issue-Key: lambda.layers-deleted-version-still-resolves-new-reference-rejected|_invoke_json#12|AssertionError|assert "FunctionError" not in resp, (
Issue-Key: lambda.layers-detach-breaks-handler-import|_invoke_json#11|AssertionError|assert "FunctionError" not in resp, (
Issue-Key: lambda.layers-version-pin-ignores-newer-version|_invoke_json#16|AssertionError|assert "FunctionError" not in resp, (
Issue-Key: lambda.layers-toplevel-zip-entry-not-importable-rejected|_invoke_json#13|AssertionError|assert "FunctionError" not in resp, (
Issue-Key: lambda.layer-unchanged-reattach-of-a-deleted-version-still-revalidated|run#7|AssertionError|assert _invoke(lam, fn) == ({"marker": token}, None), "positive control: the layer module must import"
Issue-Key: lambda.layer-python-site-packages-dir-importable-and-precedes-python-root|_invoke_ok#9|AssertionError|assert error is None, f"{fn} raised ({error}) instead of returning: {str(body)[:600]}"
Issue-Key: lambda.layer-python-site-packages-for-other-minor-version-not-importable-rejected|_invoke_ok#8|AssertionError|assert error is None, f"{fn} raised ({error}) instead of returning: {str(body)[:600]}"
Issue-Key: lambda.layer-pth-files-in-layer-site-dirs-extend-sys-path|_invoke_ok#9|AssertionError|assert error is None, f"{fn} raised ({error}) instead of returning: {str(body)[:600]}"
Issue-Key: lambda.layers-function-package-shadows-layer-module|run#19|AssertionError|assert "/var/task" in syspath and "/opt/python" in syspath, (
Issue-Commit: 677b6e35c5226b67b9bfea5e554d250e3c084a9c

# lambda: Lambda layers are never materialized into the execution environment — /opt is empty, sys.path omits /var/task and /opt (RESOLVED)

> _Note: this issue was identified against the code base at commit `677b6e35c5226b67b9bfea5e554d250e3c084a9c`._

## Operation
Invoke

## Resolution

The execution environment a `python3.x` function runs in is now the MOUNTS it is built from.
Every entry of the deployment package is a file of the task root and every entry of each
attached layer is a file of the layer root, read-only: a filesystem call names it, reads it,
lists the directory holding it and refuses to write it; the import directories of those mounts
are what `sys.path` lists, in the order a managed runtime lists them; and each import
directory of the layer root is a site directory whose path-configuration files extend that
path. A path on no mount of the function stays reachable in no way at all — this environment
has no host filesystem behind it.

- Cause 1 (layer content never extracted, so no layer module was importable) — FIXED. The
  layers' entries are mounted at the layer root and resolved from its import directories, so a
  module a layer ships imports and reports its own file under `/opt`.
- Cause 2 (the layer's site-packages directory and its `.pth` files were on no path) — FIXED.
  The site directory of the runtime's own version precedes the version-independent directory,
  a site directory named for another version is on no path, and a path-configuration file of
  either import directory extends `sys.path` with the directories it names.
- Cause 3 (`sys.path` carried neither the task root nor the layer root) — FIXED. It lists
  `/var/task`, then the layer root's import directories, then the runtime's own.
- Cause 6, the filesystem half (layer files unreachable through filesystem calls) — FIXED.
  `os.stat`/`getsize`/`listdir`/`scandir`/`access`, `open()` and the descriptor calls answer a
  mounted path from the entries the host read; a write answers `EROFS`; an absent entry of a
  mount answers `ENOENT`. The combined-unzipped-size refusal this cause also names was already
  in force (`state/layer_store.rs`'s `combined_unzipped_refusal`).

Also composed, because the causes below name them: `/opt/bin` is on the `PATH` the environment
reports and `/opt/lib` on `LD_LIBRARY_PATH`, so a layer's executable and library directories
are where a managed sandbox puts them.

What is NOT part of this resolution was split into its own report,
`issues/things-that-do-not-work/20261118T101500Z-lambda-a-layers-native-file-is-not-loaded-or-run-and-a-large.md`:
loading a layer-shipped shared object (cause 4), running a layer-shipped program (cause 5) and
holding a mount entry larger than one invocation's inflated-byte budget (the control of cause
6's quota check). The three replay checks that rest on those capabilities moved with it.

Fixed by: the mounts' filesystem (`state/execution/guest_mount.rs`), the import path and site
processing (`state/execution/bootstrap.rs`) and the sandbox's own `PATH`
(`state/execution.rs`). Covered end to end by
`services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/python_layer_filesystem.rs`
and `.../tests/all/python_layer_import_path.rs`.
