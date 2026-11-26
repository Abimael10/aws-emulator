Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2.disable-portal-revokes-access-server-side apigatewayv2.e2e-portal-product-publish-flow
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2.disable-portal-revokes-access-server-side|run#7|PublishedUrlRejected|url = f"{ctx.endpoint_url(domain)}/"
Issue-Key: apigatewayv2.e2e-portal-product-publish-flow|_fetch_status#15|PublishedUrlRejected|req = urllib.request.Request(f"{ctx.endpoint_url(host)}{path}", method="GET")
Issue-Commit: c4b32de3852571157d57fb7c85d3ff1323d48297

# apigatewayv2 portal data plane: the PortalDefaultDomainName announced for a portal is not a host the emulator serves, so a PUBLISHED portal site is unreachable

> _Note: this issue was identified against the code base at commit `c4b32de3852571157d57fb7c85d3ff1323d48297`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePortal

## Description
One net-new root cause, grounded in two failing behavioral checks of the apigatewayv2 portal lifecycle.

Cause 1 of 1: the portal default domain name the emulator publishes through endpointConfiguration.portalDefaultDomainName (CreatePortal 201 and GetPortal 200) is not a host any emulator listener claims, so no request addressed to it reaches the portal site and the PUBLISHED -> DISABLED transition cannot be observed on the data plane at all. Failing checks: apigatewayv2.disable-portal-revokes-access-server-side, apigatewayv2.e2e-portal-product-publish-flow.

AWS: Smithy (com.amazonaws.apigatewayv2#ApiGatewayV2) CreatePortal — POST /v2/portals, success code 201; GetPortal — GET /v2/portals/{PortalId}, code 200. Both return EndpointConfiguration (jsonName endpointConfiguration, target EndpointConfigurationResponse), whose member PortalDefaultDomainName (jsonName portalDefaultDomainName, @required, @length min 3 max 256) is documented: "The portal default domain name. This domain name is generated and managed by API Gateway." API Gateway both generates that host from the portal id and the Region and MANAGES it: the portal's website is actually served on it. PublishPortal (POST /v2/portals/{PortalId}/publish, 202) brings the site online — after PublishStatus reaches PUBLISHED (enum PublishStatus: PUBLISHED, PUBLISH_IN_PROGRESS, PUBLISH_FAILED, DISABLE_IN_PROGRESS, DISABLE_FAILED, DISABLED) a GET / on that domain answers HTTP 200. DisablePortal (DELETE /v2/portals/{PortalId}/publish, 204) takes the website offline, not just the status field: once PublishStatus reads DISABLED, a GET / on the same domain must stop answering 200, and must keep not answering 200 on subsequent requests. The portal id, the Region label and the host suffix in any one run are per-run values and are not the requirement; that the announced host is one the service answers the portal on IS.

Emulator: the create/get responses carry an AWS-shaped but unclaimed host. Replaying either check, the harness takes endpointConfiguration.portalDefaultDomainName straight out of the portal representation and is refused before any HTTP request is made: "simparity.published.PublishedUrlRejected: endpoint_url was given https://p-<portalId>.apigw-portal.<region>.localhost (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". Nothing registers the generated default domain with the portal data plane's host matcher, so the single address the create/get response hands out is served by no listener; the portal content is only reachable behind the emulator's own endpoint on an internal path.

Why: a client reads this member to build https://<portalDefaultDomainName>/ for the portal's consumers (and to point a CNAME / Route 53 alias at the portal). Against AWS that address answers 200 while the portal is PUBLISHED and stops answering 200 after DisablePortal. Against the emulator a portal that was created and published successfully is unreachable by the only address its own response advertises, so neither the publish-makes-it-serve behaviour nor the disable-revokes-access-server-side behaviour can be exercised; a consumer cannot distinguish a published portal from a disabled one from the data plane, and the whole portal data plane is effectively absent.

Fix: the portal host-claim registration that must pair with the portal representation — the CreatePortal handler and the portal state that mints the default domain (services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/ops/create_portal.rs and the portal-domain/site-base helpers in services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/state.rs) must register the generated domain with the portal data plane's host matcher/listener registry so the announced host is one the emulator owns, and the portal data plane must serve that host with the portal's content only while PublishStatus is PUBLISHED and stop serving it (non-200) once DisablePortal has driven the status to DISABLED.

Related open issue that does NOT carry these replay keys: 20261119T122508Z-apigateway-createportal-announces-a-portal-default-domain-the.md reports the same unclaimed-host cause from the WIRE corpus (Issue-Key apigateway|CreatePortal|2xx->2xx|$.endpointConfiguration.portalDefaultDomainName), where the minted suffix was '.on.aws'; these two behavioral checks were not marked owned by it, and they additionally bind the publish/disable data-plane semantics above, so they are filed here. The suffix now observed is '.localhost'.

Traces: apigatewayv2.disable-portal-revokes-access-server-side (the publish 200 / disable non-200 probe), apigatewayv2.e2e-portal-product-publish-flow (portal product + product page attached, then publish/disable).

## Evidence
Behavioral suite replay against the emulator built from this checkout (211/327 passed; bootstrap_signal false, emulator serving).

Both failures abort identically, before any request is sent to the announced host:

  #### [mismatch] apigatewayv2.disable-portal-revokes-access-server-side
  Error: File "context.py", line 708, in endpoint_route
    return self._published_target(url, "endpoint_url"), {}
  File "context.py", line 724, in _published_target
    raise PublishedUrlRejected(...)
  simparity.published.PublishedUrlRejected: endpoint_url was given https://p-z1hvlts8lq.apigw-portal.us-west-2.localhost (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:34615/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener

  #### [mismatch] apigatewayv2.e2e-portal-product-publish-flow
  Error: same PublishedUrlRejected on endpoint_url https://p-z1hvlts8lq.apigw-portal.us-west-2.localhost (emulator port 37755 in that run)

The value that is rejected is read verbatim out of the emulator's own response: portal["EndpointConfiguration"]["PortalDefaultDomainName"] from CreatePortal/GetPortal.

Expected (AWS): endpointConfiguration.portalDefaultDomainName names a host the service serves the portal on; GET https://<that host>/ answers HTTP 200 while GetPortal reports PublishStatus PUBLISHED, and answers non-200 (and keeps doing so on three consecutive follow-up probes) once DisablePortal has driven PublishStatus to DISABLED.
Actual (emulator): the member names a host no listener claims, so the probe cannot be issued at all; the portal site is served only behind the emulator's own endpoint, and no publish/disable effect is observable on the announced domain.

Per-run values here — the portal id 'z1hvlts8lq', the Region label 'us-west-2', the host suffix, and the emulator ports 34615/37755 — are not part of the requirement; the requirement is that the announced domain be a host the emulator owns and serves the portal on, gated on PublishStatus.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the behavioral suite (simparity-behavioral-aws-v1).
2. Replay apigatewayv2.e2e-portal-product-publish-flow: CreatePortalProduct, CreateProductPage, then CreatePortal with Authorization={'None':{}}, EndpointConfiguration={'None':{}}, PortalContent={DisplayName, Theme.CustomColors...} and IncludedPortalProductArns=[the product ARN].
3. Read EndpointConfiguration.PortalDefaultDomainName from the 201 body (and from GetPortal) — it is 'p-<portalId>.apigw-portal.<region>.localhost'.
4. Call PublishPortal(PortalId=...) and poll GetPortal until PublishStatus == PUBLISHED.
5. Issue GET / against the announced domain. No emulator listener claims that host, so the request cannot be addressed to the emulator at all (the harness refuses it as host_not_own). Expected: HTTP 200 serving the portal site.
6. Call DisablePortal(PortalId=...) and poll GetPortal until PublishStatus == DISABLED, then GET / against the same domain three more times. Expected: every probe non-200 and the revocation holds. Actual: unobservable, because the announced domain is not an address the emulator is reached on.
7. Replay apigatewayv2.disable-portal-revokes-access-server-side for the same failure without the portal-product setup.
