Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-portal-with-included-portal-product
Issue-Step: setup#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-portal-with-included-portal-product apigatewayv2-portal-with-included-product
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigateway|CreatePortal|2xx->2xx|$.endpointConfiguration.portalDefaultDomainName
Issue-Commit: 2714aa03aa1d60831b8d663aed1f4386c9b03868

# apigateway CreatePortal announces a portal default domain the emulator does not claim

> _Note: this issue was identified against the code base at commit `2714aa03aa1d60831b8d663aed1f4386c9b03868`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePortal

## Description
CreatePortal publishes a portal default domain name that is AWS-shaped but is not a host this emulator owns or serves, so the one address a portal consumer is given by the create response leads nowhere. One cluster, one root cause, reported with empty stub_path and empty owned_by.

Cause 1 of 1: the generated host in endpointConfiguration.portalDefaultDomainName is not claimed by the running emulator.
AWS: Smithy (apigatewayv2) CreatePortal — http POST /v2/portals, code 201; output member EndpointConfiguration (jsonName 'endpointConfiguration', target EndpointConfigurationResponse) carries PortalDefaultDomainName, documented as "The portal default domain name. This domain name is generated and managed by API Gateway." API Gateway both GENERATES that host from the portal's own id and the Region (host-label form p-{portalId}.apigw-portal.{region}.on.aws — the shape this checkout already encodes in state::portal_default_domain_name) and MANAGES it, i.e. the portal's website is actually served on that host. The recorded 201 asserts the member as the portal's own host, binding the portal id created in the step (recorded expectation 'p-{{bundling_portal_id}}.apigw-portal.us-west-2.on.aws'); the id itself is a per-run value and is not the requirement — that the announced host is one the service serves the portal on IS.
Emulator: answers 201 with every other member matching, and with a correctly shaped host, but the host is not one the emulator claims: the replay verdict on $.endpointConfiguration.portalDefaultDomainName is host_not_own (detail: "host_not_own (expected 'p-{{bundling_portal_id}}.apigw-portal.us-west-2.on.aws', got 'p-<portalId>.apigw-portal.us-west-2.on.aws')"). The emulator is reached on its own data listener and only serves a portal behind that endpoint, at <announced-endpoint>/_aws/portal/<portalId> (crate::state::announced_portal_site_base, which reports that address through the portal's URL-typed members instead). Nothing registers the generated default domain as a host the portal data plane answers for, even though the crate's own documentation for announced_portal_site_base asserts the portal data plane "claims host-style" the generated domain; so the member names an address other than the one the emulator is reached on and no request addressed to it is served.
Why: a client reads this member to build 'https://<portalDefaultDomainName>' for the portal's consumers and to point a CNAME / Route 53 alias record at the portal. Against AWS that address answers; against the emulator the published host answers nothing, so a portal that was created successfully is unreachable by the only address its create response hands out, and a publish → browse-the-portal flow cannot be exercised against the emulator at the documented address.
Fix: the portal host-claim registration that pairs with the portal representation — CreatePortal's handler (services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/ops/create_portal.rs, which builds the member through state::resolve_portal_endpoint_configuration → state::portal_default_domain_name and stores a separate site_base) must register the generated default domain with the portal data plane's host matcher (crate::state::listener_registry) so the announced domain is a host the emulator owns, rather than announcing a domain only an AWS resolver could reach while serving the portal exclusively behind the emulator's own endpoint.
Traces: apigatewayv2-portal-with-included-portal-product#setup#2, apigatewayv2-portal-with-included-product#setup#2.

Related issues that do NOT carry this replay key: open 20261113T182955Z-apigateway-listportals-summaries-publish-a-host-port-portal.md covers ListPortals still serializing the authority-plus-path form (different Issue-Key/operation), and resolved 20261103T032504Z-apigateway-portal-responses-return-a-host-port-url.md covered only the earlier host:port spelling of this member on CreatePortal/GetPortal/UpdatePortal — the shape is now right, the host ownership is not, and a resolved issue covers nothing. A fixer should expect this to be the last step of that same family.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true, bootstrap_signal false, 1057 steps, 888 passed, score 84.0%).

The single cluster with empty stub_path AND empty owned_by:
- divergence_key `service=apigateway|CreatePortal|status=201->201|keys=$.endpointConfiguration.portalDefaultDomainName`
- verdict: mismatch; expected_status 201, actual_status 201 (status and all other members match)
- fixture apigatewayv2-portal-with-included-portal-product, step setup#2; affected_count 2, also apigatewayv2-portal-with-included-product#setup#2
- detail: `$.endpointConfiguration.portalDefaultDomainName: host_not_own (expected 'p-{{bundling_portal_id}}.apigw-portal.us-west-2.on.aws', got 'p-z1hvlts8lq.apigw-portal.us-west-2.on.aws')`

Expected: a portal default domain name that is the portal's own generated host AND a host the service answers for. Actual: a correctly shaped host that no emulator listener claims; the portal is served only at the emulator's own endpoint with the '/_aws/portal/<portalId>' path (see crate::state::announced_portal_site_base, services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/state.rs:4196, and the domain minted at state.rs:4007 / used from ops/create_portal.rs:309).

The generated portal id ('z1hvlts8lq' in this run), the emulator's host and port are per-run values and are not the requirement; the requirement is that the announced portal default domain be a host the emulator owns and serves the portal on.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus.
2. Replay apigatewayv2-portal-with-included-portal-product#setup#2 (and apigatewayv2-portal-with-included-product#setup#2): POST /v2/portals creating a portal with an included portal product.
3. Read endpointConfiguration.portalDefaultDomainName from the 201 body — it is 'p-<portalId>.apigw-portal.<region>.on.aws'.
4. Address a portal request to that host on the emulator's data listener (Host: p-<portalId>.apigw-portal.<region>.on.aws). No listener claims it; the portal is only reachable at <emulator-endpoint>/_aws/portal/<portalId>, so the announced domain is not an address the emulator is reached on (replay verdict host_not_own).
5. Expected: the member names a host the service answers for — the generated p-<portalId>.apigw-portal.<region>.on.aws is claimed host-style by the portal data plane, as the crate's own announced_portal_site_base documentation states.
