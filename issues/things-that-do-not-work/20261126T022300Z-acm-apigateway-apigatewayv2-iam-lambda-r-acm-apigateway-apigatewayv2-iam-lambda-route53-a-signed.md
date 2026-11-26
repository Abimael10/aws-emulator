Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__apigateway__apigatewayv2__iam__lambda__route53.rest-and-http-api-custom-domains-route-mapped-paths-to-iam-authorized-routes
Issue-Key: acm__apigateway__apigatewayv2__iam__lambda__route53.rest-and-http-api-custom-domains-route-mapped-paths-to-iam-authorized-routes|_poll_https#43|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 3e9f9573c106578402cdbb599b1d33abce0d226b

# acm__apigateway__apigatewayv2__iam__lambda__route53: a signed GET through a REGIONAL REST custom domain's base path mapping is never served, so the mapped-path data plane reaches no response at all

> _Note: this issue was identified against the code base at commit `3e9f9573c106578402cdbb599b1d33abce0d226b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateBasePathMapping

## Description
Cause 1 of 1: a REGIONAL REST API custom domain whose base path mapping is created and read back, and whose domainNameStatus reaches AVAILABLE, never answers a request on the mapped path — the signed GET on the custom name produces neither a 200 from the mapped stage nor any refusal, for the whole run budget.

AWS: a custom domain created by CreateDomainName with a regionalCertificateArn and endpointConfiguration REGIONAL, with a base path mapping (CreateBasePathMapping basePath=v1, restApiId, stage=live), serves https://<domainName>/v1/<resource> by routing it to that stage's method. A SigV4-signed GET on an AWS_IAM-authorized method returns 200 with the integration response body; the same request unsigned returns 403 with the REST envelope {"message": "Missing Authentication Token"}. The TLS handshake on that name presents the ACM certificate whose SANs are the configured names. Smithy (apigateway): CreateDomainName / CreateBasePathMapping / GetDomainName, with domainNameStatus AVAILABLE and regionalDomainName as the host the custom name is aliased to. The data-plane response itself is not modelled — recorded/documented behavior only (REST API custom domain mappings).

Emulator: the control plane accepts all of it — GetBasePathMappings returns exactly [{basePath: v1, restApiId, stage: live}], GetDomainName reports domainNameStatus AVAILABLE and a regionalDomainName — but repeated HTTPS GETs against that announced host with the custom name as SNI/Host never return 200, nor any other graded response; the poll exhausts the run budget. The mapped-path leg produces no status, no body and no certificate, so the subsequent unsigned-403 and certificate-SAN assertions are never reached.

Why: a client that provisions a custom domain exactly as AWS documents gets a control plane that claims the domain is AVAILABLE and the mapping installed, and a data plane that silently never answers. Health checks, deploy gates and integration tests hang instead of failing fast, and nothing distinguishes "not serving" from "slow", because no response — not even an error — is produced. The AWS_IAM authorization decision on the mapped path (200 signed vs 403 unsigned) is unobservable as a result.

Fix: the custom-domain data plane for REST APIs — the front end that accepts a connection for a created domain name and resolves <basePath>/<path> through the base path mapping to the named stage's method, plus the component that makes the announced regionalDomainName an address the emulator actually serves that name on. The control-plane handlers (CreateDomainName, CreateBasePathMapping, GetDomainName) already behave; the gap is that an AVAILABLE domain is not a host the mapped path is served on.

Related open issues describe neighbouring symptoms but do not carry this check's replay key: 20261116T020413Z-acm-apigateway-the-regional-custom-domain-still-reaches.md (certificate presentation on a REGIONAL REST custom domain), 20261124T103252Z-apigatewayv2-a-custom-domain-name-created-by-createdomainname.md and 20261115T205932Z-apigatewayv2-a-custom-domain-s-apigatewaydomainname-is-announced.md (the HTTP API side of the same surface). This check is the REST base-path-mapping leg of the combined REST + HTTP API custom-domain scenario.

Traces: acm__apigateway__apigatewayv2__iam__lambda__route53.rest-and-http-api-custom-domains-route-mapped-paths-to-iam-authorized-routes.

## Evidence
Failing check acm__apigateway__apigatewayv2__iam__lambda__route53.rest-and-http-api-custom-domains-route-mapped-paths-to-iam-authorized-routes (verdict: error).

Expected (AWS): the signed GET https://<rest custom name>/v1/items returns 200 with the MOCK integration's marker body, over a TLS handshake presenting the ACM certificate whose DNS SANs are exactly the two configured custom names; the same request unsigned returns 403 with body {"message": "Missing Authentication Token"}.

Actual (emulator): the poll never observed any graded response on the mapped path and ended the run budget:
  File "check_acm__apigateway__apigatewayv2__iam__lambda__route53_rest_and_http_api_custom_domains_route_mapped_paths_to_iam_authorized_routes.py", line 455, in run
    status, _headers, body, peer = _poll_https(
  File "...", line 305, in _poll_https
    raise TimeoutError(f"{description}: the run budget ended first; not graded")
  TimeoutError: a signed GET /v1/items on the custom domain being served: the run budget ended first; not graded

The control-plane assertions preceding it passed: GetBasePathMappings == [{"basePath": "v1", "restApiId": <id>, "stage": "live"}], GetApiMappings == [{ApiId, ApiMappingId, ApiMappingKey "v2", Stage "live"}], and both GetDomainName polls reached status AVAILABLE and yielded a regionalDomainName / ApiGatewayDomainName to connect to. So the divergence is on the data plane of the announced host, not in provisioning.

## Steps to Reproduce
1. Request and validate an ACM certificate covering two names under a hosted zone (one for the REST domain, one for the HTTP API domain).
2. CreateRestApi (REGIONAL, disableExecuteApiEndpoint=true); create resource /items; PutMethod GET with authorizationType=AWS_IAM; PutIntegration type=MOCK returning a fixed marker; PutMethodResponse/PutIntegrationResponse 200; CreateDeployment stageName=live.
3. CreateDomainName(domainName=<rest name>, regionalCertificateArn=<cert>, endpointConfiguration REGIONAL, securityPolicy TLS_1_2); CreateBasePathMapping(domainName=<rest name>, basePath=v1, restApiId=<id>, stage=live).
4. GetBasePathMappings and GetDomainName — the mapping reads back and domainNameStatus becomes AVAILABLE; take regionalDomainName.
5. Issue a SigV4-signed HTTPS GET https://<rest name>/v1/items against the announced regionalDomainName (custom name as SNI/Host), retrying on connection errors. No 200 — and no response at all — is produced until the run budget is exhausted; the expected result is 200 with the integration marker body, and the unsigned request 403 {"message": "Missing Authentication Token"}.
