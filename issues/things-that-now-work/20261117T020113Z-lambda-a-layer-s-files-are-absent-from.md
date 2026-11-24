Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.deep-lambda-runtime-truth-layer-files-byte-exact-at-opt-and-opt-write-rejected
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.deep-lambda-runtime-truth-layer-files-byte-exact-at-opt-and-opt-write-rejected|run#12|AssertionError|assert set(inv) == set(expected), (
Issue-Commit: c31be09f961a7f044ea59076daac7fb377aa83e8

# lambda: a layer's files are absent from /opt at invoke time — an inventory of /opt is empty, so none of the packaged layer paths resolve

> _Note: this issue was identified against the code base at commit `c31be09f961a7f044ea59076daac7fb377aa83e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PublishLayerVersion

## Description
One net-new root cause.

Cause 1 of 1: the files carried by an attached layer version are never materialized into the execution environment's /opt, so the guest's inventory of /opt is missing every packaged path.

AWS: recorded behavior only for the filesystem layout (the layer content is modelled via PublishLayerVersion/GetLayerVersion but /opt is a runtime-contract fact documented in the Lambda layers guide). AWS extracts every attached layer's zip into /opt before the handler's init runs, so a layer zip containing `python/<module>.py`, `<dir>/data.bin`, `<dir>/deep/a/b/c.txt` and `<dir>/tool.sh` yields exactly those paths under /opt, byte-for-byte (same size and sha256 as packaged), with the packaged permission bits; /opt itself is read-only, so a write into it is refused with EROFS/EACCES.

Emulator: the guest's walk of /opt reports NONE of the packaged paths — `extra=[] missing=['python/sim2real_layer_mod.py', 'sim2real/data.bin', 'sim2real/deep/a/b/c.txt', 'sim2real/tool.sh']`. `extra=[]` shows the walk found no unexpected files either, i.e. /opt holds nothing from the layer at all. Because the files are absent, the byte-exactness, permission-bit, layer-executable and layer-module-import assertions downstream, and the read-only-/opt refusal, are all unreachable.

Why: a client relying on AWS's behavior ships shared code, data files and helper binaries in a layer and reads them from /opt (or imports them via /opt/python). Against the emulator every such function fails at import or at first file read, and the read-only property of /opt is unverifiable, so layers are inert.

Fix: the execution-environment provisioner in the Lambda invoke path — it must fetch each attached layer version's content, extract it into /opt preserving relative paths, sizes, bytes and mode bits before init, and mount /opt read-only.

Traces: lambda.deep-lambda-runtime-truth-layer-files-byte-exact-at-opt-and-opt-write-rejected. The open issue 20261107T091747Z-lambda-lambda-layers-are-never-materialized-into-the.md describes the same area but does NOT carry this check's replay key; this check is unowned in the replay report and is filed here so it is re-replayed.

## Evidence
Expected (AWS): the guest's inventory of /opt contains exactly the packaged relative paths — python/<module>.py, <dir>/data.bin, <dir>/deep/a/b/c.txt, <dir>/tool.sh — each with the packaged size, sha256 and mode bits; a write into /opt is refused (read-only filesystem).

Actual (emulator): AssertionError: `/opt does not hold exactly the packaged paths: extra=[] missing=['python/sim2real_layer_mod.py', 'sim2real/data.bin', 'sim2real/deep/a/b/c.txt', 'sim2real/tool.sh']` — /opt contains none of them (the names shown are this run's packaged paths; the requirement is that whatever the layer zip contains appears under /opt).

## Steps to Reproduce
1. PublishLayerVersion with a zip containing `python/<module>.py`, `<dir>/data.bin`, `<dir>/deep/a/b/c.txt` and an executable `<dir>/tool.sh`.
2. CreateFunction (python3.12) with Layers=[<that layer version arn>] and a handler that walks /opt recording each regular file's path, size, sha256 and mode.
3. Invoke the function.
Observed: the returned inventory is empty — none of the packaged paths are present. Expected: exactly those paths, byte-exact, with the packaged mode bits; and a write into /opt refused as a read-only filesystem.
