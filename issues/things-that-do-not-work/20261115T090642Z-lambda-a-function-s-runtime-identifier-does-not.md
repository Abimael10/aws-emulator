Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.python-runtime-family-every-supported-minor-runs-identically
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.python-runtime-family-every-supported-minor-runs-identically|run#6|AssertionError|assert got["version"] == want_version, (
Issue-Commit: 48ab13f61564abe631f24038b106fee51cab80bf

# lambda: a function's Runtime identifier does not select the Python interpreter — a python3.11 function runs on Python 3.14, so the whole python3.x family is one interpreter

> _Note: this issue was identified against the code base at commit `48ab13f61564abe631f24038b106fee51cab80bf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: the Lambda invoke provisioner ignores the python minor version in the function's Runtime and runs every python3.x function on one fixed interpreter.
AWS: Smithy: CreateFunction/UpdateFunctionConfiguration carry Runtime, an enum whose values include python3.11, python3.12, python3.13 and python3.14 as distinct members, and GetFunctionConfiguration echoes the one chosen. The model does not describe the execution environment, so: recorded behavior only — a managed python3.x runtime is CPython at exactly that minor version, so a handler in a function created with Runtime=python3.11 observes sys.version_info[:2] == (3, 11), one created with python3.12 observes (3, 12), and so on for every supported identifier (https://docs.aws.amazon.com/lambda/latest/dg/lambda-runtimes.html). The requirement is the correspondence itself: the interpreter version a handler sees equals the minor version named by its Runtime, on either architecture.
Emulator: the function is created and Invoke answers 200 with no X-Amz-Function-Error — the handler runs, but under the wrong interpreter. A function created with Runtime=python3.11 (arm64) reported sys.version_info[:2] == [3, 14]. The check walks python3.11/python3.12/python3.13/python3.14 and fails on the first identifier whose interpreter is not the fixed one, which is how a single shared interpreter shows up: only the identifier that happens to match the built-in interpreter agrees.
Why: a client pins a runtime precisely to pin language behavior. On the emulator it cannot verify it got the runtime it asked for; version-conditional handler code (sys.version_info branches, syntax or stdlib available only in a newer or older minor, C-extension wheels built for a specific cp3XX ABI) takes the wrong branch or loads the wrong artifact, and a package built and tested for python3.11 is silently executed by 3.14. There is no error anywhere, so nothing signals the substitution.
Fix: the runtime-identifier-to-interpreter selection in the Lambda invoke provisioner (the component that picks the execution environment for a function's Runtime), not CreateFunction's validation or GetFunctionConfiguration's serializer — both already accept and echo the identifier correctly.
Related open issue: 20261111T081112Z-lambda-managed-runtimes-are-stub-interpreters-10-node.md describes the same substitution as its Cause 10, but its replay keys cover only lambda.python-runtime-reports-configured-version (python3.12) and it does not carry the check below, so this failure is not covered by it; closing that issue's Cause 10 should be verified against this check too, because this one exercises the whole python3.x family rather than one identifier.
Traces: lambda.python-runtime-family-every-supported-minor-runs-identically

## Evidence
Check lambda.python-runtime-family-every-supported-minor-runs-identically, [mismatch]:
Expected: for each of (python3.11, arm64), (python3.12, x86_64), (python3.13, arm64), (python3.14, x86_64), a handler returning list(sys.version_info[:2]) reports the minor version named by its Runtime.
Actual: AssertionError: python3.11: sys.version_info[:2] was [3, 14], expected [3, 11]
CreateFunction and Invoke both succeeded (no refusal, no X-Amz-Function-Error); the divergence is only in which interpreter ran. The specific pairing python3.11/3.14 is this run's first mismatch, not the requirement — the requirement is that every supported python3.x identifier yields its own minor version.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite check lambda.python-runtime-family-every-supported-minor-runs-identically.
2. CreateFunction with Runtime=python3.11 (Architectures=[arm64]) and a ZipFile handler that returns {"version": list(sys.version_info[:2]), "machine": platform.machine()}.
3. Wait for State=Active, then Invoke it (RequestResponse) and read the payload: "version" is [3, 14] instead of [3, 11].
4. Repeat with Runtime=python3.12, python3.13 and python3.14 — every identifier reports the same fixed interpreter version, so only the one that coincides with it agrees.
5. Re-replay exactly the check id in trace_ids to confirm a fix.
