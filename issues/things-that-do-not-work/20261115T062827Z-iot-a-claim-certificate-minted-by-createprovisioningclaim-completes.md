Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.create-provisioning-claim-missing-template-rejected
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.create-provisioning-claim-missing-template-rejected|_assert_mtls_handshake_succeeds#31|AssertionError|raise AssertionError(
Issue-Commit: 22ca9934d3e7d714a2a3064609b658283f5e8796

# iot: a claim certificate minted by CreateProvisioningClaim completes no mutual-TLS handshake against the published data endpoint — the gateway aborts the handshake with a TLS illegal_parameter alert

> _Note: this issue was identified against the code base at commit `22ca9934d3e7d714a2a3064609b658283f5e8796`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateProvisioningClaim

## Description
One net-new root cause in the iot service's fleet-provisioning data path. It is NOT covered by the open issues about endpoint discovery and HTTPS publish routing — 20261110T222008Z-iot-the-published-data-plane-address-serves-no.md (mTLS POST /topics/{topic} answered 404 UnknownService, and endpointAddress shape), 20261109T120740Z-iot-the-published-data-plane-address-is-a.md and 20261111T093215Z-iot-3-net-new-causes-the-published-endpoint.md — none of which carries this check's replay key and none of which describes the certificate-authenticated TLS handshake on the device-gateway port (8883) that this cause is about: here a connection IS accepted at the TCP level and the emulator then aborts the TLS negotiation itself.

Cause 1 of 1: the X.509 claim certificate CreateProvisioningClaim mints cannot authenticate a mutual-TLS session against the address DescribeEndpoint publishes; the handshake is aborted by the peer with a TLS illegal_parameter alert.
AWS: Smithy com.amazonaws.iot#CreateProvisioningClaim — POST /provisioning-templates/{templateName}/provisioning-claim, success 200, output {certificateId (@length 64/64, ^(0x)?[a-fA-F0-9]+$), certificatePem, keyPair{PublicKey,PrivateKey}, expiration}; modeled errors InvalidRequestException(400), ResourceNotFoundException(404), ThrottlingException(400), UnauthorizedException(401), InternalFailureException(500), ServiceUnavailableException(503). The model does not describe the gateway transport, so for the handshake itself: recorded behavior only — against real AWS the minted claim certificate (certificatePem plus keyPair.PrivateKey) is a certificate the account's device gateway recognizes, so a TLS client-certificate handshake to the data endpoint returned by DescribeEndpoint(endpointType='iot:Data-ATS') on the MQTT port completes and negotiates a cipher; the recording shows the handshake succeeding and only an unregistered/bogus certificate being rejected during the handshake. The refusal half (an unknown certificate) is also a handshake-level rejection, never a silent accept. The first half of the same recording — CreateProvisioningClaim for a nonexistent template answering ResourceNotFoundException with message "Unable to find template named <templateName>" (fixed text "Unable to find template named " plus the templateName taken from the request) and minting no certificate — is already matched by the emulator and is not part of this defect.
Emulator: the claim call succeeds and returns a PEM and private key, but a TLS session to the published endpoint on the device-gateway port, presenting that certificate as the client certificate, never completes: the peer aborts with "[SSL: SSLV3_ALERT_ILLEGAL_PARAMETER] ssl/tls alert illegal parameter". The TCP connection is accepted (so something is listening), and the failure is the emulator's own TLS negotiation — it never accepts the minted certificate as a principal, and the handshake is torn down before any MQTT framing. The target address is the value the emulator itself published from DescribeEndpoint, so this is not a runner/network problem: the host and port in the diagnostic are per-run values and are not part of the requirement.
Why: fleet provisioning is defined as "get a claim certificate, then connect to the data endpoint with it and register over the provisioning MQTT topics". A device following exactly that gets a TLS alert instead of a session, so the certificate CreateProvisioningClaim hands out is unusable: no provisioning-by-claim flow can ever be exercised, and a client cannot tell "certificate not accepted" (which AWS signals for an unknown certificate) from "the gateway cannot negotiate TLS at all".
Fix: the iot data-plane device gateway's TLS listener and its client-certificate authentication — the listener reached at the address DescribeEndpoint publishes must complete a mutual-TLS handshake for a certificate the account holds (including one minted by CreateProvisioningClaim), and must reject only certificates that are not registered; plus the CreateProvisioningClaim handler's minting, so the certificate/key pair it returns is registered as a usable principal for that listener rather than an unreferenced PEM.
Traces: iot.create-provisioning-claim-missing-template-rejected.

## Evidence
Check iot.create-provisioning-claim-missing-template-rejected [mismatch]:
  Expected: after CreateProvisioningClaim(templateName=<existing enabled template>) returns certificatePem + keyPair.PrivateKey, a TLS connection to DescribeEndpoint(iot:Data-ATS).endpointAddress on the device-gateway port, loading that pair as the client certificate chain, completes and negotiates a cipher (the only independent read of a fresh claim certificate, since it is not visible through DescribeCertificate/ListCertificates).
  Actual: AssertionError: "mTLS handshake using the minted claim certificate failed against <endpoint>:8883 -- the certificate was not accepted: [SSL: SSLV3_ALERT_ILLEGAL_PARAMETER] ssl/tls alert illegal parameter (_ssl.c:1011)". The TCP connect succeeded; the peer aborted the TLS negotiation with an illegal_parameter alert.
  Fixed part of the divergence: a certificate-authenticated TLS session to the published data endpoint is aborted instead of completing. Per-run values that are NOT requirements: the host/port the published endpoint resolved to in this run, the generated template/policy/role names, the minted certificateId.
  Note: the earlier assertions of the same check passed — CreateProvisioningClaim on a missing template returned ('ResourceNotFoundException', 'Unable to find template named <name>') and added no certificate — so the divergence is isolated to the handshake with the minted claim certificate.

## Steps to Reproduce
1. Boot the emulator.
2. Create an IoT policy allowing iot:Connect, an IAM role trusted by iot.amazonaws.com, and a provisioning template referencing them: iot CreateProvisioningTemplate(templateName=<t>, templateBody=<minimal body creating AWS::IoT::Thing + AWS::IoT::Certificate + AWS::IoT::Policy>, provisioningRoleArn=<role arn>, enabled=true).
3. Call iot CreateProvisioningClaim(templateName=<t>); note it returns certificateId, certificatePem and keyPair.PrivateKey.
4. Call iot DescribeEndpoint(endpointType='iot:Data-ATS') and take endpointAddress.
5. Open a TLS connection to that address on the device-gateway port (8883), loading the returned certificatePem/PrivateKey as the client certificate chain (no SigV4).
6. Observe the handshake aborted with '[SSL: SSLV3_ALERT_ILLEGAL_PARAMETER] ssl/tls alert illegal parameter' instead of completing with a negotiated cipher.
7. Replay iot.create-provisioning-claim-missing-template-rejected.
