Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__cloudfront__s3.tenant-only-distribution-routing-endpoint-rejects-direct-viewer-request
Issue-Key: acm__cloudfront__s3.tenant-only-distribution-routing-endpoint-rejects-direct-viewer-request|_tls_handshake_rejection#29|AssertionError|raise AssertionError(
Issue-Commit: 14940b74adde738ffb545252b71aede81788f071

# acm__cloudfront__s3: a viewer request addressed to a connection group's own routing endpoint is answered 502 "This address serves no distribution." instead of failing the TLS handshake

> _Note: this issue was identified against the code base at commit `14940b74adde738ffb545252b71aede81788f071`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetConnectionGroup

## Description
Cause 1 of 1: the CloudFront edge completes the TLS handshake for a request sent to a connection group's RoutingEndpoint and answers an HTTP error, where AWS refuses the handshake itself.
AWS: recorded behavior only — a tenant-only distribution is reachable only through a tenant's own domain CNAMEd to its connection group's routing endpoint. A viewer that dials the routing endpoint directly (SNI/Host = the routing endpoint, not a tenant domain) gets no certificate for that name, so TLS terminates with a handshake failure alert; the recording shows the client's ssl.SSLError reason SSLV3_ALERT_HANDSHAKE_FAILURE and no HTTP response at all, even though both the connection group and the tenant are Deployed.
Emulator: the handshake succeeds and the edge answers HTTP 502 with the body "This address serves no distribution.\n" (emulator-authored text; no AWS error code or body matches it).
Why: the request is refused one layer too late. A client sees a served, trusted TLS session for a name CloudFront never has a certificate for — so hostname/SNI-dependent behavior (and any test that a tenant is reachable only through its own domain) passes against the emulator and fails against AWS; a 502 also reads as a transient origin problem and invites retries where AWS's refusal is terminal.
Fix: the CloudFront edge/viewer front end for connection-group routing endpoints — a TLS connection whose SNI (or Host) is the routing endpoint itself, rather than a tenant domain bound to that connection group, must be rejected during the handshake (handshake_failure alert) instead of being completed and answered with an HTTP body.
Traces: acm__cloudfront__s3.tenant-only-distribution-routing-endpoint-rejects-direct-viewer-request.

## Evidence
Expected: requests.get(https://<RoutingEndpoint>/index.html) raises an SSLError whose TLS alert reason is SSLV3_ALERT_HANDSHAKE_FAILURE (no HTTP status served).
Actual: AssertionError: expected the TLS handshake to fail, but the connection succeeded with status 502 and body b'This address serves no distribution.\n' (check_acm__cloudfront__s3_tenant_only_distribution_routing_endpoint_rejects_direct_viewer_request.py line 152). The distribution itself does report DomainName '-' and the connection group/tenant both reach Deployed, so the divergence is isolated to the edge's handling of the routing endpoint.

## Steps to Reproduce
1. Create an S3 bucket plus an origin access control, and create a tenant-only CloudFront distribution over that origin (origin path /{{TenantPath}}).
2. CreateDistributionTenant with a domain and ManagedCertificateRequest{ValidationTokenHost: self-hosted}; wait for the distribution, its default connection group and the tenant to all report Deployed.
3. Read the connection group's RoutingEndpoint from GetConnectionGroup and issue an HTTPS GET to https://<RoutingEndpoint>/index.html.
4. The emulator completes the TLS handshake and answers 502 with body "This address serves no distribution.\n"; AWS fails the handshake with SSLV3_ALERT_HANDSHAKE_FAILURE.
