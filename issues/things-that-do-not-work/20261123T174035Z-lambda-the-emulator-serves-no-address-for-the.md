Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.python-requirements-txt-native-wheel-imports lambda.python-requirements-txt-pure-dependency-tree-imports lambda.python-requirements-txt-pydantic-validates
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.python-requirements-txt-native-wheel-imports|_http_get#1|EgressRefused|with urllib.request.urlopen(urllib.request.Request(url), timeout=120) as resp:
Issue-Key: lambda.python-requirements-txt-pure-dependency-tree-imports|_http_get#1|EgressRefused|with urllib.request.urlopen(urllib.request.Request(url), timeout=120) as resp:
Issue-Key: lambda.python-requirements-txt-pydantic-validates|_http_get#1|EgressRefused|with urllib.request.urlopen(urllib.request.Request(url), timeout=120) as resp:
Issue-Commit: f9dc389755539ec737bd6c84507d1519aa5a1660

# lambda: the emulator serves no address for the public Python package index, so every requirements.txt-built python deployment package check is refused before any Lambda call (3 checks)

> _Note: this issue was identified against the code base at commit `f9dc389755539ec737bd6c84507d1519aa5a1660`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFunction

## Description
Cause 1 of 1: the run can reach only the emulator's own endpoint, and the emulator announces/serves no address for the public Python package index host (pypi.org), so the three packaging checks that build a python deployment package from a pinned requirements.txt are refused at their build step and never issue a single Lambda call. Failing check ids: lambda.python-requirements-txt-native-wheel-imports, lambda.python-requirements-txt-pure-dependency-tree-imports, lambda.python-requirements-txt-pydantic-validates.

AWS: recorded behavior only — the Lambda Smithy model says nothing about how a deployment package is built. The recordings build the package exactly as `pip install -r requirements.txt -t .` does: they fetch each pinned wheel from the public index at https://pypi.org/pypi, verify it against its pinned sha256, zip it with the handler, then CreateFunction on python3.12 and Invoke, asserting the vendored distributions import and run inside the sandbox (a native-wheel distribution, a pure-python dependency tree, and pydantic validating a model).

Emulator: the fetch is refused before any AWS operation: `CapabilityNotOffered: the target serves no address for pypi.org` / `simparity.egress.EgressRefused: egress refused: 'pypi.org' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator`. All three checks fail identically at the same point, with no CreateFunction, no Invoke and therefore no response from the emulator to compare.

Why: no python packaging path can be exercised. A client that vendors dependencies the documented way (pip into the package root) cannot get a function created, so nothing about import resolution, native extension loading or the sandbox's site-packages layout is reachable.

Fix: the emulator's endpoint/egress surface — the host the index is reached on must be the endpoint under test (i.e. the emulator must serve the package-index address it expects clients to fetch from) rather than a third-party address no run can reach.

Traces: lambda.python-requirements-txt-native-wheel-imports, lambda.python-requirements-txt-pure-dependency-tree-imports, lambda.python-requirements-txt-pydantic-validates.

## Evidence
Expected (AWS/recording): the pinned wheels are fetched from https://pypi.org/pypi, each matching its pinned sha256; the handler plus vendored distributions are zipped, CreateFunction(Runtime=python3.12) succeeds and Invoke returns the handler's result proving the vendored distributions import.
Actual (emulator): `CapabilityNotOffered: the target serves no address for pypi.org`, raised as `simparity.egress.EgressRefused: egress refused: 'pypi.org' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on`. Identical for all three checks; no Lambda operation is reached.

## Steps to Reproduce
1. Replay lambda.python-requirements-txt-native-wheel-imports (also -pure-dependency-tree-imports and -pydantic-validates).
2. Each check first resolves its pinned distributions from https://pypi.org/pypi to build the deployment package.
3. Observe the egress refusal (`the target serves no address for pypi.org`) before CreateFunction is called.
