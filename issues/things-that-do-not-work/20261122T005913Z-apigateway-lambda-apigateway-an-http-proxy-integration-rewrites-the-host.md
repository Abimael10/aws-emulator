Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__lambda.xsvc-apigateway-3-http-proxy-function-url-round-trips-path-query-headers
Issue-Key: apigateway__lambda.xsvc-apigateway-3-http-proxy-function-url-round-trips-path-query-headers|run#15|AssertionError|assert seen.get("host") == fn["host"], (
Issue-Commit: b19562f8ca9de9711e1cde2d426cb1fc8bc6040d

# apigateway: an HTTP_PROXY integration rewrites the Host header to the integration host but drops its port, so the backend is addressed on the wrong authority

> _Note: this issue was identified against the code base at commit `b19562f8ca9de9711e1cde2d426cb1fc8bc6040d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutIntegration

## Description
Cause 1 of 1: when a REST API method with an HTTP_PROXY integration forwards a request, API Gateway replaces the incoming Host header with the integration endpoint's authority. The emulator writes only the integration URI's host and discards its port, so a backend listening on a non-default port receives a Host header that does not name the authority the connection was made to.

AWS: an HTTP/HTTP_PROXY integration opens a connection to the integration `uri` and sends that endpoint's authority as the Host header — host, plus `:port` whenever the URI names a port other than the scheme default. The client's own Host is not forwarded. (Smithy: apigateway PutIntegration input members `type` (HTTP_PROXY) and `uri`; the Host-rewrite rule itself is documented API Gateway behavior, not modelled — recorded behavior only for the exact header value, which the recording shows as the integration endpoint's host:port.)

Emulator: the backend's echoed request shows `host` = the integration URI's host with no port, while the integration endpoint it was reached on is host:port. Verbatim: "backend Host was '127.0.0.1'; HTTP_PROXY must address the integration host '127.0.0.1:36711'". (The host and port values are per-run; the requirement is that the forwarded Host equal the integration URI's full authority, including the port when the URI carries one.)

Why: a backend that routes on Host — any virtual-hosted service, a signed request whose SigV4 canonical request includes the Host header, or a framework that builds absolute URLs from it — sees a different authority than the one it is served on. Requests get routed to the wrong virtual host, signatures computed over Host fail to verify, and self-referencing redirects/links the backend emits point at the wrong port.

Note: open issue 20261109T152222Z-apigateway-lambda-function-urls-are-published-on-lambda.md owns the sibling checks of this family for a different cause (the function URL was unreachable at all). Here the backend IS reached and answers, and the only divergence left is the forwarded Host header, so that issue does not cover this cause.

Fix: the execute-api HTTP/HTTP_PROXY integration request builder — the step that sets the outbound Host header from the parsed integration `uri`. It must use the URI's authority (host and, when present, port), not the host component alone.

Traces: apigateway__lambda.xsvc-apigateway-3-http-proxy-function-url-round-trips-path-query-headers.

## Evidence
Expected: the backend's received Host header equals the integration endpoint's authority (host:port of the integration `uri`).
Actual, verbatim from the replay:
  File "check_apigateway__lambda_xsvc_apigateway_3_http_proxy_function_url_round_trips_path_query_headers.py", line 53, in run
  assert seen.get("host") == fn["host"], (
  AssertionError: backend Host was '127.0.0.1'; HTTP_PROXY must address the integration host '127.0.0.1:36711'
Fixed part of the requirement: the forwarded Host is the integration URI's authority. Variable parts: the host and the port, taken from the integration `uri` of the request under test (here an emulator-published Lambda function URL endpoint).

## Steps to Reproduce
1. CreateFunction (python3.x) and CreateFunctionUrlConfig with AuthType NONE on it; note the published FunctionUrl, whose authority carries a port.
2. CreateRestApi with a greedy `{proxy+}` resource and an ANY method whose integration is `type=HTTP_PROXY`, `integrationHttpMethod=ANY`, `uri=<function URL>/{proxy}`; CreateDeployment to a stage.
3. GET `<stage base>/items/abc/def?color=blue&size=9&tag=a&tag=b` with extra request headers.
4. Have the backend echo the headers it received and read its `host` value.
5. Observed: the host component only ('127.0.0.1'). Expected: the integration endpoint's full authority ('127.0.0.1:36711').
