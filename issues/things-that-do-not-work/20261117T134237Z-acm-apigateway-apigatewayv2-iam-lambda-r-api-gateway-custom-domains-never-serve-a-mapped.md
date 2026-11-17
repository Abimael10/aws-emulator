Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__apigateway__apigatewayv2__iam__lambda__route53.rest-and-http-api-custom-domains-route-mapped-paths-to-iam-authorized-routes
Issue-Key: acm__apigateway__apigatewayv2__iam__lambda__route53.rest-and-http-api-custom-domains-route-mapped-paths-to-iam-authorized-routes|_poll_https#43|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# API Gateway custom domains never serve a mapped path: a signed GET on the custom domain never answers inside the run budget

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
