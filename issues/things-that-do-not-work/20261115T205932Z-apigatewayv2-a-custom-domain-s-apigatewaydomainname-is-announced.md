Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2.e2e-custom-domain-mapping-invoke-flow
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2.e2e-custom-domain-mapping-invoke-flow|_fetch_status_via_ip#18|PublishedUrlRejected|raw = socket.create_connection(ctx.endpoint_host(ip, 443), timeout=timeout)
Issue-Commit: 65202ee683bcb66d82ebd52a38b6336bbaa34d8b

# apigatewayv2: a custom domain's ApiGatewayDomainName is announced as a host:port authority, so its alias/CNAME target is not a DNS name and the custom-domain data plane is unreachable

> _Note: this issue was identified against the code base at commit `65202ee683bcb66d82ebd52a38b6336bbaa34d8b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDomainName

## Description
One net-new root cause in the apigatewayv2 custom-domain surface, grounded in a failing behavioral check of this replay.

Related open issues that do NOT cover this cause and do not carry this check's replay key: 20261111T014037Z-apigatewayv2-getdomainname-returns-an-account-qualified-domain-arn.md (a WIRE finding keyed on apigatewayv2|GetDomainName|$.domainNameConfigurations[*].apiGatewayDomainName, recorded when the emulator announced the AWS-shaped 'd-*.execute-api.<region>.amazonaws.com' host; the value observed here is a different one - an authority carrying the emulator's listener port - and that issue holds no behavioral check id), and 20261027T053233Z-apigatewayv2-9-net-new-root-causes-api-mapping.md cause 8 (the custom-domain data plane timing out, grounded in apigatewayv2.custom-domain-mapping-key-routes-to-stage-routes and apigatewayv2.deploy-api-mapping-cfn-behaves, not in this check).

Cause 1 of 1: the alias target handed out for a custom domain is not a resolvable DNS host, so nothing can be reached through the domain.
AWS: Smithy: apigatewayv2 GetDomainName, http GET /v2/domainnames/{DomainName}, code 200, output member DomainNameConfigurations -> list of DomainNameConfiguration, whose member ApiGatewayDomainName is documented '<p>A domain name for the API.</p>' and whose sibling HostedZoneId is the alias zone for it. ApiGatewayDomainName is the value a caller points its own DNS record at (a CNAME, or a Route 53 A/ALIAS record with AliasTarget.DNSName = that host and AliasTarget.HostedZoneId = HostedZoneId). It is therefore a DNS HOSTNAME - a name, resolvable to an address, with NO port: a DNS record cannot carry a port at all. Resolving a domain whose A record aliases that host (Route 53 TestDNSAnswer with RecordType=A) yields RecordData entries that are IPv4 address literals, and a client then connects to that address on 443 and is served the stage mapped at the ApiMappingKey.
Emulator: after CreateDomainName reaches DomainNameStatus=AVAILABLE and an A/ALIAS record is created with AliasTarget.DNSName = the reported ApiGatewayDomainName, the resolved RecordData[0] comes back as the authority '127.0.0.1:36941.' - a loopback address with the emulator's listener port appended, plus a trailing dot - instead of a bare address literal. The harness refuses that value as a connection target ('simparity.published.PublishedUrlRejected: endpoint_host was given 127.0.0.1\07236941.:443 (published by the emulator or built by the check): its host is not one of the emulator's own hosts'), so the TLS request to the custom domain is never made and the mapped stage is never reached. Hypothesis: ApiGatewayDomainName itself is derived as '<own host>:<listener port>' (the emulator announcing its reachable authority rather than a hostname) and the alias resolution then echoes that string, trailing-dot qualified, as the A record's data; the alternative mechanism with the same evidence is that ApiGatewayDomainName is a port-free host but the alias resolution answers an A query with the target NAME (plus port) instead of resolving it to an address. Both legs must be checked together - the requirement is that the announced value is a port-free name AND that an A-record answer for a domain aliased to it is an address literal.
Why: the announced ApiGatewayDomainName is the whole contract of a custom domain - it is the only thing a customer can put in DNS. A value containing a port cannot be placed in any DNS record, and an A answer that is not an address cannot be connected to, so a client that configures a custom domain exactly as it does on AWS reaches nothing: mapping-key routing, base-path stripping and the per-stage domain layout cannot be exercised locally even though the control plane reports the domain AVAILABLE. The emulator also announces an address other than the one it is reached on, so the failure looks like a client/DNS misconfiguration rather than an emulator gap.
Fix: the apigatewayv2 DomainName state/serializer that derives DomainNameConfigurations[].ApiGatewayDomainName (announce a port-free hostname that the emulator's execute-api front door actually serves, consistent with HostedZoneId), together with the alias-target resolution that an A-record query over that host answers from (an A record must resolve to an address literal, never to a name or an authority with a port).
Traces: apigatewayv2.e2e-custom-domain-mapping-invoke-flow (the ACM import, CreateDomainName, the AVAILABLE poll, the hosted-zone/alias creation and the INSYNC change all succeed; the check dies on the first attempt to connect to the resolved target, before the GetApiMappings / GetApiMapping-after-delete assertions).

## Evidence
Check apigatewayv2.e2e-custom-domain-mapping-invoke-flow [mismatch]:

  File "context.py", line 650, in endpoint_host
    self._published_target(f"{published_host}:{port}", "endpoint_host")
  simparity.published.PublishedUrlRejected: endpoint_host was given 127.0.0.1\07236941.:443 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address

The rejected host is RecordData[0] of the A/ALIAS resolution ('\072' is ':', i.e. the string '127.0.0.1:36941.'), taken from Route 53 TestDNSAnswer(HostedZoneId=<zone for the custom domain>, RecordName=<domain>, RecordType='A') after the record was created with AliasTarget={'DNSName': GetDomainName(...)['DomainNameConfigurations'][0]['ApiGatewayDomainName'], 'HostedZoneId': <...>['HostedZoneId']} and the change reached INSYNC.

Expected: an A-record answer that is an IPv4 address literal (no port, no trailing-dot name), reachable on 443 and serving the stage mapped at the ApiMappingKey; i.e. ApiGatewayDomainName is a port-free DNS hostname (AWS's form is 'd-<id>.execute-api.<region>.amazonaws.com').
Actual: the authority '127.0.0.1:36941.' - loopback address + emulator listener port + trailing dot - which is not a DNS name, is not an address, and is refused as a connection target, so no request crosses the custom domain.

## Steps to Reproduce
1. Boot the emulator from this checkout and point an SDK at it (region/account per the environment).
2. ACM ImportCertificate a self-signed certificate for '<name>.sim2real-test.internal'.
3. apigatewayv2 CreateDomainName(DomainName='<name>.sim2real-test.internal', DomainNameConfigurations=[{'CertificateArn': <arn>, 'EndpointType': 'REGIONAL'}]) and poll GetDomainName until DomainNameConfigurations[0].DomainNameStatus == 'AVAILABLE'.
4. Read DomainNameConfigurations[0].ApiGatewayDomainName and HostedZoneId. Assert ApiGatewayDomainName is a DNS hostname: it must contain NO ':' and no port, and must be a name the emulator's execute-api front door serves.
5. route53 CreateHostedZone for the domain, ChangeResourceRecordSets CREATE an A record for the domain with AliasTarget={'HostedZoneId': <that HostedZoneId>, 'DNSName': <that ApiGatewayDomainName>, 'EvaluateTargetHealth': False}, wait for the change to be INSYNC.
6. route53 TestDNSAnswer(HostedZoneId=<zone>, RecordName=<domain>, RecordType='A') -> every RecordData entry must be a bare IPv4 address literal (ipaddress.ip_address() must accept it); it must not be a name, must not carry a port and must not end in '.'.
7. Create an HTTP API with an HTTP_PROXY integration and a deployed stage, CreateApiMapping it onto the domain, then open a TLS connection to the resolved address on 443 with SNI = the domain and GET the mapped path -> expect HTTP 200 from the mapped stage.
8. Replay apigatewayv2.e2e-custom-domain-mapping-invoke-flow to confirm.
