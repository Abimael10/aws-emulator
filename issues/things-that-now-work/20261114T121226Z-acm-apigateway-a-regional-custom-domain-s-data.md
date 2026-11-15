Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__apigateway.regional-custom-domain-refuses-a-pending-certificate-then-serves-the-issued-one
Issue-Key: acm__apigateway.regional-custom-domain-refuses-a-pending-certificate-then-serves-the-issued-one|_poll_until#48|Timeout|last = probe()
Issue-Commit: b99723d2552fb1e14ee46a597822867680795a41

# acm__apigateway: a REGIONAL custom domain's data plane never becomes observable — the ISSUED-certificate presentation leg reaches no verdict and the check is killed at the 60 s cap

## Resolution

The BLOCKING loop this report identifies — candidate (a), "a Route 53 ALIAS whose DNSName target
is a host:port authority rather than a DNS host resolves no IPv4 A record, so the TestDNSAnswer
poll never returns an address" — is FIXED, and the custom domain's data plane is reachable at the
address Route 53 now answers with:

* `TestDNSAnswer` resolves an alias whose target names an ADDRESS to that address
  (`services/service-com-amazonaws-route53-awsdnsv20130401/src/ops/test_dns_answer.rs`,
  `src/state.rs::dns_name_address`): an `A` query is answered with an IPv4 address, an `AAAA`
  query with an IPv6 one, and the port a data plane is dialled on is no part of the answer.
  Pinned by `tests/all/alias_address_answers.rs` in that crate.
* The address really is the custom domain's front door: `core/router/tests/all/apigateway_custom_domain_dns_alias.rs`
  creates a REGIONAL custom domain, aliases it from a hosted zone exactly as a client does,
  resolves the alias through `TestDNSAnswer` and dials what came back — the front door there
  answers `403` for a Host no base path mapping covers.
* The TLS half of that front door — the leaf `regionalCertificateArn` names, selected by
  `ClientHello` SNI, the `securityPolicy` floor and the mutual-TLS contract — was already served
  by `services/service-com-amazonaws-apigateway-backplanecontrolservice/src/service/custom_domain_socket.rs`
  and is pinned by that crate's `tests/all/regional_custom_domain_front_door.rs`.

What REMAINS for this check, and it is not these service crates' to serve: the check dials the
resolved address on the port `https` implies (443), while an `A` record carries an address and no
port. `Listeners::tcp_plaintext` (`vendor/server/src/listeners.rs`, binding through
`bind_tokio`/`bind_std`) takes no requested port — it binds an OS-assigned port inside
`EMULATOR_LISTENER_PORT_RANGE` — and the core's TLS presents the one process certificate with no
per-connection certificate resolver (`vendor/server/src/tls_certificate_authority.rs`), so neither
can the custom domain's front door answer on 443 nor can the emulator's data port present that
domain's ACM leaf. It needs the core listener API to accept a port (the same core gap recorded for
the RDS engine endpoints in
`issues/things-that-now-work/20261104T154127Z-rds-the-aurora-cluster-endpoint-admits-no-sql-session.md`).

> _Note: this issue was identified against the code base at commit `b99723d2552fb1e14ee46a597822867680795a41`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomainName

## Description
1 net-new root cause for the acm__apigateway pair. Related open issues that do NOT cover it: 20261111T014037Z-apigateway-custom-domain-representation-diverges-on-domain-arn.md (control-plane REPRESENTATION of domainNameArn/hostedZoneId/tags/published host — a field-value divergence, not an absent data plane) and 20261025T210904Z-acm-apigatewayv2-a-regional-custom-domain-never-presents.md (the same shape of defect in the apigatewayv2 service, a different service token and a different crate). Neither carries this check's replay key, and the report marked this failure neither ALREADY FILED nor OWNED.

Cause 1 of 1: the REST (apigateway) REGIONAL custom-domain flow never reaches a terminal observation. The check is killed with 'Error: Check timed out after 60.0s' having produced no verdict at all, against a trajectory whose own declared waits are far longer (imported certificate -> ISSUED 120 s, domainNameStatus -> AVAILABLE 300 s, ALIAS -> INSYNC 120 s, TestDNSAnswer -> A record 60 s, first verified TLS handshake 60 s). So the budget was consumed inside one blocking poll, not by a wrong field value.

AWS: a REGIONAL custom domain is a real TLS terminator reachable through DNS. Smithy (com.amazonaws.apigateway): DomainName$regionalCertificateArn — "The reference to an Amazon Web Services-managed certificate that will be used for validating the regional domain name. Certificate Manager is the only supported source"; DomainName$regionalDomainName — "The domain name associated with the regional endpoint for this custom domain name. You set up this association by adding a DNS record that points the custom domain name to this regional domain name"; DomainName$regionalHostedZoneId — "The region-specific Amazon Route 53 Hosted Zone ID of the regional endpoint"; DomainName$domainNameStatus — AVAILABLE | UPDATING; DomainName$securityPolicy — "The Transport Layer Security (TLS) version + cipher suite for this DomainName". Beyond the model, recorded behavior only: regionalDomainName is a DNS HOST (no port) that a Route 53 ALIAS record, targeted with regionalHostedZoneId, resolves to IPv4 A records; a TLS handshake to those addresses with ClientHello SNI = the custom domain presents, byte for byte, the leaf whose ARN was given as regionalCertificateArn (selection is by SNI, RFC 6066 §3); the negotiated version is >= TLS 1.2 when securityPolicy is TLS_1_2; and the HTTP layer behind that terminator answers HTTP 403 for a custom domain with no base path mapping.

Emulator: the control plane answers, and the certificate-state gate is correct — services/service-com-amazonaws-apigateway-backplanecontrolservice/src/state/acm_certificate_state.rs resolves each named ARN through acm:DescribeCertificate and refuses a non-ISSUED one with BadRequestException carrying exactly the fixed text 'Certificate is not in an ISSUED state, and cannot be used' (CERTIFICATE_NOT_ISSUED), which is the message this check asserts. What never happens is the DATA plane: create_domain_name.rs opens a listener via crate::state::open_custom_domain_endpoint and publishes endpoint.authority() — a host:port authority — into regionalDomainName, and nothing in the emulator binds that listener to the domain's regionalCertificateArn leaf, so no verified TLS handshake with SNI = the custom domain ever completes and no 403-for-unmapped-domain ever arrives. The two candidate blocking loops are both emulator-side: (a) a Route 53 ALIAS whose DNSName target is a host:port authority rather than a DNS host resolves no IPv4 A record, so the TestDNSAnswer poll never returns an address; (b) even given an address, the handshake loop retries transport/handshake failures until the deadline because the custom domain presents no certificate. Hypothesis: (a) fires first, since the published authority carries a port. Either way the stored mutualTls/securityPolicy/regionalCertificateArn configuration is write-only.

Why: a client relying on AWS's behavior cannot serve or test TLS on a REST custom domain at all. CreateDomainName becomes a stored-but-unproven config write: the ARN is echoed back, but no client is ever presented that certificate, the securityPolicy floor is unenforced, the documented 403-for-unmapped-domain response never arrives, and the published regionalDomainName cannot be used as a Route 53 ALIAS target. Worse than a refusal, the call HANGS — nothing distinguishes 'not ready yet' from 'will never be ready', so a deployment or test that pins a leaf or probes the domain's endpoint blocks instead of failing.

Fix: the apigateway REGIONAL custom-domain data plane. The listener opened by crate::state::open_custom_domain_endpoint must terminate TLS using the leaf referenced by the record's regionalCertificateArn (resolved from ACM state), select it by ClientHello SNI, honour securityPolicy as a minimum-version floor, and answer HTTP 403 when no base path mapping matches the Host; and regionalDomainName must publish an address that the emulator's own Route 53 can take as an ALIAS target and resolve to an A record. File citations are under Traces; the control-plane record already carries every needed field.

Traces: acm__apigateway.regional-custom-domain-refuses-a-pending-certificate-then-serves-the-issued-one (mismatch, 'Check timed out after 60.0s'). Component files: services/service-com-amazonaws-apigateway-backplanecontrolservice/src/ops/create_domain_name.rs lines 479-496 (open_custom_domain_endpoint) and 553-559 (regional_domain_name = endpoint.authority()); src/state.rs lines 1428-1448 (DomainNameEntry endpoint/security_policy/mutual_tls_authentication) and 2437-2462 (DOMAIN_NAME_CREATE_SPAN = 500 ms, so the AVAILABLE poll is not the stall); src/state/acm_certificate_state.rs lines 60-69 and 198-230 (the certificate-state gate, which is correct).

## Evidence
Expected (AWS): acm:ImportCertificate of a self-signed leaf (DNS SAN = the custom domain) reaches Status=ISSUED; acm:RequestCertificate(ValidationMethod=DNS) for the same name stays PENDING_VALIDATION; apigateway:CreateDomainName(domainName, regionalCertificateArn=<issued>, endpointConfiguration={types:[REGIONAL]}, securityPolicy=TLS_1_2) returns 201 echoing regionalCertificateArn; the same create with the PENDING_VALIDATION ARN is refused HTTP 400 BadRequestException with message exactly 'Certificate is not in an ISSUED state, and cannot be used' (all fixed text — it carries no ARN, no actual status and no member name, which is what keeps it distinct from the malformed-ARN refusal) and leaves no domain behind; the re-created domain reaches domainNameStatus=AVAILABLE with a non-empty regionalDomainName AND regionalHostedZoneId; a Route 53 ALIAS A record for the custom domain targeting {DNSName: regionalDomainName, HostedZoneId: regionalHostedZoneId} reaches INSYNC and TestDNSAnswer returns NOERROR with IPv4 RecordData; a TLS connection to one of those addresses on port 443 with server_hostname = the custom domain, trusting only the importing root, completes, presents exactly the imported leaf's DER, negotiates TLSv1.2 or TLSv1.3, and the tunnel carries a well-formed 'HTTP/1.1 403 ...' status line because no base path mapping exists; GetDomainNames then lists the domain.

Actual (emulator): the check reaches no terminal assertion. Replay verdict: mismatch, 'Error: Check timed out after 60.0s'. No presented-certificate DER, no negotiated TLS version, no 403 status line and no final GetDomainNames listing is ever observed. The 60 s figure is the harness ceiling, not an AWS requirement — the divergence reported is the absent/unreachable custom-domain TLS endpoint, not the wall-clock number. The control-plane handlers return promptly (create_domain_name.rs mints and stores the record including regionalDomainName/regionalHostedZoneId), which is why the failure manifests as a hang in a readiness/handshake retry loop rather than as an API error.

## Steps to Reproduce
1. Mint a private RSA-2048 root CA and a leaf signed by it carrying DNS SAN = <domain>, where <domain> is a label under a random parent under a real TLD.
2. acm:ImportCertificate(Certificate=leaf PEM, PrivateKey=leaf key PEM, CertificateChain=root PEM) -> issued_arn; poll acm:DescribeCertificate until Status=ISSUED.
3. acm:RequestCertificate(DomainName=<domain>, ValidationMethod=DNS) -> pending_arn; confirm DescribeCertificate reports PENDING_VALIDATION.
4. apigateway:CreateDomainName(domainName=<domain>, regionalCertificateArn=issued_arn, endpointConfiguration={types:[REGIONAL]}, securityPolicy=TLS_1_2). Expect 201 with regionalCertificateArn echoed. Then DeleteDomainName and poll GetDomainNames until the name is gone.
5. CreateDomainName again with regionalCertificateArn=pending_arn. Expect 400 BadRequestException, message exactly 'Certificate is not in an ISSUED state, and cannot be used', and GetDomainNames still not listing <domain>.
6. CreateDomainName again with issued_arn; poll GetDomainName until domainNameStatus=AVAILABLE and read regionalDomainName + regionalHostedZoneId.
7. route53:CreateHostedZone(Name=<parent>); ChangeResourceRecordSets CREATE an A ALIAS for <domain> with AliasTarget={DNSName: regionalDomainName, HostedZoneId: regionalHostedZoneId}; poll GetChange until INSYNC; poll route53:TestDNSAnswer(RecordName=<domain>, RecordType=A) for NOERROR with IPv4 RecordData. Observed: no IPv4 address is ever returned (the published regionalDomainName is a host:port authority, not an ALIAS-targetable DNS host).
8. TCP-connect to the resolved address on 443 and wrap it in a TLS client context trusting ONLY the private root, check_hostname=True, server_hostname=<domain>; send 'GET / HTTP/1.1' with Host: <domain>. Expect a completed handshake, peer DER == the imported leaf's DER, negotiated version in {TLSv1.2, TLSv1.3}, and an 'HTTP/1.1 403' status line. Observed: the flow is still retrying when the harness cap fires; the check is killed with no verdict.
