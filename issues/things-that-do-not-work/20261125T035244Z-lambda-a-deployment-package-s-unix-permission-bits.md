Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.package-file-modes-normalized-so-every-file-is-readable-and-executable
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.package-file-modes-normalized-so-every-file-is-readable-and-executable|run#10|AssertionError|assert got["mode"] == "0o755", (
Issue-Commit: 643d7fc40de53dee6cfee0ebba73d0a58d0d5505

# lambda: a deployment package's Unix permission bits are not normalized on extraction — /var/task files read back 0o555 instead of 0o755

> _Note: this issue was identified against the code base at commit `643d7fc40de53dee6cfee0ebba73d0a58d0d5505`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFunction

## Description
Cause 1 of 1: the function package's file modes are not normalized to 0755 when the ZipFile is extracted into /var/task.

AWS:      recorded behavior only — Smithy (com-amazonaws-lambda-awsgirapiservice) models CreateFunction's Code.ZipFile as opaque blob content and says nothing about permission bits, so the requirement comes from the recording and from AWS's deployment-troubleshooting guidance: Lambda does NOT keep the Unix mode a .zip records in its entry's external_attr. Every regular file extracted into /var/task reads back mode 0755, and an implicitly created directory reads back 0755. That is what makes a script packaged 0644, 0600 or even 0000 both readable and directly executable by the non-root sandbox user, and it is observable from inside the handler with os.stat("/var/task/<rel>").

Emulator: the packaged bits are carried through to the sandbox instead of being normalized, and are additionally stripped of write permission: a file packaged 0o100755 reads back 0o555 from the handler. Since the normalization is absent, the lower-moded entries in the same package (0644, 0600, 0000) and the implicit directory cannot read back 0755 either — the first assertion simply stops at exec.sh.

Why:      A client relying on AWS's behavior gets two things wrong. First, a handler that execs or opens a file it shipped at 0644/0600/0000 — a wrapper script, a bundled binary, a data file written with restrictive umask by the build — fails with EACCES on the emulator where it runs on AWS. Second, code that stats or rewrites its own package files sees different modes, so permission-sensitive bootstrap logic and self-tests take a different path. Because the bits are read-only (0o555), anything that chmods or temporarily rewrites a packaged file also diverges.

Fix:      The deployment-package extraction / sandbox materialization step of the lambda service crate (the path that unpacks Code.ZipFile into the execution environment's /var/task): after extracting, set every regular file to 0755 and every created directory to 0755, ignoring the zip entry's external_attr permission bits entirely, and keep entries as regular files.

Related but NOT covering this: open issue 20261119T204102Z-lambda-8-net-new-causes-layer-content-is.md is about LAYER content extraction under /opt; this cause is the FUNCTION package under /var/task and is a separate extraction path.

Traces:   lambda.package-file-modes-normalized-so-every-file-is-readable-and-executable.

## Evidence
Failing check lambda.package-file-modes-normalized-so-every-file-is-readable-and-executable, [mismatch]:

AssertionError: exec.sh was packaged 0o755 and must read back 0o755; got '0o555'

The check builds a zip whose entries carry explicit Unix modes in external_attr (create_system=3):
  exec.sh 0o100755, plain.sh 0o100644, owner_only.sh 0o100600, no_bits.sh 0o100000, nested/dir/data.txt 0o100600
creates a python3.12 function from it, and the handler reports oct(stat.S_IMODE(os.stat("/var/task/<rel>").st_mode)) plus the mode of the implicit directory /var/task/nested/dir, and execs each *.sh.

Expected (AWS): every regular file reads back 0o755, the implicit directory reads back 0o755, every *.sh exits 0 printing its marker, and every file is readable with its exact bytes.
Actual (emulator): exec.sh reads back 0o555 (write bit stripped, not normalized to 0755); the remaining modes and the execution legs are not reached.
The handler also asserts os.getuid() != 0 first, so the sandbox is non-root and the mode genuinely decides access.

## Steps to Reproduce
1. Build a zip with zipfile.ZipInfo entries whose create_system=3 and external_attr = mode << 16, for modes 0o100755, 0o100644, 0o100600, 0o100000 and a nested path "nested/dir/data.txt".
2. CreateRole (lambda.amazonaws.com trust), then CreateFunction(Runtime="python3.12", Handler="index.handler", Code={"ZipFile": <zip>}, Timeout=30, MemorySize=128).
3. Invoke the function with {"files": [...the packaged relative paths...]}; the handler returns oct(stat.S_IMODE(os.stat("/var/task/"+rel).st_mode)) per file, the mode of /var/task/nested/dir, the file contents, and subprocess.run([path]) results for each *.sh.
   Expected: every file mode "0o755", dir mode "0o755", each *.sh exits 0 with its marker on stdout.
   Observed: exec.sh mode "0o555".
