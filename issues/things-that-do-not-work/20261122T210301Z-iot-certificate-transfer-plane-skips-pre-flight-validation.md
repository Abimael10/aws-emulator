Issue-Source: trace-sweep-divergence
Issue-Trace: iot-certificate-transfer-lifecycle
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-certificate-transfer-lifecycle
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|TransferCertificate|4xx->2xx|$.message
Issue-Key: iot|TransferCertificate|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: iot|TransferCertificate|4xx->2xx|status
Issue-Key: iot|CancelCertificateTransfer|4xx->2xx|$
Issue-Key: iot|CancelCertificateTransfer|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: iot|CancelCertificateTransfer|4xx->2xx|status
Issue-Commit: 88ec02c2d9f7d3a83855b459fddcd06276ce6950

# iot certificate-transfer plane skips pre-flight validation: self-transfer and cancel-transfer on a nonexistent certificate both answer 200

> _Note: this issue was identified against the code base at commit `88ec02c2d9f7d3a83855b459fddcd06276ce6950`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TransferCertificate

## Description
The emulator's IoT certificate-transfer operations perform no request/resource pre-flight: both of the modeled refusal paths below are swallowed and answered as successes. There are 2 causes in this issue; both must be closed.

=== Cause 1 of 2: TransferCertificate accepts a self-transfer instead of refusing it (fixture iot-certificate-transfer-lifecycle, step task#2) ===
AWS: Smithy (com-amazonaws-iot-awsiotservice) TransferCertificate: PATCH /transfer-certificate/{certificateId}, required input members certificateId (@httpLabel, @length 64..64, @pattern ^(0x)?[a-fA-F0-9]+$) and targetAwsAccount (@httpQuery targetAwsAccount, @length 12..12, @pattern ^[0-9]+$); modeled errors include InvalidRequestException (httpError 400). AWS rejects a transfer whose targetAwsAccount is the caller's own account — a certificate cannot be transferred to the account that already owns it — with HTTP 400, x-amzn-errortype 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/' and the message 'Cannot perform a self-transfer'. That message is entirely fixed text: it interpolates no request or state value, so it is reproducible verbatim.
Emulator: answers HTTP 200 with a TransferCertificateResponse body and no error. Replay reports 'status: expected 400, got 200; $.message: missing (expected "Cannot perform a self-transfer", got None); $headers.x-amzn-errortype: missing (expected "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/", got None)'.
Why: a client that transfers a certificate to the wrong (its own) account gets a success it must not get. Worse, the certificate is reported as transferred, so the client believes ownership moved and the certificate's status changed, while against AWS the call would have been rejected and the certificate left untouched. Error-path tests that assert InvalidRequestException on self-transfer pass vacuously, and ownership-handoff logic built on this call is never exercised.
Fix: TransferCertificate's input validation / handler — add the pre-flight comparison of targetAwsAccount against the owning account before any state mutation, and emit InvalidRequestException (400) with the fixed message above.
Traces: iot-certificate-transfer-lifecycle#task#2.

=== Cause 2 of 2: CancelCertificateTransfer on a certificate that does not exist answers 200 with an empty body instead of ResourceNotFoundException (fixture iot-certificate-transfer-lifecycle, step task#3) ===
AWS: Smithy CancelCertificateTransfer: PATCH /cancel-certificate-transfer/{certificateId}, required certificateId (@httpLabel, @length 64..64, @pattern ^(0x)?[a-fA-F0-9]+$), output Unit; modeled errors include ResourceNotFoundException (httpError 404) and TransferAlreadyCompletedException (410). The operation documentation states it 'Cancels a pending transfer for the specified certificate', so an identifier that names no certificate in the account is resolved and refused before anything else: HTTP 404, x-amzn-errortype 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/', message of the form 'CertificateId <certificateId> does not exist' — the fixed text is 'CertificateId ' and ' does not exist', and the single variable part is the certificateId taken verbatim from the request path label (the recording's 64-hex value is only one instance of that template, not part of it).
Emulator: answers HTTP 200 with an empty body and no error header. Replay reports 'status: expected 404, got 200; $: empty_body (expected {"message": "CertificateId ... does not exist"}, got {}); $headers.x-amzn-errortype: missing (expected "ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/", got None)'.
Why: the operation is silently a no-op for any unknown certificate id, so a client cancelling a transfer it believes is pending — including one cancelling with a typo'd, stale, or already-deleted certificate id — is told the cancellation succeeded. Because the modeled success output is Unit (an empty body), a 200 is indistinguishable from a real cancellation, so the caller has no way to detect the mistake and will proceed as though the certificate returned to its account in the INACTIVE state. It also masks the 404/410 distinction AWS draws between 'no such certificate' and 'transfer already completed'.
Why these are one issue: both are the same missing pre-flight stage in the certificate-transfer handlers — the request/resource is never resolved or checked before the operation reports success — and both surface as a 200 standing in for a modeled 4xx refusal.
Fix: CancelCertificateTransfer's handler and state lookup — resolve certificateId against the account's certificate state first and emit ResourceNotFoundException (404) when it resolves to nothing, before returning the empty success body.
Traces: iot-certificate-transfer-lifecycle#task#3.

No open issue owns either cause: the replay report lists owned_by as empty for both clusters. The nearest open iot issues cover different operations and planes — 20261114T091403Z-iot-dependent-resource-pre-flight-is-missing-on.md (certificate providers, streams, dynamic thing groups), 20261118T071347Z-iot-ca-certificate-plane-diverges-updatecacertificate-never-applies.md (CA certificates), 20261117T200355Z-iot-5-net-new-causes-updatecertificate-s-two.md (UpdateCertificate state guards) and 20261120T170349Z-iot-resource-state-divergences-deleted-scheduled-audit-stays.md (DeleteCertificate 409) — none of them touches TransferCertificate or CancelCertificateTransfer.

## Evidence
Cause 1 — TransferCertificate (fixture iot-certificate-transfer-lifecycle, step task#2):
  Expected: HTTP 400, x-amzn-errortype 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/', $.message = 'Cannot perform a self-transfer'
  Actual:   HTTP 200, success body, no $.message, no x-amzn-errortype
  Replay detail: status: expected 400, got 200 | $.message: missing (expected 'Cannot perform a self-transfer', got None) | $headers.x-amzn-errortype: missing (expected 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/', got None)
  divergence_key: service=iot|TransferCertificate|status=400->200|keys=$.message,$headers.x-amzn-errortype,status

Cause 2 — CancelCertificateTransfer (fixture iot-certificate-transfer-lifecycle, step task#3):
  Expected: HTTP 404, x-amzn-errortype 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/', body {'message': 'CertificateId <certificateId> does not exist'}
  Actual:   HTTP 200, empty body {}, no x-amzn-errortype
  Replay detail: status: expected 404, got 200 | $: empty_body (expected {'message': 'CertificateId a8265762f244bc0c0ecd2126a3ea0262d67b7b4195f93b00be951a5faaf7db85 does not exist'}, got {}) | $headers.x-amzn-errortype: missing (expected 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/', got None)
  divergence_key: service=iot|CancelCertificateTransfer|status=404->200|keys=$,$headers.x-amzn-errortype,status

Model evidence (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json):
  TransferCertificate errors: CertificateStateException(406), InvalidRequestException(400), ResourceNotFoundException(404), TransferConflictException(409), UnauthorizedException(401), ThrottlingException(400), ServiceUnavailableException(503), InternalFailureException(500)
  CancelCertificateTransfer errors: InvalidRequestException(400), ResourceNotFoundException(404), TransferAlreadyCompletedException(410), UnauthorizedException(401), ThrottlingException(400), ServiceUnavailableException(503), InternalFailureException(500); output shape Unit (empty success body)

## Steps to Reproduce
Cause 1 (fixture iot-certificate-transfer-lifecycle, step task#2):
1. Create an IoT certificate in the emulator so it is ACTIVE and owned by the calling account.
2. Call TransferCertificate with certificateId = that certificate's 64-hex id and targetAwsAccount = the caller's OWN 12-digit account id (PATCH /transfer-certificate/{certificateId}?targetAwsAccount=<own-account>).
3. Observe HTTP 200 and a TransferCertificateResponse body. Expected: HTTP 400, x-amzn-errortype 'InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/', $.message 'Cannot perform a self-transfer', and the certificate left unchanged.

Cause 2 (fixture iot-certificate-transfer-lifecycle, step task#3):
1. Call CancelCertificateTransfer with a well-formed but nonexistent certificateId (64 hex characters, matching ^(0x)?[a-fA-F0-9]+$, naming no certificate in the account): PATCH /cancel-certificate-transfer/{certificateId}.
2. Observe HTTP 200 with an empty body {} and no error type header. Expected: HTTP 404, x-amzn-errortype 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/', body {'message': 'CertificateId <the certificateId from the request> does not exist'}.

To confirm a fix, re-replay iot-certificate-transfer-lifecycle#task#2 and iot-certificate-transfer-lifecycle#task#3.
