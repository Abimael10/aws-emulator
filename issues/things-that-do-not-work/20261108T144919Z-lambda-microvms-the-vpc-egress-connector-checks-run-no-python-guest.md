Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda-core__lambda-microvms.microvm-denied-the-vpc-egress-connector-cannot-reach-the-private-subnet lambda-core__lambda-microvms.vpc-egress-connector-path-is-blocked-by-the-target-security-group
Issue-Key: lambda-core__lambda-microvms.microvm-denied-the-vpc-egress-connector-cannot-reach-the-private-subnet|_guest_get#25|AssertionError|assert status == 200, (
Issue-Key: lambda-core__lambda-microvms.vpc-egress-connector-path-is-blocked-by-the-target-security-group|_guest_get#26|AssertionError|assert status == 200, (
Issue-Commit: 5bdc422e571ff089197010dd8b23a6f4d1e72235

# lambda-microvms the two lambda-core VPC-egress connector checks still answer HTTP 502: their image's guest is a PYTHON program, and this emulator embeds no engine that runs one

> _This report carries the two replay keys that do NOT pass, re-filed here so they are not recorded as working. The report they came from
> (`issues/things-that-now-work/20261108T144919Z-lambda-core-lambda-microvms-lambda-core-vpc-egress-connector-reachability-cannot-be.md`) is resolved for the layers it was filed about — the endpoint serves a guest's own response, the VPC_EGRESS connector decides a guest's outbound connection, and an image that declares its guest only through its container specification's `RUN` steps now runs — and this is the one layer of it that is not._

## Operation
RunMicrovm

## Description
CAUSE COUNT: 1.

--- Cause 1 of 1: the code artifact of the image both checks run declares a PYTHON guest, and the only execution substrate this crate embeds is the JavaScript one, so every authenticated request to the MicroVM's published endpoint is answered with the proxy's own "reached nothing inside" status ---

AWS: `RunMicrovmResponse$endpoint` (Smithy: `com.amazonaws.lambdamicrovms#RunMicrovm`, member `endpoint` String @length 1..2048 @required) is "The HTTPS endpoint URL for communicating with the MicroVM. Include a valid authentication token in the X-aws-proxy-auth header when sending requests", and `CreateMicrovmAuthToken` mints the value for that header. The model says nothing about the LANGUAGE of the program inside the image — `CodeArtifact$uri` is only "The URI of the code artifact in Amazon S3" — so what runs is whatever the image's container specification builds and starts. Recorded behavior only: an authenticated GET to a RUNNING MicroVM answers 2xx with the GUEST's own body.

Emulator: both checks reach a RUNNING MicroVM with a minted all-ports token and then read HTTP 502 `The MicroVM returned no response.` The reason is the LANGUAGE of the artifact. The artifact is a one-entry `.zip` whose only member is a `Dockerfile` of this shape:

    FROM public.ecr.aws/amazonlinux/amazonlinux:2023
    RUN dnf install -y python3 && dnf clean all
    RUN mkdir -p /srv
    RUN echo <base64 of the guest program> | base64 -d > /srv/<module>.py
    CMD ["/bin/sh","-c","python3 /srv/<module>.py"]

The build's steps are now performed and what they write is part of the image, and the command's module is resolved over those files (`src/state/image_build.rs`, `src/state/guest_program.rs`), so the version DOES hold the program — but its entry is a `.py` module, and `src/state/guest_runtime.rs` dispatches it to `src/state/guest_python.rs`, which reports a MicroVM nothing was reached inside of because no Python engine is embedded here.

The program itself is an ordinary `http.server` guest: it imports `errno`, `json`, `socket` and `urllib.parse`, subclasses `BaseHTTPRequestHandler` with `protocol_version = "HTTP/1.1"`, answers `/health` with `{"healthy": true}`, answers `/connect?host=&port=` by calling `socket.create_connection((host, port), timeout=8)` and reporting `{"connected": true, "outcome": "connected", "peer": list(sock.getpeername()), "local": list(sock.getsockname())}` or, for a connection that raised, `{"connected": false, "outcome": <timed_out|refused|unreachable|error:<class>>, "detail": str(exc)[:200]}`, and serves it with `HTTPServer(("0.0.0.0", 8080), H).serve_forever()`.

Fix: a SECOND execution substrate for a MicroVM guest, beside the JavaScript one — the same capability `issues/things-that-do-not-work/20261115T120000Z-lambda-microvms-the-microvm-endpoint-runs-no-python-guest.md` owns and states the engine for (the project's code-execution SOP names RustPython, with the native-module allow-list, the emulator's own `posix`, the trace-hook step and deadline bounds). Two pieces are what make these two checks reachable once that engine stands up:

1. the guest is an HTTP SERVER, not a function: the proxied request has to arrive at it through this crate's own seam — a `socket`-shaped module and an `http.server`-shaped module of this crate, in the same shape `src/state/guest_runtime.rs` already implements for the JavaScript `http`/`net` modules;
2. a connection the guest's `socket` opens has to be decided by `src/state/microvm_network.rs`, which already answers that question for the JavaScript substrate: an uncarried connection must raise `TimeoutError` (which is `socket.timeout`), and a carried one must report the MicroVM's link-local address as its local end.

An attempt at that substrate in this crate did not stand its interpreter up (`import encodings` recursed inside it) and was removed rather than submitted as an engine that does not run, so nothing here reports success for a Python guest.

Traces: lambda-core__lambda-microvms.microvm-denied-the-vpc-egress-connector-cannot-reach-the-private-subnet, lambda-core__lambda-microvms.vpc-egress-connector-path-is-blocked-by-the-target-security-group.

## Evidence
Replay of the two units against the emulator built from this checkout:

[mismatch] lambda-core__lambda-microvms.microvm-denied-the-vpc-egress-connector-cannot-reach-the-private-subnet
  AssertionError: GET '/connect?host=<target>&port=443' on microvm <id> answered HTTP 502: b'The MicroVM returned no response.\n'

[mismatch] lambda-core__lambda-microvms.vpc-egress-connector-path-is-blocked-by-the-target-security-group
  AssertionError: GET '/connect?host=<target>&port=443' on microvm <id> answered HTTP 502: b'The MicroVM returned no response.\n'

  expected: HTTP 200 with the guest's own JSON — `{"connected": true, "outcome": "connected", "peer": [<target>, <port>], "local": [<link-local source inside 169.254.0.0/16>, <port>]}` for the MicroVM on the ACTIVE VPC_EGRESS connector, `{"connected": false, "outcome": "timed_out", ...}` with no `local` member for one attached only to INTERNET_EGRESS or when the target security group admits nothing, and `/health` answering `{"healthy": true}` in both.
  actual: HTTP 502 for every path, because the image's program is Python.

  Everything upstream succeeds in both checks (CreateVpc/CreateSubnet/CreateSecurityGroup/CreateVpcEndpoint, CreateNetworkConnector settling ACTIVE, CreateMicrovmImage settling CREATED/SUCCESSFUL, RunMicrovm settling RUNNING with the expected egressNetworkConnectors, CreateMicrovmAuthToken minting a token). The addresses and MicroVM ids any one run carries are per-run values and are not requirements.
  Source: services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/src/state/guest_python.rs (no engine for a Python guest), src/state/guest_runtime.rs (the JavaScript engine this crate embeds).

## Steps to Reproduce
1. Stage a `.zip` whose ONLY entry is a `Dockerfile` that writes a Python `http.server` program with a `RUN` step and starts it with `CMD ["/bin/sh","-c","python3 /srv/app.py"]`; the program must answer `/health` and `/connect?host=&port=` as above.
2. CreateMicrovmImage with `codeArtifact.uri` pointing at that object; poll GetMicrovmImage until CREATED with an active version.
3. RunMicrovm from it with `egressNetworkConnectors=[<an ACTIVE VPC_EGRESS connector>]`; poll GetMicrovm until state=RUNNING and read `endpoint`.
4. CreateMicrovmAuthToken(expirationInMinutes=15, allowedPorts=[{allPorts:{}}]) and GET `<endpoint>/health` with `X-aws-proxy-auth: <token>`.
   Expected: HTTP 200 with the guest's own JSON.
   Observed: HTTP 502 `The MicroVM returned no response.`
5. Repeat step 1 with a Node-shaped guest written by the same `RUN` step and `CMD ["node", "/server.js"]`: the same GET answers 200 with that guest's body, which shows the endpoint, the token, the build and the connector are not what refuses the Python one.
