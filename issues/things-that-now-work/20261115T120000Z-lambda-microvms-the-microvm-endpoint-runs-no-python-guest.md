Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda-microvms.suspend-resume-preserves-guest-memory-and-process-identity
Issue-Shape-Id: com.amazonaws.lambdamicrovms#LambdaMicrovms
Issue-Key: lambda-microvms.suspend-resume-preserves-guest-memory-and-process-identity|_get_json#10|AssertionError|assert status == 200, f"GET {url!r} answered HTTP {status} with {body[:200]!r}"
Issue-Commit: 5bdc422e571ff089197010dd8b23a6f4d1e72235

# lambda-microvms the MicroVM endpoint runs no PYTHON guest: an image whose code artifact is a Python program answers HTTP 502 for every path, because the emulator's guest engine runs JavaScript only

> _Filed by the fixer of `20261108T144919Z-lambda-microvms-microvm-endpoint-refuses-a-validly-minted.md`, which carried this check's replay key. That issue's two other checks are resolved: the endpoint now decides a token's `allowedPorts` against the port the request addresses, and a MicroVM runs its guest as ONE process whose memory, disk and published identity survive its requests and a `SuspendMicrovm` + `ResumeMicrovm`. This check is NOT resolved and nothing reports it as working — it needs a capability the emulator does not have yet, which is why it is filed on its own._

## Operation
RunMicrovm

## Description
CAUSE COUNT: 1.

--- Cause 1 of 1: a MicroVM whose image's code artifact holds a PYTHON program runs no guest, so every authenticated request to its published endpoint is answered with the proxy's own "reached nothing inside" status instead of the guest's response ---

AWS: `RunMicrovmResponse$endpoint` (Smithy: `RunMicrovm`, member `endpoint` String @length 1..2048 @required) is "The HTTPS endpoint URL for communicating with the MicroVM. Include a valid authentication token in the X-aws-proxy-auth header when sending requests." The model says nothing about the language the image's code is written in: `CodeArtifact$uri` is "The URI of the code artifact in Amazon S3", and what runs inside the MicroVM is whatever the image's container specification starts. Recorded behavior only: a GET to the published endpoint of a RUNNING MicroVM carrying a granting token is proxied into the MicroVM and the caller reads the GUEST's own HTTP response — here HTTP 200 with the JSON a Python app serves on port 8080 under `/id`, `/set` and `/get`, whose `/id` reports the guest process's identity (`boot_id`, `pid`, `starttime`) unchanged across a suspend and resume, with the in-memory and on-disk values written before the suspend still readable after it.

Emulator: the MicroVM is RUNNING and the token is granted, but the image version holds no guest program this emulator can run, so the endpoint answers `502` with the proxy's own sentence `The MicroVM returned no response.\n` for every guest path. The reason is the LANGUAGE of the artifact, not the endpoint: `services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/src/state/guest_program.rs` keeps only the entries the embedded engine can compile (`JAVASCRIPT_SUFFIXES`: `.js`, `.mjs`, `.cjs`, `.json`), and `src/state/guest_runtime.rs` runs them in the QuickJS engine this crate embeds (`rquickjs`). The check's artifact is a `.zip` holding `app.py` (1642 bytes) and a `Dockerfile` (165 bytes) — no entry the JavaScript engine can load — so `guest_program::read` yields no program, the MicroVM's record holds a machine but no process, and the endpoint reports a MicroVM it reached nothing inside of. A MicroVM whose artifact is a Node-shaped program is served end to end today (`lambda-microvms.microvm-auth-token-grants-endpoint-access` and `lambda-microvms.suspend-resume-microvm-transitions` both pass), which isolates this to the missing interpreter.

Why: a client that runs a MicroVM from a Python image — the ordinary case for this service, whose base images are Amazon Linux — can never read its guest, so nothing of its data plane is observable: not the guest's own response, not what a suspend preserves, not the process identity the platform publishes to it. Every arm of such a test gets the identical 502, and the failure says nothing about the language being the reason.

Fix: a SECOND execution substrate for a MicroVM guest, beside the JavaScript one. The project's code-execution SOP names the engine to use (RustPython: `rustpython-vm` / `rustpython-stdlib` / `rustpython-pylib` with the pins it lists, `default-features = false`, a native-module allow-list, the emulator's own `posix`/`os`/`_thread`, a trace-hook step and deadline bound, and memory sampling) and says to land execution and containment first, then the bounds. Two pieces are specific to this service and are what make a Python guest reachable at all:

1. `guest_program.rs` must read a Python entry the way it reads a JavaScript one — the module the image's `Dockerfile` command names (`CMD ["python3", "app.py"]`), then the single module of a one-module artifact — and the record must say which engine the program is for, so the endpoint hands it to that engine.
2. the guest is an HTTP SERVER, not a function: a Python app serving with `http.server` / `socketserver` expects a socket. With no host socket reachable (the SOP's containment), the proxied request has to arrive at the app through the emulator's own seam — a `socket`-shaped module of this crate that hands the request in and takes the response out, in the same shape `src/state/guest_runtime.rs` already implements for the JavaScript `http` module, and the machine (`src/state/guest_machine.rs`) already publishes the `/proc` identity, the environment and the filesystem such a guest reads through `os` and `open`.

Traces: lambda-microvms.suspend-resume-preserves-guest-memory-and-process-identity.

## Evidence
Replay of the behavioral suite against the emulator built from this checkout, with the MicroVM endpoint's port-scope and one-process fixes in place:

[mismatch] lambda-microvms.suspend-resume-preserves-guest-memory-and-process-identity
  token minted with expirationInMinutes=30, allowedPorts=[{"allPorts": {}}]; headers = {"X-aws-proxy-auth": <token>}
  check_..._suspend_resume_preserves_guest_memory_and_process_identity.py line 129 -> line 232:
  AssertionError: GET 'https://127.0.0.1:<port>/microvms/<microvm id>/id' answered HTTP 502 with 'The MicroVM returned no response.\n'
  expected: HTTP 200 with the guest's JSON from /id ({"boot_id": ..., "pid": ..., "starttime": ...}), unchanged across SuspendMicrovm + ResumeMicrovm, and /get returning the values written through /set before the suspend.
  actual: HTTP 502 for every guest path, because the image version holds no program the emulator's engine can run.
  (host, port and the microvm id are per-run values and are not requirements.)

What the artifact holds, read with a temporary diagnostic on `serve_request` (removed again): `key="code.zip" bytes=2013 entries=[("app.py", 1642), ("Dockerfile", 165)]`, and the version's stored guest is `None`.

Source: services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/src/state/guest_program.rs (`JAVASCRIPT_SUFFIXES`, `is_loadable`, `resolve_entry`), src/state/guest_runtime.rs (the QuickJS engine and the modules it implements), src/state/microvm_endpoint.rs (the `NO_GUEST_RESPONSE_STATUS` answer for a MicroVM with no process to hand the request to).

## Steps to Reproduce
1. Stage a `.zip` holding a `Dockerfile` whose command starts a Python program (`CMD ["python3", "app.py"]`) and an `app.py` that serves HTTP on port 8080 — `/id` reporting `os.getpid()`, `/proc/sys/kernel/random/boot_id` and field 22 of `/proc/self/stat`, `/set` keeping a value in memory and on disk, `/get` reporting both.
2. CreateMicrovmImage with `codeArtifact.uri` pointing at that object; poll GetMicrovmImage until the image is CREATED with an active version.
3. RunMicrovm from it; poll GetMicrovm until state=RUNNING and read `endpoint`.
4. CreateMicrovmAuthToken(microvmIdentifier=<id>, expirationInMinutes=30, allowedPorts=[{allPorts:{}}]) and GET `<endpoint>/id` with `X-aws-proxy-auth: <token>`.
   Expected: HTTP 200 with the guest's own JSON.
   Observed: HTTP 502 `The MicroVM returned no response.`
5. Repeat step 1 with a Node-shaped `server.js` and `CMD ["node", "server.js"]`: the same GET answers 200 with that guest's body, which shows the endpoint, the token and the machine are not what refuses the Python one.
