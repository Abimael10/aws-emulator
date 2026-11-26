Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.deploy-provisioning-template-cfn-issues-claim-certificates
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.deploy-provisioning-template-cfn-issues-claim-certificates|_assert_claim_cert_authenticates#31|SSLEOFError|with tls_context.wrap_socket(sock, server_hostname=endpoint_host) as tls_sock:
Issue-Commit: 108c88ce5712cfd0acccac272e2b422c7e2a7f39

# iot: a CreateProvisioningClaim claim certificate still authenticates no mutual-TLS session at the published iot:Data-ATS endpoint — the listener now aborts the handshake with an unexpected EOF, so the CFN-deployed fleet-provisioning path reaches no verdict

> _Note: this issue was identified against the code base at commit `108c88ce5712cfd0acccac272e2b422c7e2a7f39`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateProvisioningClaim

## Description
One net-new root cause in the iot service's fleet-provisioning data path, grounded in the only failing check in this run that the suite report marks neither ALREADY FILED nor OWNED.

Related but NOT covering it: the open issue 20261115T062827Z-iot-a-claim-certificate-minted-by-createprovisioningclaim-completes.md describes the same component but carries only the replay key of iot.create-provisioning-claim-missing-template-rejected, and it records a different observable refusal (a TLS illegal_parameter alert); what is observed here is a handshake torn down with an unexpected EOF. The open issues about the data-plane address and the broker's policy/certificate-status enforcement (20261110T222008Z-iot-the-published-data-plane-address-serves-no.md, 20261122T200530Z-iot-the-iot-data-ats-mqtt-broker-never.md, 20261119T032013Z-iot-7-net-new-causes-the-published-iot.md) do not describe it either: in this same run the listener does complete TLS and MQTT sessions for ordinary CreateKeysAndCertificate material (those checks fail later, on authorization), so the failure is specific to claim material minted by CreateProvisioningClaim.

Cause 1 of 1: the X.509 claim certificate/key pair CreateProvisioningClaim mints cannot authenticate a mutual-TLS session against the address DescribeEndpoint(endpointType='iot:Data-ATS') publishes; the device-gateway port accepts the TCP connection and then closes it mid-handshake.
AWS: Smithy com.amazonaws.iot#CreateProvisioningClaim — POST /provisioning-templates/{templateName}/provisioning-claim, success 200, output { certificateId (@length 64/64, pattern ^(0x)?[a-fA-F0-9]+$), certificatePem (@length 1..65536), keyPair{PublicKey,PrivateKey}, expiration }; modeled errors InvalidRequestException(400), ResourceNotFoundException(404), ThrottlingException(400), UnauthorizedException(401), InternalFailureException(500), ServiceUnavailableException(503). Smithy com.amazonaws.iot#DescribeEndpoint — GET /endpoint?endpointType, output endpointAddress. The gateway transport itself is not in the model, so for the handshake: recorded behavior only — a claim certificate is minted already usable and ACTIVE, so a TLS client-certificate handshake to the published iot:Data-ATS address on the device-gateway port (8883), presenting certificatePem + keyPair.PrivateKey as the client chain, COMPLETES and negotiates a protocol/cipher. A claim certificate is deliberately absent from the certificate registry (DescribeCertificate/ListCertificates never show it), so this handshake is the only destination-side proof that the issued material is genuinely active. Real AWS refuses only material it does not recognize, and it refuses it as a certificate rejection during the handshake — never by truncating the TLS stream.
Emulator: every step before the handshake succeeded in this run — the CloudFormation stack carrying AWS::IoT::ProvisioningTemplate reached CREATE_COMPLETE, DescribeProvisioningTemplate reported enabled=true, CreateProvisioningClaim returned a PEM/private key/public key and a 64-character certificateId, and DescribeEndpoint published an address. The TCP connection to that address on 8883 is accepted, and the emulator then ends the TLS negotiation without completing it and without a TLS alert: `ssl.SSLEOFError: [SSL: UNEXPECTED_EOF_WHILE_READING] EOF occurred in violation of protocol`, raised from the client's do_handshake(). No MQTT framing is ever reached, so the check errors out with no verdict on the provisioning flow. The host, port and generated template/role/stack names are per-run values and are not part of the requirement; the requirement is that claim material authenticates, and that a rejection is a certificate rejection rather than a truncated stream.
Why: fleet provisioning by claim is defined as 'obtain a claim certificate, then open a mutual-TLS session to the data endpoint with it and register over the provisioning topics'. A client following exactly that gets a transport-level EOF instead of a session, so the certificate the emulator hands out is unusable and the whole provisioning-by-claim path (including a template deployed through CloudFormation, which this check proves reaches CREATE_COMPLETE) can never be exercised. Worse, the client cannot distinguish 'this certificate was not accepted' from 'the gateway cannot speak TLS', because an unexpected EOF is what a crashed or half-closed listener looks like.
Fix: the iot data-plane device gateway's TLS listener and its client-certificate authentication path (the listener reached at the address the DescribeEndpoint handler publishes) must complete a mutual-TLS handshake for certificate material the account holds, including material minted by CreateProvisioningClaim, and must surface a non-accepted certificate as a handshake-level certificate rejection instead of closing the connection mid-handshake; plus the CreateProvisioningClaim handler's minting, so the pair it returns is registered as a principal that listener will accept (while staying out of the certificate registry reads, as AWS does).
Traces: iot.deploy-provisioning-template-cfn-issues-claim-certificates.

## Evidence
Suite: 261/351 passed. Exactly one failing check is marked neither ALREADY FILED nor OWNED, under the report heading `### iot`:

[error] iot.deploy-provisioning-template-cfn-issues-claim-certificates
  Expected: after the CloudFormation stack carrying AWS::IoT::ProvisioningTemplate reaches CREATE_COMPLETE and DescribeProvisioningTemplate reports enabled=true, CreateProvisioningClaim(templateName=<that template>) returns a PEM-shaped certificate, private key and public key with a 64-character certificateId, and a TLS connection to DescribeEndpoint(endpointType='iot:Data-ATS').endpointAddress on the device-gateway port 8883 — loading that certificate/key pair as the client certificate chain — completes with a negotiated TLS version (tls_sock.version() is not None). This handshake is the check's only destination-side proof that the claim certificate is genuinely ACTIVE, because a claim certificate has no registry entry to read back.
  Actual (error detail tail, verbatim): 
    File "ssl.py", line 1054, in _create
      self.do_handshake()
    File "ssl.py", line 1332, in do_handshake
      self._sslobj.do_handshake()
    ssl.SSLEOFError: [SSL: UNEXPECTED_EOF_WHILE_READING] EOF occurred in violation of protocol (_ssl.c:1011)
  The preceding assertions all passed (stack CREATE_COMPLETE, enabled=True, PEM header/footer present, 'PRIVATE KEY-----' and 'PUBLIC KEY-----' present, len(certificateId) == 64), so the divergence is isolated to the mutual-TLS handshake with the minted claim certificate. socket.create_connection() to the published address succeeded, so something is listening on 8883 and the torn-down negotiation is the emulator's own.
  Contrast within the same run: ordinary certificate material does reach MQTT on that listener (e.g. iot.mqtt-topic-scoped-policy-confines-publish-to-its-resource-arn fails on an unauthorized PUBACK, i.e. after a completed handshake and CONNACK), which is why this is scoped to claim material rather than to the listener as a whole.
  Per-run values that are NOT requirements: the resolved endpoint host and the generated template/role/stack/thing names and certificateId.

## Steps to Reproduce
1. Boot the emulator and run the behavioral suite (run_behavioral_suite); iot.deploy-provisioning-template-cfn-issues-claim-certificates errors as shown.
2. Manually:
   a. iam CreateRole with a trust policy for Service iot.amazonaws.com, then AttachRolePolicy arn:aws:iam::aws:policy/service-role/AWSIoTThingsRegistration.
   b. cloudformation CreateStack with a template whose single resource is AWS::IoT::ProvisioningTemplate { TemplateName, ProvisioningRoleArn=<that role>, TemplateBody=<a device template creating AWS::IoT::Certificate (CertificateId: {Ref: 'AWS::IoT::Certificate::Id'}, Status: Active), AWS::IoT::Policy and AWS::IoT::Thing>, Enabled: true }; wait for CREATE_COMPLETE.
   c. iot DescribeProvisioningTemplate(templateName=<name>) — enabled is true.
   d. iot CreateProvisioningClaim(templateName=<name>) — note certificatePem, keyPair.PrivateKey, keyPair.PublicKey, certificateId.
   e. iot DescribeEndpoint(endpointType='iot:Data-ATS') — take endpointAddress.
   f. Open a TLS client connection to endpointAddress:8883 loading the returned certificatePem/PrivateKey via load_cert_chain (no SigV4, check_hostname off, verify_mode CERT_NONE).
3. Observe: the TCP connect succeeds and the handshake then fails with '[SSL: UNEXPECTED_EOF_WHILE_READING] EOF occurred in violation of protocol' instead of completing and reporting a negotiated TLS version.
4. Re-replay iot.deploy-provisioning-template-cfn-issues-claim-certificates to confirm a fix.
