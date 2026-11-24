Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: mwaa.cli-token-native-airflow-cli-round-trip
Issue-Shape-Id: com.amazonaws.mwaa#AmazonMWAA
Issue-Key: mwaa.cli-token-native-airflow-cli-round-trip|run#14|URLError|with urllib.request.urlopen(req, timeout=30) as resp:
Issue-Commit: 2de9869ac15499f72a3a9a80cfa18082252ec30f

# CreateCliToken publishes a WebServerHostname without the port the emulator is served on, so the Airflow CLI endpoint refuses the connection

> _Note: this issue was identified against the code base at commit `2de9869ac15499f72a3a9a80cfa18082252ec30f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCliToken

## Description
REOPENS 20261109T113428Z-mwaa-createclitoken-publishes-a-webserverhostname-on-aws-s-real.md: its fix left the call refused.

One net-new root cause in the Amazon MWAA emulator, grounded in the failing behavioural check mwaa.cli-token-native-airflow-cli-round-trip. The check is NOT marked OWNED or ALREADY FILED by the suite: its replay key changed, because the failure is no longer the one the earlier issues describe. Two open/fixed issues are named here and neither covers it: (a) the fixed issue 20261109T113428Z-mwaa-createclitoken-publishes-a-webserverhostname-on-aws-s-real.md reported that WebServerHostname named a real-AWS host (<uuid>.c2.<region>.airflow.amazonaws.com) the harness refused before any request left the client; after that fix the published host IS one of the emulator's own hosts and the harness now composes a URL and dials it — but the dial is refused at the TCP level, so the operation CreateCliToken exists for is still unusable; (b) the open issue 20261106T045055Z-mwaa-amazon-mwaa-six-behavioural-divergences-native-aws-mwaa.md lists this same check among its trace ids, and its Cause 1 is 'a POST to the published host reaches no handler and answers 404 UnknownService'. That cause is strictly LATER than this one: here no HTTP exchange happens at all, so serving /aws_mwaa/cli cannot clear this check while the published address is not dialable.

=== Cause 1 of 1: the address CreateCliToken publishes is not an address the emulator is reached on — the host is published without the port the emulator listens on, so dialing it is refused ===
AWS:      Smithy (com.amazonaws.mwaa#AmazonMWAA): CreateCliToken is POST /clitoken/{Name}, @endpoint hostPrefix "env.", input Name (EnvironmentName, @httpLabel, required, @length 1..80, @pattern ^[a-zA-Z][0-9a-zA-Z-_]*$), output CliToken (Token, @sensitive) + WebServerHostname (Hostname, @length 1..255, @pattern ^(([a-zA-Z0-9]|[a-zA-Z0-9][a-zA-Z0-9\-]*[a-zA-Z0-9])\.)*([A-Za-z0-9]|[A-Za-z0-9][A-Za-z0-9\-]*[A-Za-z0-9])$); error ResourceNotFoundException (@httpError 404). The model constrains the SHAPE of the value, not which host it is; for the host's meaning this is recorded behavior only: WebServerHostname is the address of the Apache Airflow web server that serves THIS environment, and the documented use of the pair is to POST https://<WebServerHostname>/aws_mwaa/cli with `Authorization: Bearer <CliToken>` and `Content-Type: text/plain` carrying an Airflow CLI command, which answers HTTP 200 with a JSON document whose `stdout` (and `stderr`, when non-empty) are base64-encoded CLI output (a 307 to the same path, preserving method and body, may come first). The requirement is relational: the address published must be the address the responder is actually reached at, on the default port for the scheme a client composes, or else carried in a member that can hold the port.
Emulator:  CreateCliToken answers 200 and reports the HOST of the environment's stored webserver_url with the port stripped — crate::state::webserver_hostname reads the authority of the stored URL and then drops everything but the host, because the Hostname shape's @pattern admits no colon. The emulator serves this service on a loopback address and an ephemeral port, so the published value is a bare host (e.g. a loopback literal) whose implied port for https is 443, which nothing in the emulator listens on. Dialing the published address therefore never reaches the emulator: the check's POST fails with `urllib.error.URLError: <urlopen error [Errno 111] Connection refused>` raised out of http.client's connect, before any request is written. Environment$WebserverUrl keeps the port (published_webserver_url publishes scheme://authority), so the two members disagree about the address and only the one no SDK uses for the CLI flow is dialable.
Why:      a client that does exactly what CreateCliToken exists for — mint a token, then drive the Apache Airflow CLI against the published web-server host, the documented MWAA CLI pattern used by CI scripts and the mwaa wrappers — cannot connect. The 200 is a false success: a token is reported as issued alongside an address nothing answers at, so no DAG can be listed, triggered or backfilled against the emulator, and a client cannot discover the problem from the response. Dropping the port silently is worse than reporting no host at all, which is what the same module already does for an address the shape cannot spell (an IPv6 literal).
Fix:      the published data-plane address and the one place it is built — crate::state::webserver_address (webserver_hostname / published_webserver_url / webserver_url) as consumed by the CreateCliToken handler and the CreateWebLoginToken handler, together with the environment data-plane front door (src/service/env_endpoint.rs) that must answer at it. Either serve the environment's Apache Airflow surface on a per-resource listener whose host alone (default scheme port) is reachable, or report a host that is dialable as published and keep Environment$WebserverUrl consistent with it; a host the Hostname shape can carry but the emulator does not answer on must not be published.
Traces:   mwaa.cli-token-native-airflow-cli-round-trip.

## Evidence
Replayed behavioural suite: 14/21 passed; mwaa 13/19 passed, 6 failed. Five of the six mwaa failures are marked OWNED by 20261106T045055Z-mwaa-amazon-mwaa-six-behavioural-divergences-native-aws-mwaa.md and the single cloudformation__mwaa failure is OWNED by 20261112T024755Z-cloudformation-mwaa-a-template-property-that-violates-the.md; all of those are skipped. This one is unowned and is filed here.

mwaa.cli-token-native-airflow-cli-round-trip  [error]
  Check flow: provision S3 bucket + MWAA execution role + VPC/subnets/SG, CreateEnvironment, poll GetEnvironment to AVAILABLE, CreateCliToken(Name=env) (CliToken and a non-empty WebServerHostname are returned and those assertions pass), then
    url = f"{ctx.endpoint_url(hostname)}/aws_mwaa/cli"   # hostname = CreateCliToken's WebServerHostname
    POST url, headers Authorization: Bearer <CliToken>, Content-Type: text/plain, body b"dags list"
  Expected: HTTP 200 and a JSON body whose base64 `stdout` contains the Airflow dag table (a `dag_id` header, or "No data found").
  Actual:   no HTTP exchange at all — the connect to the published address is refused:
              File "request.py", line 1405, in https_open
                return self.do_open(http.client.HTTPSConnection, req,
              File "request.py", line 1360, in do_open
                raise URLError(err)
              urllib.error.URLError: <urlopen error [Errno 111] Connection refused>
  Source:  services/service-com-amazonaws-mwaa-amazonmwaa/src/state/webserver_address.rs:79-86 (webserver_hostname strips the port off the stored authority via data_plane_addressing::host_only) and :60-66 (published_webserver_url keeps scheme://authority, port included), consumed at services/service-com-amazonaws-mwaa-amazonmwaa/src/ops/create_cli_token.rs:210. The crate's own unit test the_published_host_is_the_url_host_the_shape_can_carry asserts the port is dropped ("https://127.0.0.1:9297" -> Some("127.0.0.1")), so the published host is dialable only if the emulator happens to serve the scheme's default port.
  The specific loopback address and the ephemeral port of a given run are per-run values and are NOT the requirement; the requirement is that the address published in WebServerHostname be dialable as published.
  Note: the previous replay of this same check (fixed issue 20261109T113428Z) failed earlier still, with the harness refusing the published URL because its host was <uuid>.c2.<region>.airflow.amazonaws.com. The host is now the emulator's own, but the call is still refused.

## Steps to Reproduce
1. Boot the emulator (note the host and port it is served on). Provision an S3 bucket with a `dags/` key, an MWAA execution role, and a VPC with two subnets and a security group.
2. CreateEnvironment with that role ARN, bucket ARN, DagS3Path="dags" and the network configuration; poll GetEnvironment until Status=AVAILABLE.
3. Call CreateCliToken(Name=<that environment>). Observe HTTP 200 with a CliToken and a WebServerHostname.
4. Compare WebServerHostname with GetEnvironment's Environment.WebserverUrl: the URL carries the port the emulator is served on, the host member does not.
5. Compose https://<WebServerHostname>/aws_mwaa/cli and POST body `dags list` with headers `Authorization: Bearer <CliToken>` and `Content-Type: text/plain` (following a 307 to the same path, preserving method and body). Expected: HTTP 200 with a JSON document carrying base64 `stdout` containing the Airflow dag table. Observed: the connection is refused (Errno 111) — nothing listens on the scheme's default port at the published host, so the request is never issued.
