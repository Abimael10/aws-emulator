Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.delete-package-with-existing-versions-rejected iot.deploy-provisioning-template-cfn-enables-claim iot.deploy-provisioning-template-cfn-issues-claim-certificates
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.delete-package-with-existing-versions-rejected|_error_code#21|AssertionError|raise AssertionError(f"expected a rejection, but the call SUCCEEDED and returned {result!r}")
Issue-Key: iot.deploy-provisioning-template-cfn-enables-claim|run#20|AssertionError|assert 0 < ttl_seconds < 30 * 60, f"unexpected claim TTL: {ttl_seconds:.0f}s"
Issue-Key: iot.deploy-provisioning-template-cfn-issues-claim-certificates|_assert_claim_cert_authenticates#31|SSLError|with tls_context.wrap_socket(sock, server_hostname=endpoint_host) as tls_sock:
Issue-Commit: 4fb279e8268ff71ec62c1d9ac755c72465a1585e

# iot: 3 net-new causes — DeletePackageVersion on a deleted package succeeds instead of refusing, CreateProvisioningClaim mints a 1-hour claim on the CFN-deployed template path, and the minted claim certificate completes no mutual-TLS handshake at the published data endpoint

> _Note: this issue was identified against the code base at commit `4fb279e8268ff71ec62c1d9ac755c72465a1585e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeletePackageVersion

## Description
Three independent root causes under the suite heading '### iot' (147/257 passed). Each lives in a different handler/component and closes independently. Causes are numbered; a fixer must close all three before submitting.

Cause 1 of 3: DeletePackageVersion against a package that no longer exists SUCCEEDS instead of being refused.
  Failing check: iot.delete-package-with-existing-versions-rejected.
  AWS: Smithy com.amazonaws.iot#DeletePackageVersion -> DELETE /packages/{packageName}/versions/{versionName}, success code 200 with an empty output; input packageName (@required, httpLabel, @length 1..128, pattern ^[a-zA-Z0-9-_.]+$), versionName (@required, httpLabel, @length 1..64), clientToken (httpQuery, idempotency token); modeled errors ValidationException (httpError 400), ThrottlingException (400), InternalServerException (500) — note there is NO ResourceNotFoundException modeled on this operation. Recorded behavior: once the owning package has been deleted, a DeletePackageVersion naming it is refused with HTTP 400, error code ValidationException and the message "Package with packageName: <packageName> not found" — fixed text "Package with packageName: " and " not found", with <packageName> taken from the request. This is distinct from the read path, which AWS answers ResourceNotFoundException "Package <packageName> not found" for GetPackage; a mutation against a vanished package is a validation failure, not a not-found.
  Emulator: the call SUCCEEDS — HTTP 200 with an empty JSON body ({'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/json', 'content-length': '2', ...}}}). No error is raised at all.
  Why: a client cannot tell a real version delete from a delete against a package that is already gone, so idempotency and reconciliation logic silently believes it removed a version that never existed and never surfaces the dangling reference. The emulator is strictly more permissive than AWS on a mutation, which is the class of divergence that hides bugs rather than exposing them.
  Fix: the DeletePackageVersion handler's validation/state lookup in the iot crate's package operations (services/service-com-amazonaws-iot-awsiotservice/src/ops/delete_package_version.rs) — resolve the owning package first and, when the account holds no package under that packageName, return the modeled ValidationException with AWS's wording before touching version state.
  Traces: iot.delete-package-with-existing-versions-rejected. Everything before this leg PASSED in the same check: DeletePackage while a version still existed was correctly refused ValidationException "Unable to delete package '<packageName>' as there are package versions associated with it", GetPackage still returned the intact package, deleting the version then the package succeeded, and GetPackage on the deleted package answered ResourceNotFoundException "Package <packageName> not found". The divergence is isolated to the final mutation-against-a-dead-package leg.

Cause 2 of 3: CreateProvisioningClaim mints a claim certificate that expires one hour after the call instead of the short-lived window AWS issues.
  Failing check: iot.deploy-provisioning-template-cfn-enables-claim.
  AWS: Smithy com.amazonaws.iot#CreateProvisioningClaim -> POST /provisioning-templates/{templateName}/provisioning-claim, success 200, output {certificateId (@length 64/64, pattern ^(0x)?[a-fA-F0-9]+$), certificatePem (@length 1..65536), keyPair{PublicKey,PrivateKey}, expiration (timestamp)}; modeled errors InvalidRequestException(400), ResourceNotFoundException(404), ThrottlingException(400), UnauthorizedException(401), InternalFailureException(500), ServiceUnavailableException(503). The model states no validity window, so for the lifetime: recorded behavior only — a provisioning claim is a deliberately short-lived bearer credential and the recorded expiration is less than 30 minutes after the call (AWS documents a five-minute provisioning-claim lifetime). The requirement is that bound, not any one recorded instant.
  Emulator: expiration is exactly 3600 s (one hour) after the call, for a provisioning template deployed through CloudFormation as well as one created directly.
  Why: a device driving fleet provisioning treats the returned expiration as the window in which it must finish the handshake; it is told a secret is valid twelve times longer than AWS permits, and any client asserting the credential is short-lived passes against the emulator and fails against AWS.
  Fix: the CreateProvisioningClaim handler's validity constant (PROVISIONING_CLAIM_VALIDITY in the iot crate's state module, as used by services/service-com-amazonaws-iot-awsiotservice/src/ops/create_provisioning_claim.rs) — set it to AWS's provisioning-claim lifetime so both the minted certificate's notAfter and the reported expiration fall under 30 minutes.
  Traces: iot.deploy-provisioning-template-cfn-enables-claim. Earlier legs of the same check PASSED: the CFN-deployed AWS::IoT::ProvisioningTemplate was described enabled=true with a ':role/' provisioningRoleArn, and the claim's certificateId matched ^(0x)?[a-fA-F0-9]{64}$ with a correctly framed PEM and private key.
  Related open issue: 20261114T165150Z-iot-4-net-new-causes-createdomainconfiguration-accepts-a.md describes this same 3600 s validity as its Cause 3, but carries only the replay key iot.e2e-fleet-provisioning-claim-to-thing. This check is NOT in its trace_ids and is not owned by it, so it is filed here to carry its replay key; closing that issue's Cause 3 should close this cause too.

Cause 3 of 3: the claim certificate CreateProvisioningClaim mints cannot authenticate a mutual-TLS session against the address DescribeEndpoint publishes — the emulator aborts the handshake with a TLS illegal_parameter alert.
  Failing check: iot.deploy-provisioning-template-cfn-issues-claim-certificates.
  AWS: recorded behavior only — the Smithy model describes CreateProvisioningClaim's output (see Cause 2) but not the device-gateway transport. Against real AWS the minted certificatePem + keyPair.PrivateKey are a principal the account's device gateway recognizes, so a TLS client-certificate connection to DescribeEndpoint(endpointType='iot:Data-ATS').endpointAddress on the MQTT port completes and negotiates a cipher; only an unregistered or inactive certificate is rejected, and that rejection too is a handshake-level refusal, never a silent accept. This is the only destination-side read of a fresh claim certificate, because a claim certificate deliberately has no registry row (it is invisible to DescribeCertificate/ListCertificates).
  Emulator: the TCP connection to the published address is accepted, then the peer aborts the TLS negotiation itself: ssl.SSLError: [SSL: SSLV3_ALERT_ILLEGAL_PARAMETER] ssl/tls alert illegal parameter. The target address is the value the emulator published from its own DescribeEndpoint, so this is the emulator's listener and not a runner network failure; the host and port are per-run values and are not part of the requirement.
  Why: fleet provisioning is defined as "take a claim certificate, then connect to the data endpoint with it and register over the provisioning topics". A device following exactly that gets a TLS alert instead of a session, so the certificate the API hands out is unusable and no provisioning-by-claim flow can be exercised; a client also cannot distinguish "certificate not accepted" from "the gateway cannot negotiate TLS at all".
  Fix: the iot data-plane device gateway's TLS listener and its client-certificate authentication (the listener reached at the address DescribeEndpoint publishes), plus the CreateProvisioningClaim handler's minting so the pair it returns is registered as a usable principal for that listener rather than an unreferenced PEM.
  Traces: iot.deploy-provisioning-template-cfn-issues-claim-certificates. Earlier legs PASSED: the IAM role and the CFN stack declaring AWS::IoT::ProvisioningTemplate reached CREATE_COMPLETE, DescribeProvisioningTemplate reported enabled=true, and the claim's certificateId length, certificate PEM header/footer and private/public key PEM framing were all correct.
  Related open issue: 20261115T062827Z-iot-a-claim-certificate-minted-by-createprovisioningclaim-completes.md describes the same handshake abort but carries only the replay key iot.create-provisioning-claim-missing-template-rejected. This check is NOT in its trace_ids and is not owned by it, so it is filed here to carry its replay key; closing that issue should close this cause too.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout. Total 200/351 passed (57.0%); bootstrap_signal=false. Heading '### iot': 147/257 (57%), 110 failed — the three checks below are the only ones under that heading not already marked ALREADY FILED (blocked by an unimplemented scaffold stub) or OWNED by an open issue.

Cause 1 — [mismatch] iot.delete-package-with-existing-versions-rejected:
  File "check_iot_delete_package_with_existing_versions_rejected.py", line 61, in run -> dead_mutation_code, dead_mutation_message = _error_code(lambda: iot.delete_package_version(packageName=<pkg>, versionName='v1'))
  File "check_iot_delete_package_with_existing_versions_rejected.py", line 87, in _error_code
  AssertionError: expected a rejection, but the call SUCCEEDED and returned {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/json', 'content-length': '2', 'date': '...'}, 'RetryAttempts': 0}}
  Expected: HTTP 400, Error.Code 'ValidationException', Error.Message "Package with packageName: <packageName> not found" (fixed text around the packageName from the request).
  Actual: HTTP 200, empty JSON body, no error.
  Passed in the same check (so NOT part of this cause): DeletePackage with a live version -> ValidationException "Unable to delete package '<packageName>' as there are package versions associated with it"; GetPackage still intact; DeletePackageVersion then DeletePackage succeeding; GetPackage on the deleted package -> ResourceNotFoundException "Package <packageName> not found".

Cause 2 — [mismatch] iot.deploy-provisioning-template-cfn-enables-claim:
  File "check_iot_deploy_provisioning_template_cfn_enables_claim.py", line 83, in run
  assert 0 < ttl_seconds < 30 * 60, f"unexpected claim TTL: {ttl_seconds:.0f}s"
  AssertionError: unexpected claim TTL: 3600s
  ttl_seconds = CreateProvisioningClaim(templateName=<CFN-deployed template>)['expiration'].timestamp() - time.time(). The 3600 s is the emulator's fixed validity, not a per-run instant; the requirement is the bound (well under 30 minutes), not a specific timestamp.
  Passed in the same check: DescribeProvisioningTemplate enabled=True with ':role/' in provisioningRoleArn; certificateId matching ^(0x)?[a-fA-F0-9]{64}$; certificatePem starting '-----BEGIN CERTIFICATE-----'; keyPair.PrivateKey starting '-----BEGIN (RSA )?PRIVATE KEY-----'.

Cause 3 — [error] iot.deploy-provisioning-template-cfn-issues-claim-certificates:
  File "ssl.py", line 1054, in _create / self.do_handshake() / File "ssl.py", line 1332, in do_handshake
  ssl.SSLError: [SSL: SSLV3_ALERT_ILLEGAL_PARAMETER] ssl/tls alert illegal parameter (_ssl.c:1011)
  The failing step: endpoint_host = iot.describe_endpoint(endpointType='iot:Data-ATS')['endpointAddress']; then a TLS client-certificate connection to that address on the device-gateway port (8883) loading the minted certificatePem/PrivateKey as the client chain, asserting tls_sock.version() is not None.
  Expected: the mutual-TLS handshake completes and a protocol version/cipher is negotiated — the only independent proof a fresh claim certificate is genuinely active, since it has no DescribeCertificate row.
  Actual: the TCP connect succeeds, then the emulator aborts the TLS negotiation with an illegal_parameter alert; no MQTT framing is ever reached.
  Passed in the same check: the IAM role + attached AWSIoTThingsRegistration policy, CreateStack/CREATE_COMPLETE for the AWS::IoT::ProvisioningTemplate, DescribeProvisioningTemplate enabled=True, 64-char certificateId, PEM header/footer on the certificate, 'PRIVATE KEY-----' and 'PUBLIC KEY-----' framing on the key pair.

Per-run values that are NOT requirements anywhere above: the generated package/version, template, stack, role and policy names; the minted certificateId and PEM bytes; the host and port the published data endpoint resolved to; timestamps and request ids.

Smithy consulted (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json): DeletePackageVersion DELETE /packages/{packageName}/versions/{versionName}, success 200, empty output, errors ValidationException 400 / ThrottlingException 400 / InternalServerException 500 (no ResourceNotFoundException modeled); DeletePackage DELETE /packages/{packageName} with the same error set; CreateProvisioningClaim POST /provisioning-templates/{templateName}/provisioning-claim, output certificateId (@length 64/64, ^(0x)?[a-fA-F0-9]+$) / certificatePem / keyPair / expiration, no modeled validity window.

## Steps to Reproduce
Boot the emulator built from this checkout and point botocore iot, cloudformation and iam clients at it. Re-replay the three checks in trace_ids to confirm all three causes are closed.

Cause 1 (DeletePackageVersion on a deleted package):
1. Ensure package indexing is active, then iot CreatePackage(packageName=P).
2. iot CreatePackageVersion(packageName=P, versionName='v1').
3. iot DeletePackage(packageName=P) — expect 400 ValidationException "Unable to delete package 'P' as there are package versions associated with it" (this already matches).
4. iot DeletePackageVersion(packageName=P, versionName='v1'), then iot DeletePackage(packageName=P) — both succeed.
5. iot DeletePackageVersion(packageName=P, versionName='v1') again, now that P is gone.
   Expect: HTTP 400, Error.Code ValidationException, Error.Message "Package with packageName: P not found".
   Observe today: HTTP 200 with an empty body and no error.

Cause 2 (claim TTL on the CFN-deployed template path):
1. cloudformation CreateStack with a template declaring AWS::IoT::ProvisioningTemplate (TemplateName=T, ProvisioningRoleArn=<role trusted by iot.amazonaws.com>, TemplateBody=<body creating AWS::IoT::Thing + AWS::IoT::Certificate>, Enabled=true); wait for CREATE_COMPLETE.
2. iot DescribeProvisioningTemplate(templateName=T) — enabled is true.
3. iot CreateProvisioningClaim(templateName=T) and compute expiration - now.
   Expect: a positive TTL well under 30 minutes (AWS's five-minute claim lifetime), with the certificate PEM's notAfter equal to the reported expiration.
   Observe today: exactly 3600 s.

Cause 3 (mutual TLS with the minted claim certificate):
1. With the same CFN-deployed enabled template T, call iot CreateProvisioningClaim(templateName=T) and keep certificatePem and keyPair.PrivateKey.
2. Call iot DescribeEndpoint(endpointType='iot:Data-ATS') and take endpointAddress.
3. Open a TLS connection to that address on the device-gateway port (8883), loading the returned certificatePem/PrivateKey as the client certificate chain (no SigV4), and read the negotiated TLS version.
   Expect: the handshake completes and a TLS version/cipher is negotiated; only an unregistered certificate is refused, and then at the handshake level.
   Observe today: '[SSL: SSLV3_ALERT_ILLEGAL_PARAMETER] ssl/tls alert illegal parameter' after the TCP connect succeeds.
