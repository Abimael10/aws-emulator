Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__apigateway__s3.regional-custom-domain-mutual-tls-enforces-trusted-client-certificates
Issue-Key: acm__apigateway__s3.regional-custom-domain-mutual-tls-enforces-trusted-client-certificates|_poll_request#44|Timeout|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: b99723d2552fb1e14ee46a597822867680795a41

# acm__apigateway__s3: the mutual-TLS REGIONAL custom domain still never becomes observable — neither a trusted nor an absent client certificate gets an answer, and the check is killed at the 60 s cap

## Resolution

The last item of this report's own Fix list — "route a trusted request through the matching base
path mapping to its integration" — did not hold for the api shape this check builds, and it is
FIXED. Everything else the Fix list names (the leaf selected by `ClientHello` SNI, the mandatory
client certificate, the truststore resolved from `truststoreUri`/`truststoreVersion` through Amazon
S3, the TLS-layer refusal of a client presenting none, and `403` `{"message":"Forbidden"}` with
`x-amzn-ErrorType: ForbiddenException` for one signed outside the truststore) was already served by
`services/service-com-amazonaws-apigateway-backplanecontrolservice/src/service/custom_domain_socket.rs`
and stays pinned by that crate's `tests/all/regional_custom_domain_front_door.rs` and
`tests/all/issued_certificate_fronts_a_mutual_tls_domain.rs`.

Two emulator-side defects stood between a TRUSTED client and the stage:

* the api this check creates carries `disableExecuteApiEndpoint=true`, and the data plane refused
  EVERY request of such an api `403` whatever endpoint it arrived at — while the member turns off
  the default `execute-api` endpoint alone ("To require that clients use a custom domain name to
  invoke your API, disable the default endpoint"). The plane now resolves a request addressed to a
  custom domain name of the account as its own addressing and refuses only the default endpoint
  (`src/service/invoke.rs`, `src/service/invoke/custom_domain_arrival.rs`);
* the front door RE-ADDRESSED the request it relayed to the api's own reserved data-plane path, so
  the SigV4 signature of the `AWS_IAM` method this check invokes was verified against a request no
  client ever sent. The relay now hands the plane the client's own request — verb, path, query,
  headers, body — and the api and stage are read from the record's base path mappings instead
  (`src/service/custom_domain_socket.rs`).

Both rules are pinned over tables of inputs by that crate's `tests/all/custom_domain_arrival.rs`,
which dials the address the record publishes.

What REMAINS for this check, and it is not this service crate's to serve: the check dials
`regionalDomainName:443`, while the front door answers on the port the core assigned it.
`Listeners::tcp_plaintext` (`vendor/server/src/listeners.rs`, binding through `bind_tokio`/
`bind_std`) takes no requested port — it binds an OS-assigned port, or the first free port in
`EMULATOR_LISTENER_PORT_RANGE` — and the emulator's own data port presents the one process
certificate with no per-connection certificate resolver, so nothing inside `services/` can make a
custom domain's front door answer on 443. That is the same core gap recorded for the sibling check
in `issues/things-that-now-work/20261114T121226Z-acm-apigateway-a-regional-custom-domain-s-data.md`
and in `core/router/tests/all/apigateway_custom_domain_dns_alias.rs`; until the core listener API
accepts a port, this check's four beats stay unobservable however faithfully the data plane behind
them answers.

> _Note: this issue was identified against the code base at commit `b99723d2552fb1e14ee46a597822867680795a41`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomainName

## Description
1 net-new root cause for the acm__apigateway__s3 triple. Two open issues name this same check id but the report marked this failure neither ALREADY FILED nor OWNED, so neither carries its current replay key: 20261113T183248Z-acm-apigateway-s3-the-mutual-tls-regional-custom-957ca8.md and 20261110T152935Z-acm-apigateway-s3-the-mutual-tls-regional-custom.md (the latter attributes the stall to a DNS-validated certificate never reaching ISSUED, which is a different point in the trajectory). This filing records the cause as it reproduces now, with the emulator-side evidence below; if a fixer closes it together with those, it must re-replay this check id.

Cause 1 of 1: the mutual-TLS REGIONAL custom-domain data plane produces no observation at all — neither a served response nor a TLS refusal. The check is killed with 'Error: Check timed out after 60.0s' and reaches none of its four beats, even though its own waits are budget-aware (they would report 'the run budget ended first; not graded' rather than being killed), so the budget was consumed inside a single blocking socket/poll step.

AWS: a REGIONAL custom domain created with mutualTlsAuthentication performs two-way TLS. Smithy (com.amazonaws.apigateway): MutualTlsAuthentication — "The mutual TLS authentication configuration for a custom domain name. If specified, API Gateway performs two-way authentication between the client and the server. Clients must present a trusted certificate to access your API"; MutualTlsAuthentication$truststoreUri — "An Amazon S3 URL that specifies the truststore for mutual TLS authentication, for example s3://bucket-name/key-name"; MutualTlsAuthentication$truststoreVersion — "The version of the S3 object that contains your truststore"; DomainName$regionalDomainName, DomainName$domainNameStatus, DomainName$securityPolicy as modelled. Beyond the model, recorded behavior only: (1) a SigV4-signed GET presenting a client certificate the truststore trusts is served HTTP 200 with the integration's body; (2) a request presenting NO client certificate is refused at the TLS layer — a connection reset, a teardown with no response, or alert TLSV13_ALERT_CERTIFICATE_REQUIRED / SSLV3_ALERT_HANDSHAKE_FAILURE — never an HTTP response; (3) a client certificate signed by a CA outside the truststore completes the handshake and is refused at the HTTP layer with status 403, body exactly {"message":"Forbidden"} and header x-amzn-ErrorType: ForbiddenException; (4) the trusted path still serves 200 afterwards. Every one of those answers, success or refusal.

Emulator: mutualTlsAuthentication is STORED and never enforced. services/service-com-amazonaws-apigateway-backplanecontrolservice/src/state.rs carries DomainNameEntry::mutual_tls_authentication and security_policy, and create_domain_name.rs opens a listener through crate::state::open_custom_domain_endpoint and publishes endpoint.authority() as regionalDomainName, but nothing resolves the S3 truststore object, requests a client certificate in the handshake, validates it against that truststore, or presents the domain's regionalCertificateArn leaf. The result is a listener that accepts the connection and then neither completes nor refuses the handshake, so the client hangs; the three refusal/serve outcomes above are all unobservable. (recorded behavior only for the handshake outcomes themselves.)

Why: a client that creates a mutual-TLS custom domain and connects to it gets neither bytes nor an error. A hung TLS endpoint is worse than a refusal: nothing distinguishes 'not ready' from 'client certificate rejected' from 'certificate accepted', so the truststore's entire purpose — admitting trusted clients and refusing everyone else — is unverifiable, and any test or deployment that validates mTLS blocks indefinitely instead of passing or failing.

Fix: the apigateway REGIONAL custom-domain data plane and its mutual-TLS enforcement. The listener behind the published regionalDomainName must present the leaf named by regionalCertificateArn (selected by ClientHello SNI), request a client certificate when the record carries mutualTlsAuthentication, resolve truststoreUri/truststoreVersion against S3 state as the client trust anchor, refuse a handshake presenting no client certificate with a TLS alert or reset, answer 403 with body {"message":"Forbidden"} and x-amzn-ErrorType: ForbiddenException for a certificate signed outside the truststore, and route a trusted request through the matching base path mapping to its integration. The stored record already carries every needed field.

Traces: acm__apigateway__s3.regional-custom-domain-mutual-tls-enforces-trusted-client-certificates (mismatch, 'Check timed out after 60.0s'). Component files: services/service-com-amazonaws-apigateway-backplanecontrolservice/src/ops/create_domain_name.rs lines 479-496 and 553-559; src/state.rs line 1448 (mutual_tls_authentication stored) and lines 2437-2462 (DOMAIN_NAME_CREATE_SPAN = 500 ms, so the AVAILABLE poll is not the stall); src/state/acm_certificate_state.rs lines 198-230.

## Evidence
Expected (AWS): after an ACM certificate for <domain> reaches ISSUED, a CA bundle is uploaded to a versioned S3 bucket as truststore.pem, and apigateway:CreateDomainName(domainName=<domain>, endpointConfiguration={types:[REGIONAL]}, regionalCertificateArn=<arn>, securityPolicy=TLS_1_2, mutualTlsAuthentication={truststoreUri: s3://<bucket>/truststore.pem, truststoreVersion: <VersionId>}) plus a base path mapping to a deployed MOCK-integration stage: (1) a SigV4-signed GET over TLS presenting a leaf signed by the truststore CA is answered HTTP 200 with the integration's body; (2) the same GET presenting NO client certificate is refused at the TLS layer — reset, silent teardown, or alert TLSV13_ALERT_CERTIFICATE_REQUIRED / SSLV3_ALERT_HANDSHAKE_FAILURE — and never yields an HTTP status; (3) the same GET presenting a leaf signed by a DIFFERENT CA completes the handshake and is answered HTTP 403 with body exactly {"message":"Forbidden"} (fixed text; no request or state values in it) and header x-amzn-ErrorType: ForbiddenException; (4) the trusted client is served 200 again afterwards.

Actual (emulator): none of the four beats produces a value. Replay verdict: mismatch, 'Error: Check timed out after 60.0s' — no HTTP status, no response body, no TLS alert and no reset is ever observed. The check's polls are budget-aware and would have reported 'the run budget ended first; not graded' instead of being killed, so the whole budget was spent inside one blocking handshake or poll. The 60 s is the harness ceiling, not an AWS requirement: the divergence is a mutual-TLS custom-domain endpoint that neither completes nor refuses a handshake.

## Steps to Reproduce
1. Pick <domain> = <label>.<apex> in the account's Route 53 baseline zone and issue a DNS-validated ACM certificate for it (RequestCertificate -> write the validation CNAME -> poll DescribeCertificate to ISSUED).
2. s3:CreateBucket(<bucket>) with the region's LocationConstraint; PutBucketVersioning Status=Enabled; mint a private RSA-2048 root CA 'trusted' and a CLIENT_AUTH leaf signed by it, plus a second unrelated root CA 'other' and a CLIENT_AUTH leaf signed by that; s3:PutObject(Key=truststore.pem, Body=<trusted CA PEM>) and keep the returned VersionId.
3. apigateway:CreateRestApi(endpointConfiguration={types:[REGIONAL]}, disableExecuteApiEndpoint=true); PutMethod GET authorizationType=AWS_IAM on /; PutIntegration type=MOCK returning statusCode 200; PutMethodResponse/PutIntegrationResponse 200 with a fixed JSON body; CreateDeployment(stageName=prod).
4. apigateway:CreateDomainName(domainName=<domain>, endpointConfiguration={types:[REGIONAL]}, regionalCertificateArn=<arn>, securityPolicy=TLS_1_2, mutualTlsAuthentication={truststoreUri: s3://<bucket>/truststore.pem, truststoreVersion: <VersionId>}); CreateBasePathMapping(domainName=<domain>, restApiId=<id>, stage=prod).
5. Poll GetDomainName until domainNameStatus=AVAILABLE and read regionalDomainName.
6. Open a TLS connection to regionalDomainName:443 with server_hostname=<domain> presenting the TRUSTED client leaf and send a SigV4-signed (execute-api) 'GET / HTTP/1.1' with Host: <domain>. Expect 200 and the fixed body. Observed: the connection neither completes a handshake nor is refused; the step blocks until the harness cap fires.
7. Repeat step 6 with NO client certificate. Expect a TLS-layer refusal (reset, silent teardown, or certificate-required alert) and no HTTP status. Observed: unreachable — the check was already killed.
8. Repeat step 6 presenting the leaf signed by the 'other' CA. Expect a completed handshake then HTTP 403 with body {"message":"Forbidden"} and x-amzn-ErrorType: ForbiddenException. Observed: unreachable.
9. Repeat step 6 with the trusted leaf. Expect 200 again. Observed: unreachable.
