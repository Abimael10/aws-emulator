Issue-Source: trace-sweep-divergence
Issue-Trace: iot-iot-create-certificate-from-csr
Issue-Step: task#3
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-iot-create-certificate-from-csr
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|CreateCertificateFromCsr|4xx->4xx|$.message
Issue-Commit: 99fd1ab9e434da983b1df489b3856c4e5082b013

# iot CreateCertificateFromCsr rejects an unreadable CSR with emulator-authored wording instead of AWS's "Invalid CSR format"

> _Note: this issue was identified against the code base at commit `99fd1ab9e434da983b1df489b3856c4e5082b013`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCertificateFromCsr

## Description
One net-new root cause in the iot control plane.

Cause 1 of 1 — CreateCertificateFromCsr's InvalidRequestException message for a certificateSigningRequest that is not a readable PKCS#10 certification request is emulator-authored prose, not the sentence AWS sends.

AWS: Smithy: CreateCertificateFromCsr (POST /certificates, success 200) takes the required member certificateSigningRequest (target CertificateSigningRequest, @length min 1 max 4096, pattern ^[\s\S]*$) and models InvalidRequestException (errorKind client, httpError 400, doc "The request is not valid."). The operation's own documentation states the CSR must carry an RSA key of at least 2048 bits or an ECC key on NIST P-256/P-384/P-521, but the model does not state the refusal text. Recorded behavior only: when the supplied certificateSigningRequest cannot be parsed as a PEM-encoded PKCS#10 certification request, AWS answers HTTP 400 with error code InvalidRequestException and the message exactly "Invalid CSR format". The whole message is FIXED text — there are no variable parts: AWS does not echo the member name, the submitted bytes, the key type or any length/limit into it. HTTP status (400), error code (InvalidRequestException) and the error type header all already match; only the message body diverges.

Emulator: answers the right status (400) and the right code (InvalidRequestException) but with the message "Invalid certificate signing request: the certificateSigningRequest field is not a PEM-encoded PKCS#10 certification request" — emulator-authored text that additionally names the input member and describes the parse rule, neither of which appears in AWS's sentence.

Why: a client or operator that matches, asserts on, localizes or logs AWS's refusal sentence for a malformed CSR sees text AWS never sends, so the malformed-CSR branch is not recognized and surfaces as an unclassified 400. The error message is part of the wire contract for this error class, so a CSR-provisioning tool validated against AWS behaves differently against the emulator even though the status and code line up.

Fix: the CreateCertificateFromCsr handler's input validation — specifically the refusal message it propagates from the CSR reader (crate::state::read_certificate_signing_request's refusal) must be AWS's fixed sentence for this error class rather than a description of the parse failure.

Traces: iot-iot-create-certificate-from-csr#task#3. Component: services/service-com-amazonaws-iot-awsiotservice/src/ops/create_certificate_from_csr.rs lines 198-209 (the map_err that lifts the CSR reader's refusal.message() into InvalidRequestException) and the refusal text produced by read_certificate_signing_request in services/service-com-amazonaws-iot-awsiotservice/src/state.rs. (No open issue covers this operation: the closest open iot wording issues — 20261117T200355Z-iot-5-net-new-causes-updatecertificate-s-two and 20261120T170349Z-iot-resource-state-divergences-deleted-scheduled-audit-stays — cover UpdateCertificate/UpdateThing/DeletePolicy/ListThings and the certificate/audit state plane, not CreateCertificateFromCsr.)

## Evidence
Fixture iot-iot-create-certificate-from-csr, step task#3, CreateCertificateFromCsr with an unparseable certificateSigningRequest.
Status: expected 400, got 400 (match). Error code / x-amzn-errortype: InvalidRequestException (match).
$.message: expected 'Invalid CSR format' (entirely fixed text, no request- or state-derived values), got 'Invalid certificate signing request: the certificateSigningRequest field is not a PEM-encoded PKCS#10 certification request'.
Replay verdict: mismatch, divergence_key service=iot|CreateCertificateFromCsr|status=400->400|keys=$.message; affected_count 1.

## Steps to Reproduce
1. Against the emulator's iot endpoint, call CreateCertificateFromCsr (POST /certificates) with a certificateSigningRequest whose bytes are not a PEM-encoded PKCS#10 certification request (e.g. aws iot create-certificate-from-csr --certificate-signing-request 'not-a-csr').
2. Observe HTTP 400 with x-amzn-errortype InvalidRequestException and message 'Invalid certificate signing request: the certificateSigningRequest field is not a PEM-encoded PKCS#10 certification request'.
3. AWS answers HTTP 400 InvalidRequestException with the message exactly 'Invalid CSR format'.
4. Re-replay fixture iot-iot-create-certificate-from-csr step task#3 to confirm the fix.
