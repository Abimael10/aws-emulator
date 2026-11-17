Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda-microvms.microvm-proxy-endpoint-serves-guest-http
Issue-Shape-Id: com.amazonaws.lambdamicrovms#LambdaMicrovms
Issue-Key: lambda-microvms.microvm-proxy-endpoint-serves-guest-http|run#12|AssertionError|assert status == 200, f"the authenticated proxy GET answered HTTP {status}"
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# lambda-microvms MicroVM proxy endpoint answers HTTP 502 for an image whose guest is declared only by its Dockerfile command (python3 -m http.server over files the build's RUN steps created)

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunMicrovm

## Description
CAUSE COUNT: 1.

--- Cause 1 of 1: a MicroVM built from an image whose code artifact declares its guest ONLY through the container spec (a `Dockerfile` with `RUN` steps that materialise content and a `CMD` that starts an interpreter MODULE, not a source file) runs no guest, so every authenticated request to its published endpoint is answered with the proxy's own "reached nothing inside" status instead of the guest's response ---

AWS: Smithy `RunMicrovm` (POST /2025-09-09/microvms) -> `RunMicrovmResponse$endpoint` (String, @required, @length 1..2048) is the HTTPS endpoint for communicating with the MicroVM, addressed with a valid token in the `X-aws-proxy-auth` header; `CreateMicrovmAuthToken` (POST /2025-09-09/microvms/{microvmIdentifier}/auth-token) returns `authToken` (TokenParts map, @required) whose part is that header. The model says nothing about the LANGUAGE or the FORM of the program inside the image: `CodeArtifact$uri` is only "The URI of the code artifact in Amazon S3", and what runs inside the MicroVM is whatever the image's container specification builds and starts. Recorded behavior only: for a RUNNING MicroVM run with the managed `HTTP_INGRESS` ingress connector, a GET to `<endpoint>/<path>` carrying a granting token is proxied into the MicroVM on the guest's listening port and the caller reads the GUEST's own HTTP response — here HTTP 200 whose body is the file the build wrote (`/srv/probe.txt`). AWS also serves the unauthenticated arm from the proxy itself (HTTP 403 naming the missing authentication); that arm was never reached here.

Emulator: the image builds to CREATED with a SUCCESSFUL version and the MicroVM reaches RUNNING, the endpoint is a well-formed host and the token mints with the single `X-aws-proxy-auth` part — but the authenticated GET to `<endpoint>/probe.txt` answers HTTP **502** (the endpoint's own no-guest answer, recorded elsewhere in this service as the body `The MicroVM returned no response.\n`) instead of HTTP 200 with the guest's bytes. The artifact in this case holds NO program source at all: it is a one-entry `.zip` whose only member is a `Dockerfile` whose `RUN` lines create `/srv/probe.txt` and `/srv/index.html` and whose command is `CMD ["/bin/sh","-c","cd /srv && python3 -m http.server 8080"]`. The emulator's guest loader keeps only entries its embedded JavaScript engine can compile and resolves an entry only from a file the `Dockerfile` command NAMES, a `package.json` `main`, or a lone module (`src/state/guest_program.rs`: `JAVASCRIPT_SUFFIXES = [".js", ".mjs", ".cjs", ".json"]`, `DEFAULT_ENTRY`, `CONTAINER_SPEC`, `resolve_entry`). A command that starts an interpreter MODULE (`-m http.server`) names no file and the build's filesystem effects (`RUN echo ... > /srv/probe.txt`) are never applied, so the version stores no program, the MicroVM record holds a machine with no process, and `src/state/microvm_endpoint.rs` reports the no-guest 502 for every path.

This is NOT the cause already filed as `20261115T120000Z-lambda-microvms-the-microvm-endpoint-runs-no-python-guest.md` (that issue owns only `lambda-microvms.suspend-resume-preserves-guest-memory-and-process-identity`, an artifact holding an `app.py` the engine cannot compile). Adding a Python interpreter alone does not fix this check: here there is no user program to interpret. What is missing is (a) applying the image build's declared filesystem steps so the content the guest serves exists inside the MicroVM, and (b) resolving a guest whose entry is an interpreter's own module/builtin server rather than a source file in the artifact.

Why: a client that ships the ordinary container-image shape for this service — a `Dockerfile` that bakes content and starts a stock static/HTTP server — gets a MicroVM that reports RUNNING, publishes an endpoint, and mints working tokens, yet whose data plane is permanently unreachable: every path answers an identical 502 that names no reason. The proxy's own authentication behavior cannot be observed either, because the request never gets past the missing guest, so a test of the 403 arm is masked by the 502.

Fix: the MicroVM guest substrate and the image build that feeds it — `src/state/guest_program.rs` (entry resolution for a container command that starts an interpreter module, and retention of the build's non-source content), the image build that applies the container spec's `RUN` steps into the machine's filesystem (`src/state/guest_machine.rs`), and `src/state/microvm_endpoint.rs`, which must hand a proxied request to that guest instead of answering the no-guest 502. No change belongs in `RunMicrovm`'s control-plane handler: its response is already correct.

Traces: lambda-microvms.microvm-proxy-endpoint-serves-guest-http.

## Evidence
Replay of the behavioral suite against the emulator built from this checkout:

[mismatch] lambda-microvms.microvm-proxy-endpoint-serves-guest-http
  check_lambda_microvms_microvm_proxy_endpoint_serves_guest_http.py line 60:
  AssertionError: the authenticated proxy GET answered HTTP 502

Sequence actually executed, all of which succeeded before the divergence:
  - CreateMicrovmImage(name=<unique>, baseImageArn=<first ListManagedMicrovmImages arn>, buildRoleArn=<own role>, codeArtifact.uri=s3://<bucket>/code.zip) -> image CREATED, version SUCCESSFUL
  - code.zip holds ONE entry, `Dockerfile`:
        FROM public.ecr.aws/amazonlinux/amazonlinux:2023
        RUN dnf install -y python3 && dnf clean all
        RUN mkdir -p /srv
        RUN echo sim2real-microvm-dataplane-ok > /srv/probe.txt
        RUN echo sim2real-microvm-dataplane-ok > /srv/index.html
        CMD ["/bin/sh","-c","cd /srv && python3 -m http.server 8080"]
  - RunMicrovm(imageIdentifier=<arn>, maximumDurationInSeconds=600, ingressNetworkConnectors=[arn:aws:lambda:<region>:aws:network-connector:aws-network-connector:HTTP_INGRESS]) -> microvmId, endpoint (well-formed host), state polled to RUNNING
  - CreateMicrovmAuthToken(microvmIdentifier=<id>, expirationInMinutes=15, allowedPorts=[{allPorts:{}}]) -> authToken with exactly the part `X-aws-proxy-auth`

Divergence on the data-plane GET:
  request:  GET <endpoint>/probe.txt with header X-aws-proxy-auth: <token>
  expected: HTTP 200, body `sim2real-microvm-dataplane-ok` (the file the build's RUN step wrote, served by the guest on port 8080 and fronted by the proxy on 443)
  actual:   HTTP 502 — the endpoint's no-guest answer (recorded body in this service: `The MicroVM returned no response.\n`)

Not reached because of the above (same defect, no separate cause): the unauthenticated arm, GET <endpoint>/probe.txt with no header, which AWS refuses at the proxy with HTTP 403 whose body names the missing authentication (`Request missing authentication`). Fixed text there is the refusal sentence; nothing in it is per-run.

Per-run values that are NOT requirements: the host and port of the endpoint, the microvm id, the image arn/version, the bucket and role names, timestamps and request ids.

Source: services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/src/state/guest_program.rs (JAVASCRIPT_SUFFIXES, CONTAINER_SPEC, DEFAULT_ENTRY, resolve_entry), src/state/guest_machine.rs, src/state/microvm_endpoint.rs (the no-guest 502 answer).

## Steps to Reproduce
1. Create an S3 bucket and a build role allowed to read it.
2. Stage a `.zip` whose ONLY entry is a `Dockerfile`:
     FROM public.ecr.aws/amazonlinux/amazonlinux:2023
     RUN dnf install -y python3 && dnf clean all
     RUN mkdir -p /srv
     RUN echo sim2real-microvm-dataplane-ok > /srv/probe.txt
     CMD ["/bin/sh","-c","cd /srv && python3 -m http.server 8080"]
   Put it at s3://<bucket>/code.zip.
3. CreateMicrovmImage(name=..., baseImageArn=<an arn from ListManagedMicrovmImages>, buildRoleArn=<role>, codeArtifact={uri: s3://<bucket>/code.zip}); poll GetMicrovmImage until CREATED and GetMicrovmImageVersion until SUCCESSFUL.
4. RunMicrovm(imageIdentifier=<image arn>, maximumDurationInSeconds=600, ingressNetworkConnectors=["arn:aws:lambda:<region>:aws:network-connector:aws-network-connector:HTTP_INGRESS"]); poll GetMicrovm until state=RUNNING and read `endpoint`.
5. CreateMicrovmAuthToken(microvmIdentifier=<id>, expirationInMinutes=15, allowedPorts=[{allPorts:{}}]) and GET `<endpoint>/probe.txt` with header `X-aws-proxy-auth: <token>`.
   Expected: HTTP 200 with body `sim2real-microvm-dataplane-ok`.
   Observed: HTTP 502 (no guest to hand the request to).
6. Repeat the GET with no header.
   Expected: HTTP 403 from the proxy, body naming the missing authentication (`Request missing authentication`).
   Observed today: masked by step 5 — the guest is absent, so this arm is never exercised.
