Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.python-runtime-reports-configured-version
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.python-runtime-reports-configured-version|run#5|AssertionError|assert out["python_implementation"] == "CPython", (
Issue-Commit: 7ddb4188dd9af5c3d292129d1524d0998b9f045b

# The python3.12 managed runtime is not CPython: platform.python_implementation() and sys.implementation.name report RustPython

> _Note: this issue was identified against the code base at commit `7ddb4188dd9af5c3d292129d1524d0998b9f045b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: a function configured with Runtime=python3.12 runs on an interpreter that identifies itself as RustPython, so the runtime identity a handler reads is wrong.

AWS: the python3.12 managed runtime is CPython 3.12 (Lambda developer guide, "Lambda runtimes"). A handler therefore observes sys.version_info[:2] == (3, 12), platform.python_implementation() == 'CPython' and sys.implementation.name == 'cpython'. Smithy: lambda CreateFunction's Runtime member is the Runtime enum containing python3.12; the enum names the runtime, and the interpreter identity behind it is documented rather than modelled, so this is documented behavior plus the recording.

Emulator: the handler reports platform.python_implementation() == 'RustPython' for a python3.12 function. The version numbers were accepted, so it is specifically the implementation identity that diverges (and with it sys.implementation.name).

Why: code and libraries that branch on the interpreter — platform.python_implementation()/sys.implementation.name checks, CPython-only C-extension or ctypes paths, implementation-specific workarounds and telemetry that reports the runtime — take the wrong branch or refuse to run, and a client cannot trust that what passes locally on CPython 3.12 behaves the same here. This is adjacent to the open issue 20261111T081112Z-lambda-managed-runtimes-are-stub-interpreters-10-node.md (managed runtimes being stub interpreters), which does not carry this check's replay key; the runtime-identity assertion is filed here.

Fix: the Lambda managed python runtime — the interpreter the python3.1x runtimes execute handlers on (and, failing that, the runtime identity it reports through platform/sys).

Traces: lambda.python-runtime-reports-configured-version.

## Evidence
AssertionError: platform.python_implementation() must be 'CPython', got 'RustPython'

Handler: returns {"version": list(sys.version_info[:2]), "python_implementation": platform.python_implementation(), "implementation_name": sys.implementation.name} from a Runtime=python3.12 function.
Expected (AWS): version [3, 12], python_implementation 'CPython', implementation_name 'cpython'.
Actual (emulator): python_implementation 'RustPython' (the version check passed before this assertion).

Corroborating in the same service, already owned by other issues: lambda.python-sqlite3-in-memory-query fails with "Unable to import module 'index': No module named '_sqlite3'", which is the same non-CPython interpreter showing through.

## Steps to Reproduce
1. CreateFunction with Runtime=python3.12, Handler=index.handler and a zip whose index.py returns platform.python_implementation() and sys.implementation.name.
2. Invoke it (RequestResponse) and read the payload.
3. Observe python_implementation == 'RustPython' instead of 'CPython'.
