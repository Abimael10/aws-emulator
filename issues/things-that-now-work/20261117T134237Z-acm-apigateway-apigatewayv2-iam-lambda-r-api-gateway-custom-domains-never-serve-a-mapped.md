Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__apigateway__apigatewayv2__iam__lambda__route53.rest-and-http-api-custom-domains-route-mapped-paths-to-iam-authorized-routes
Issue-Key: acm__apigateway__apigatewayv2__iam__lambda__route53.rest-and-http-api-custom-domains-route-mapped-paths-to-iam-authorized-routes|_poll_https#43|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# API Gateway custom domains never serve a mapped path: a signed GET on the custom domain never answers inside the run budget

## Resolution

A custom domain name's mapped path is now served at EVERY address this emulator answers at, for
every endpoint type a public alias is published for:

* the domain name itself, and the address the record published for a caller's DNS record to
  target — `regionalDomainName` (the `host:port` of the socket the create opened) and
  `distributionDomainName` (the distribution name the record was assigned) — read as the `Host`
  of a request made at either of them;
* either of those CARRIED in the emulator's reserved data-plane path namespace
  (`/_aws/{host}/{path}`), which is how a client on the machine this emulator runs on addresses
  a host it resolves nowhere — the rewriting every other data plane of this tree already served
  and a custom domain name did not;
* an EDGE-OPTIMIZED record as well as a regional one. An edge-optimized record — the type a
  create naming no `endpointConfiguration` carries, which is API Gateway's default — was
  resolved by NO addressing at all: its base path mappings were never consulted, so every
  request to it reached no api of the account.

Pinned by `core/router/tests/all/apigateway_custom_domain_mapped_path.rs` (Certificate Manager,
Route 53 and API Gateway on one engine: an issued DNS-validated certificate, a regional and an
edge-optimized domain name over one deployed `AWS_IAM` method, every spelling of every host, with
the unsigned twin refused) and, for an HTTP api's custom domain, by
`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/custom_domain_carried_host_invoke.rs`.

What REMAINS for this check, and it is not these service crates' to serve: its client reaches the
custom domain at an address this process cannot answer on. A client that resolves the published
record and dials it uses the port `https` implies (443), while `Listeners::tcp_plaintext`
(`vendor/server/src/listeners.rs`, through `bind_tokio`/`bind_std`) takes no requested port — it
binds an OS-assigned port inside `EMULATOR_LISTENER_PORT_RANGE` — and a client that verifies the
name it dialled cannot be served either, because the one process certificate
(`vendor/server/src/tls_certificate_authority.rs`, `covering` is crate-private and its names are
fixed on first use) covers loopback and the bound interface alone, with no per-connection
certificate resolver. This is the same core gap recorded for this surface in
`issues/things-that-now-work/20261114T121226Z-acm-apigateway-a-regional-custom-domain-s-data.md`.


> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomainName

## Description
Cause 1 of 1: a REST/HTTP API custom domain name with a base-path mapping never becomes servable, so the data-plane leg of the check never gets an answer.

AWS: recorded behavior only for the data plane (execute-api/custom-domain traffic is not in the Smithy model). The control-plane shape is modelled: apigateway CreateDomainName/CreateBasePathMapping and apigatewayv2 CreateDomainName/CreateApiMapping publish a `distributionDomainName`/`regionalDomainName` (REST) or `DomainNameConfigurations[].ApiGatewayDomainName` (HTTP) that resolves and terminates TLS with the ACM certificate named by `certificateArn`/`CertificateArn`, and a GET of a path covered by a base-path mapping is routed to the mapped stage/API and answered by the integration. A SigV4-signed GET on an IAM-authorized route must answer 200 with the integration's body; an unsigned one must answer 403 with the `{"message":"Forbidden"}` body API Gateway words for a missing-credentials refusal.

Emulator: the signed `GET /v1/items` against the custom domain never produces any HTTP answer at all. The check's bounded poll exhausts the whole run budget and raises `TimeoutError: a signed GET /v1/items on the custom domain being served: the run budget ended first; not graded`, so the custom-domain front door is not reachable within the budget. The control-plane calls that precede it succeeded, so the divergence is in serving the published domain, not in creating it.

Why: a client that deploys an API behind a custom domain — the normal production shape for every REST and HTTP API — can never reach it. Every assertion that depends on custom-domain routing (base-path mapping selection, the certificate presented, IAM authorization on the mapped route) is unobservable, and a caller sees a hang rather than a refusal it could handle.

Fix: the custom-domain front door of the API Gateway data plane — the listener/vhost that the domain name published by CreateDomainName resolves to, and the base-path-mapping lookup that dispatches a request on that host to the mapped stage/API. It must accept a connection and answer (with the integration's response, or with API Gateway's own 403/404) rather than never answering.

Traces: acm__apigateway__apigatewayv2__iam__lambda__route53.rest-and-http-api-custom-domains-route-mapped-paths-to-iam-authorized-routes

## Evidence
Expected: a SigV4-signed `GET /v1/items` on the custom domain answers 200 with the mapped integration's body (and the unsigned twin answers 403 `{"message":"Forbidden"}`).
Actual: no HTTP answer is ever produced. The check's bounded poll exhausts the run budget and raises, verbatim:
  TimeoutError: a signed GET /v1/items on the custom domain being served: the run budget ended first; not graded
raised from `_poll_https(...)`. All preceding control-plane calls (acm, apigateway CreateDomainName/CreateBasePathMapping, apigatewayv2 CreateDomainName/CreateApiMapping, route53) succeeded, so the gap is in serving the published domain.

## Steps to Reproduce
1. Request an ACM certificate for a custom name and have it issued.
2. apigateway CreateDomainName (regional, that certificate) + CreateBasePathMapping onto a deployed REST stage; apigatewayv2 CreateDomainName + CreateApiMapping onto an HTTP API stage.
3. Publish the returned regional/ApiGatewayDomainName in Route 53 and send a SigV4-signed `GET /v1/items` to `https://<custom-domain>/v1/items`.
4. Observe: no HTTP answer is ever returned — the poll runs out the budget. Expected: 200 from the mapped integration for the signed GET, and API Gateway's own 403 `{"message":"Forbidden"}` for the unsigned twin.
