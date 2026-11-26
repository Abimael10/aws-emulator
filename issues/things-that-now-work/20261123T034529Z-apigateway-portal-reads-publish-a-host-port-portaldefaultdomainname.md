Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-portal-create-update-delete-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-portal-create-update-delete-lifecycle apigatewayv2-portal-multiple-isolation
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigateway|GetPortal|2xx->2xx|$.endpointConfiguration.portalDefaultDomainName
Issue-Key: apigateway|UpdatePortal|2xx->2xx|$.endpointConfiguration.portalDefaultDomainName
Issue-Commit: 616e510b6c9fc5a742ae081415d2f4cf49efcb62

# apigateway portal reads publish a host:port portalDefaultDomainName instead of a DNS host

> _Note: this issue was identified against the code base at commit `616e510b6c9fc5a742ae081415d2f4cf49efcb62`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetPortal

## Description
The read and update paths for a portal leak the emulator's own data-listener address into the portal representation, so the endpoint configuration AWS returns and the one the emulator returns are not the same kind of value. Two net-new clusters, one root cause; both were reported with empty stub_path and empty owned_by.

Cause 1 of 2: GetPortal returns endpointConfiguration.portalDefaultDomainName as an authority (host plus port) rather than a domain name.
AWS: Smithy (com.amazonaws.apigatewayv2#ApiGatewayV2) GetPortal - http GET /v2/portals/{PortalId}, code 200; output member EndpointConfiguration (jsonName 'endpointConfiguration', target EndpointConfigurationResponse). EndpointConfigurationResponse.PortalDefaultDomainName (jsonName 'portalDefaultDomainName', target __stringMin3Max256, @length 3..256, @required/clientOptional) is documented as 'The portal default domain name. This domain name is generated and managed by API Gateway.' A domain name is dot-separated host labels only: no scheme, no ':port', no path. The recording asserts that member by shape rather than by literal value because the generated host is per-account, so the requirement is the form of the value, not any recorded host.
Emulator: serializes the member as '<listener-host>:<listener-port>' - the replay detail is `$.endpointConfiguration.portalDefaultDomainName: value_mismatch (expected '<shaped-not-asserted>', got '127.0.0.1:43511')`. The particular host and port are per-run values and are not the requirement; what diverges is that an authority with a port is emitted where a bare DNS host name belongs. Status is 200 on both sides and every other member of the response matches.
Why: a client reads this member to build 'https://<portalDefaultDomainName>', to point a CNAME or a Route 53 alias record at the portal, or to validate the portal host; a value carrying ':port' is not a DNS name at all, so DNS wiring is impossible and hostname validation rejects the portal the describe call just returned.
Fix: the portal representation builder behind GetPortal's serializer (the same one that mints endpointConfiguration for the create and list paths) - emit host labels only and keep the emulator's listener port out of the member.
Traces: apigatewayv2-portal-create-update-delete-lifecycle#task#2.

Cause 2 of 2: UpdatePortal echoes the same malformed member on the mutation response.
AWS: Smithy UpdatePortal - http PATCH /v2/portals/{PortalId}, code 200; output member EndpointConfiguration (jsonName 'endpointConfiguration', target EndpointConfigurationResponse), i.e. the identical PortalDefaultDomainName binding and the same hostname-only requirement. The recorded response asserts it by shape.
Emulator: the update response carries the same '<host>:<port>' authority - detail `$.endpointConfiguration.portalDefaultDomainName: value_mismatch (expected '<shaped-not-asserted>', got '127.0.0.1:43511')` - which confirms the fault is in the shared portal representation and not in one read handler.
Why: a client that updates portal content and re-reads the endpoint configuration from the mutation response (the usual single-round-trip pattern) stores a non-DNS value and wires its front door to an unreachable name.
Fix: same component - whatever shared portal-to-wire mapping UpdatePortal's serializer uses must produce the hostname-only form, so fixing it once covers read, update, create and list.
Traces: apigatewayv2-portal-create-update-delete-lifecycle#task#4.

Related open issues that do NOT carry these replay keys: 20261119T122508Z-apigateway-createportal-announces-a-portal-default-domain-the.md (CreatePortal only) and 20261113T182955Z-apigateway-listportals-summaries-publish-a-host-port-portal.md (ListPortals only). The GetPortal and UpdatePortal keys above are unowned and are filed here; a fixer should expect one shared root cause across all four.

## Evidence
Replay of simparity-wire-aws-v1 (116 fixtures, 1057 steps, score 85.3%). Clusters with empty stub_path and empty owned_by:

1) divergence_key `service=apigateway|GetPortal|status=200->200|keys=$.endpointConfiguration.portalDefaultDomainName`, fixture apigatewayv2-portal-create-update-delete-lifecycle step task#2, verdict mismatch, expected 200 / actual 200, 4 affected steps (apigatewayv2-portal-create-update-delete-lifecycle#task#2, #task#5; apigatewayv2-portal-multiple-isolation#task#4, #task#5). Expected: portalDefaultDomainName asserted by shape (a generated DNS host name). Actual: '127.0.0.1:43511' - an authority with a port.

2) divergence_key `service=apigateway|UpdatePortal|status=200->200|keys=$.endpointConfiguration.portalDefaultDomainName`, fixture apigatewayv2-portal-create-update-delete-lifecycle step task#4, verdict mismatch, expected 200 / actual 200, 1 affected step. Same expected-vs-actual as above.

The host and port in the actuals are per-run values; the requirement, from EndpointConfigurationResponse.PortalDefaultDomainName (@length 3..256, documented as a domain name API Gateway generates and manages), is that the member be a bare DNS host name with no port and no path.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay simparity-wire-aws-v1.
2. Replay apigatewayv2-portal-create-update-delete-lifecycle#task#2: GET /v2/portals/{portalId} after CreatePortal. Inspect endpointConfiguration.portalDefaultDomainName - it is '<host>:<port>' (the emulator's data listener) instead of a generated DNS host name.
3. Replay apigatewayv2-portal-create-update-delete-lifecycle#task#4: PATCH /v2/portals/{portalId} with new portalContent. The 200 response echoes the same '<host>:<port>' value.
4. Expected on both: a hostname-only portalDefaultDomainName (dot-separated host labels, 3..256 chars, no port and no path), with the rest of the response unchanged.
