Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2.e2e-portal-product-publish-flow apigatewayv2.disable-portal-revokes-access-server-side
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2.e2e-portal-product-publish-flow|_fetch_status#15|PublishedUrlRejected|req = urllib.request.Request(f"{ctx.endpoint_url(host)}{path}", method="GET")
Issue-Key: apigatewayv2.disable-portal-revokes-access-server-side|run#7|PublishedUrlRejected|url = f"{ctx.endpoint_url(domain)}/"
Issue-Commit: 7cfebfe55ff6426f4bd53063bb67677021372215

# apigatewayv2: the portal's announced PortalDefaultDomainName is an AWS-shaped host the emulator never serves, so a published portal's site is unreachable (1 cause)

> _Note: this issue was identified against the code base at commit `7cfebfe55ff6426f4bd53063bb67677021372215`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePortal

## Description
Cause 1 of 1: the portal website address this emulator publishes in EndpointConfigurationResponse$PortalDefaultDomainName is a generated real-AWS-shaped DNS host that the running emulator does not claim and is not reachable at, so no client of the emulator can ever fetch a published portal's site, and the publish -> serve -> DisablePortal -> stop-serving flow stays unobservable. Failing checks: apigatewayv2.e2e-portal-product-publish-flow, apigatewayv2.disable-portal-revokes-access-server-side.

AWS: Smithy (services/service-com-amazonaws-apigatewayv2-apigatewayv2/smithy-model/com-amazonaws-apigatewayv2-apigatewayv2.json) models CreatePortal POST /v2/portals -> 201 with CreatePortalResponse$EndpointConfiguration (EndpointConfigurationResponse) and GetPortal GET /v2/portals/{PortalId} -> 200 with the same member; EndpointConfigurationResponse$PortalDefaultDomainName is the host "generated and managed by API Gateway" that the portal's site is served on. PublishPortal (POST /v2/portals/{PortalId}/publish, 202, empty body) puts the built site on that host; while GetPortal$PublishStatus reads PUBLISHED an unauthenticated GET of that host's root answers HTTP 200 with the portal content. DisablePortal (DELETE /v2/portals/{PortalId}/publish, 204, empty body) takes the publication down, so the same URL must answer a non-200 and keep doing so on every later probe (Smithy PublishStatus = PUBLISHED | PUBLISH_IN_PROGRESS | PUBLISH_FAILED | DISABLE_IN_PROGRESS | DISABLE_FAILED | DISABLED). The requirement is that the host the service itself reports is the host the portal site is actually served on by the endpoint the caller is talking to; the concrete label, region and port are environment values, not part of the contract.

Emulator: CreatePortal/GetPortal announce a PortalDefaultDomainName of the generated form <portal-id>.apigw-portal.<region>.on.aws (replay value: p-z1hvlts8lq.apigw-portal.us-west-2.on.aws) even though the emulator is reached on, and only serves, its own local endpoint host. Both checks therefore abort before any probe can be made: 'simparity.published.PublishedUrlRejected: endpoint_url was given https://p-z1hvlts8lq.apigw-portal.us-west-2.on.aws (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener'. The fixed issue 20261027T133045Z-apigatewayv2-the-publish-serve-disableportal-stop-serving-flow.md covered this same flow for check apigatewayv2.disable-portal-revokes-access-server-side and explicitly dismissed addressing as the cause (its status note says state::announced_portal_default_domain_name "already announces the emulator's own endpoint for a create addressed there and the generated AWS host otherwise"); the replay shows that conditional announcement is exactly what now blocks both checks: the address announced is not an address the emulator is reached on, so the portal data plane is unreachable regardless of how fast the publish/disable builds settle. The sibling v1 service carries the same defect under its own open issue 20261119T122508Z-apigateway-createportal-announces-a-portal-default-domain-the.md (service apigateway, not apigatewayv2 — a different work item).

Why: a client that does what the API tells it to do — read PortalDefaultDomainName from CreatePortal/GetPortal and GET it — gets a name-resolution failure instead of the portal site, so the whole point of publishing a portal (its site being served on the domain the service reported) cannot be exercised. The security-relevant half is worse: because the domain is never reachable, a caller cannot verify that DisablePortal actually revoked public access rather than only flipping PublishStatus, and any release gate asserting "the portal stopped serving after DisablePortal" silently can never run.

Fix: belongs in the portal endpoint addressing that CreatePortal/GetPortal/ListPortals serialize (the announcement helper in this service's state, state::announced_portal_default_domain_name, consumed by src/ops/create_portal.rs and src/ops/get_portal.rs) together with the portal data plane's host claiming (src/service/portal.rs claims/resolve). The announced default domain must unconditionally be an address the emulator itself answers on — its own endpoint host/port, or a per-resource listener it claims — rather than a generated real-AWS DNS name chosen on the basis of how the CreatePortal call happened to be addressed.

Traces: apigatewayv2.e2e-portal-product-publish-flow (publish a portal with an attached PortalProduct/ProductPage, then GET PortalDefaultDomainName expecting 200), apigatewayv2.disable-portal-revokes-access-server-side (publish, probe 200, DisablePortal, probe non-200 three times).

## Evidence
Expected (AWS): CreatePortal -> 201 with endpointConfiguration.portalDefaultDomainName set to the host the portal site is served on by this endpoint; PublishPortal -> 202; GetPortal$PublishStatus settles PUBLISHED; unauthenticated GET https://{PortalDefaultDomainName}/ -> HTTP 200 (portal site); DisablePortal -> 204; GetPortal$PublishStatus settles DISABLED; the same GET -> non-200 and stays non-200 on every follow-up probe.

Actual (emulator replay):
- apigatewayv2.e2e-portal-product-publish-flow, verdict 'mismatch': 'simparity.published.PublishedUrlRejected: endpoint_url was given https://p-z1hvlts8lq.apigw-portal.us-west-2.on.aws (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:43535/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener'.
- apigatewayv2.disable-portal-revokes-access-server-side, verdict 'mismatch': the identical PublishedUrlRejected on the same announced host (port 35593 in that run).
The host label (p-z1hvlts8lq), region and the loopback port differ per run and are not requirements; the requirement is that the announced host is one the emulator answers on. No HTTP status or AWS error code is produced for the diverging step because the portal domain is never contacted — the announced address cannot be reached at all.

## Steps to Reproduce
1. apigatewayv2 CreatePortal with Authorization={"None":{}}, EndpointConfiguration={"None":{}}, PortalContent={DisplayName, Theme.CustomColors{...}} (optionally IncludedPortalProductArns from a CreatePortalProduct + CreateProductPage) -> note PortalId and endpointConfiguration.portalDefaultDomainName.
2. Observe the announced portalDefaultDomainName: it is <portal-id>.apigw-portal.<region>.on.aws, a host the emulator does not claim and is not reachable at (the caller is talking to 127.0.0.1:<port>).
3. PublishPortal(PortalId), poll GetPortal(PortalId).PublishStatus until PUBLISHED.
4. GET https://{portalDefaultDomainName}/ with no credential -> expect HTTP 200; against this emulator the address does not resolve/belong to the emulator, so the probe cannot be made at all.
5. DisablePortal(PortalId), poll until PublishStatus is DISABLED, re-probe the same URL expecting non-200 -> also unobservable for the same reason.
