Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr.registry-v2-root-on-the-published-proxy-endpoint-answers-the-basic-token
Issue-Shape-Id: com.amazonaws.ecr#AmazonEC2ContainerRegistry_V20150921
Issue-Key: ecr.registry-v2-root-on-the-published-proxy-endpoint-answers-the-basic-token|run#3|AssertionError|assert not (p := ctx.published_url_mismatch(auth["proxyEndpoint"])), p
Issue-Commit: 719a7d873c2ead99db4a3b531125e6f7ff90e604

# ecr: GetAuthorizationToken publishes a proxyEndpoint on a host the emulator does not serve (<account>.dkr.ecr.<region>.localhost), so no docker login / registry v2 call can reach the registry

> _Note: this issue was identified against the code base at commit `719a7d873c2ead99db4a3b531125e6f7ff90e604`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAuthorizationToken

## Description
Cause 1 of 1: the registry address GetAuthorizationToken announces is not an address the emulator answers on, so the registry v2 data plane is unreachable through the API's own published endpoint.

AWS: GetAuthorizationToken returns authorizationData[].proxyEndpoint, documented in the Smithy model (ECR AuthorizationData$proxyEndpoint) as "The registry URL to use for this authorization token in a docker login command" — i.e. the address that actually serves the Docker Registry v2 API for that account and region. A client is expected to take the proxyEndpoint verbatim, present authorizationToken as HTTP Basic credentials, and have GET <proxyEndpoint>/v2/ answer 200 with header Docker-Distribution-API-Version: registry/2.0 (AWS registry-auth documentation; the response status/header are recorded behavior of the registry root). In an emulator the account id, region, host and port in that URL are per-environment values; the requirement is only that the announced host:port is one the emulator itself serves and that the v2 root behind it answers 200 with that header.

Emulator: GetAuthorizationToken publishes a proxyEndpoint whose host is a synthesized AWS-shaped registry name that the emulator does not serve — in this replay 'https://000000000000.dkr.ecr.us-west-2.localhost:36619'. The fixed template is '<scheme>://<accountId>.dkr.ecr.<region>.localhost:<port>'; the account id, region and port are per-run values from the emulator's own state/config, the '<...>.dkr.ecr.<...>.localhost' host shape is the defect. The replay refuses the announced address before any HTTP call: "the emulator published https://000000000000.dkr.ecr.us-west-2.localhost:36619: its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:36619/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". The GET /v2/ that should answer (200, 'registry/2.0') is therefore never made.

Why: every client that does what AWS documents — feed proxyEndpoint to 'docker login', or derive the pull/push base URL from it — is sent to a hostname that does not resolve to (or is not served by) the emulator. Authentication appears to succeed (a token is returned) and then every pull, push, manifest and blob request fails at connection time, so container-image workflows built on the emulator cannot use the registry at all even though the control plane answers.

Related but not covering this: open issue 20261109T141609Z-cloudwatch-ecr-the-ecr-registry-data-plane-address.md reports the same class of defect for Repository$repositoryUri (an '<account>.dkr.ecr.<region>.amazonaws.com' host from DescribeRepositories). This failure is a different operation and a different published field (GetAuthorizationToken's authorizationData[].proxyEndpoint, here with a '.localhost' suffix), and remains broken independently of that one; both should end up announcing the emulator's own registry listener.

Fix: the GetAuthorizationToken handler / the ECR registry-endpoint serializer that composes the announced registry URL — it must emit the emulator's own registry listener host and port (the same address the caller reached the emulator on, or the per-resource registry listener) instead of composing an AWS-shaped '<accountId>.dkr.ecr.<region>...' hostname, and the v2 root behind that address must answer 200 with Docker-Distribution-API-Version: registry/2.0 for a request carrying the returned token as Basic auth.

Traces: ecr.registry-v2-root-on-the-published-proxy-endpoint-answers-the-basic-token.

## Evidence
Failing check: ecr.registry-v2-root-on-the-published-proxy-endpoint-answers-the-basic-token

Assertion raised by the check at check_ecr_registry_v2_root_on_the_published_proxy_endpoint_answers_the_basic_token.py line 26 (`assert not (p := ctx.published_url_mismatch(auth["proxyEndpoint"])), p`):

  AssertionError: the emulator published https://000000000000.dkr.ecr.us-west-2.localhost:36619: its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:36619/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener

Expected: GetAuthorizationToken returns authorizationData[0].proxyEndpoint whose host:port is one the emulator serves, and GET <proxyEndpoint>/v2/ with 'Authorization: Basic <authorizationToken>' answers status 200 with header Docker-Distribution-API-Version: registry/2.0.

Actual: proxyEndpoint is 'https://<accountId>.dkr.ecr.<region>.localhost:<port>' — a host the emulator does not serve; the GET /v2/ probe is never reached, so neither the 200 nor the Docker-Distribution-API-Version header is produced.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Re-replay ecr.registry-v2-root-on-the-published-proxy-endpoint-answers-the-basic-token.
3. Or by hand: call ECR GetAuthorizationToken and read authorizationData[0].proxyEndpoint — the host is an '<accountId>.dkr.ecr.<region>.localhost' name rather than the emulator's own listener host. Then GET <proxyEndpoint>/v2/ with 'Authorization: Basic <authorizationToken>' and observe that nothing answers; against AWS this returns 200 with Docker-Distribution-API-Version: registry/2.0.
