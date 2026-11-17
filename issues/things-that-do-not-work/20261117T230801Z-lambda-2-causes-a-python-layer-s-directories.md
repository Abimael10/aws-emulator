Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.layer-python-site-packages-dir-importable-and-precedes-python-root lambda.layer-python-site-packages-for-other-minor-version-not-importable-rejected
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.layer-python-site-packages-dir-importable-and-precedes-python-root|run#13|AssertionError|assert opt_path[:2] == [f"/opt/{_SITE}", "/opt/python"], (
Issue-Key: lambda.layer-python-site-packages-for-other-minor-version-not-importable-rejected|run#11|AssertionError|assert probe == {"onDisk": True}, (
Issue-Commit: 75900fccf35b95945ac4e89ca5619cc881e6a687

# lambda: 2 causes — a Python layer's directories are absent from the handler's sys.path, and layer entries under a non-matching Python minor version are never extracted to /opt

> _Note: this issue was identified against the code base at commit `75900fccf35b95945ac4e89ca5619cc881e6a687`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Two net-new root causes in how a published Lambda layer is materialized into the Python execution environment. Both are observed on invokes that otherwise succeed, so neither is a stub. Related open issues do NOT cover them: 20261107T091747Z-lambda-lambda-layers-are-never-materialized-into-the.md and 20261117T020113Z-lambda-a-layer-s-files-are-absent-from.md both describe /opt being EMPTY, whereas in these two checks layer modules under /opt DO import and report `/opt/...` file paths — what diverges is the sys.path the handler sees, and which zip entries get written at all.

Cause 1 of 2: the layer's Python directories are missing from `sys.path`.
AWS: for a managed Python runtime, the attached layers' content is unpacked under `/opt` and the interpreter's search path includes the layer's Python directories, with the runtime-specific site-packages directory ahead of the layer's `python/` root — i.e. `sys.path` lists `/opt/python/lib/python<major>.<minor>/site-packages` and then `/opt/python` (AWS docs, 'Packaging your layer content' / Lambda layer paths for Python). Because of that order, a module shipped at BOTH `python/lib/python3.12/site-packages/<mod>.py` and `python/<mod>.py` in one layer resolves to the site-packages copy. The ordering is not expressible in the Smithy model, so this is recorded behavior only.
Emulator: the handler's `sys.path` contains no entry beginning with `/opt` at all — `[p for p in sys.path if p.startswith("/opt")]` is `[]` — even though `importlib.import_module` on the layer modules succeeds and reports `/opt/python/lib/python3.12/site-packages/<mod>.py` as `__file__`. The layer directories are therefore reachable by some out-of-band resolution rather than by being on the interpreter's search path.
Why: any handler or library that reasons about `sys.path` rather than just importing behaves differently against the emulator — appending/prepending a path and expecting layer precedence, computing a package root from `sys.path`, `site`/`.pth` processing, `importlib.metadata`/`pkg_resources` distribution discovery, or vendoring checks that assert a layer directory precedes another. Shadowing order between a layer's site-packages copy and its `python/` root copy cannot be relied on, because the mechanism that would decide it is not present.
Fix: the Lambda Python execution-environment bootstrap (the component that builds the child interpreter's PYTHONPATH / sys.path before the handler module is imported) must place the attached layers' `/opt/python/lib/python<major>.<minor>/site-packages` entry, then `/opt/python`, on sys.path for the function's configured runtime minor version.
Traces: lambda.layer-python-site-packages-dir-importable-and-precedes-python-root.

Cause 2 of 2: layer zip entries for another Python minor version are not extracted to /opt.
AWS: a layer's archive is extracted into `/opt` in full, verbatim; the runtime's search path is the only thing that decides what is IMPORTABLE. So a layer attached to a python3.12 function that ships `python/lib/python3.11/site-packages/<mod>.py` leaves that file on disk at `/opt/python/lib/python3.11/site-packages/<mod>.py` (readable by `os.path.isfile`), while `import <mod>` fails with errorType `ModuleNotFoundError` and errorMessage `No module named '<mod>'` — the interpreter's own message, with the module name taken from the request. Recorded behavior only; the extraction rule is documented, not modelled.
Emulator: that file is never written — `os.path.isfile("/opt/python/lib/python3.11/site-packages/<mod>.py")` returns False — while the python3.12 sibling in the SAME layer does resolve. Extraction is being filtered by the function's runtime minor version instead of extracting every entry.
Why: layers are routinely used to ship non-importable payloads — CA bundles, model/config/data files, binaries, wheels for several minors, `bin/` and `lib/` trees — read by absolute path rather than imported. Against the emulator every such path under a directory that does not match the function's runtime minor silently does not exist, so the handler fails on a missing file instead of reading it, and the AWS contract 'extracted but not importable' collapses into 'not there'.
Fix: the layer-extraction step of the execution-environment provisioner — unpack all entries of every attached layer archive under /opt and let sys.path alone decide importability; do not filter entries by the function's runtime version.
Traces: lambda.layer-python-site-packages-for-other-minor-version-not-importable-rejected.

## Evidence
Cause 1 of 2 — check lambda.layer-python-site-packages-dir-importable-and-precedes-python-root [mismatch].
Setup: one layer (CompatibleRuntimes=[python3.12]) shipping `python/lib/python3.12/site-packages/sim2real_l8_sitepkg.py`, `python/lib/python3.12/site-packages/sim2real_l8_both.py` and `python/sim2real_l8_both.py`; a python3.12 function attaches it and the handler returns `[p for p in sys.path if p.startswith("/opt")]`.
Expected (AWS): the first two /opt entries are `['/opt/python/lib/python3.12/site-packages', '/opt/python']`.
Actual (emulator):
  AssertionError: sys.path must list the layer's site-packages directory, then /opt/python; the /opt entries were []
The import assertions ahead of it passed — both modules imported and reported `/opt/python/lib/python3.12/site-packages/...` as `__file__`, and the site-packages copy won the shadowing — so the divergence is exactly that no /opt entry is on sys.path.

Cause 2 of 2 — check lambda.layer-python-site-packages-for-other-minor-version-not-importable-rejected [mismatch].
Setup: one layer (CompatibleRuntimes=[python3.12]) shipping `python/lib/python3.12/site-packages/sim2real_l8_py312.py` AND `python/lib/python3.11/site-packages/sim2real_l8_py311.py`; a python3.12 function attaches it.
Expected (AWS): importing `sim2real_l8_py312` succeeds from `/opt/python/lib/python3.12/site-packages/sim2real_l8_py312.py` (this passed); `os.path.isfile('/opt/python/lib/python3.11/site-packages/sim2real_l8_py311.py')` is True; importing `sim2real_l8_py311` fails with FunctionError `Unhandled`, errorType `ModuleNotFoundError`, errorMessage `No module named 'sim2real_l8_py311'`.
Actual (emulator):
  AssertionError: the layer must still extract /opt/python/lib/python3.11/site-packages/sim2real_l8_py311.py (only the import is refused); got {'onDisk': False}

## Steps to Reproduce
Cause 1 of 2:
1. PublishLayerVersion with CompatibleRuntimes=["python3.12"] and a zip containing `python/lib/python3.12/site-packages/mod_a.py`, `python/lib/python3.12/site-packages/mod_both.py` and `python/mod_both.py` (the two `mod_both` copies carrying distinguishable content).
2. CreateFunction: Runtime python3.12, Handler index.handler, Layers=[that layer version ARN]; handler returns `{"opt_path": [p for p in sys.path if p.startswith("/opt")]}` plus the `__file__` of each imported module.
3. Invoke. Emulator: `opt_path` is `[]`. AWS: it starts with `/opt/python/lib/python3.12/site-packages` then `/opt/python`.

Cause 2 of 2:
1. PublishLayerVersion with CompatibleRuntimes=["python3.12"] and a zip containing BOTH `python/lib/python3.12/site-packages/mod_312.py` and `python/lib/python3.11/site-packages/mod_311.py`.
2. CreateFunction: Runtime python3.12, Layers=[that layer version ARN]; handler returns `{"onDisk": os.path.isfile(event["probe"])}` for a probe path, and otherwise imports `event["module"]`.
3. Invoke with `{"probe": "/opt/python/lib/python3.11/site-packages/mod_311.py"}`. Emulator returns `{"onDisk": false}`; AWS returns `{"onDisk": true}`.
4. Invoke with `{"module": "mod_311"}` — on AWS this still fails with ModuleNotFoundError: No module named 'mod_311', which is the behavior the extracted-but-not-importable contract requires.
