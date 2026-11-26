Issue-Severity: LOW
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.create-certificate-from-csr-undersized-rsa-key-rejected
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.create-certificate-from-csr-undersized-rsa-key-rejected|run#9|AssertionError|assert refusal == _REFUSAL, f"expected {_REFUSAL!r} for a 1024-bit RSA CSR, got {refusal!r}"
Issue-Commit: 5af22e124f1b4edcbe644b4b43fbf6a6331dae9c

# iot CreateCertificateFromCsr refuses an undersized-key CSR with emulator-authored prose instead of AWS's "CSR violates constraints"

> _Note: this issue was identified against the code base at commit `5af22e124f1b4edcbe644b4b43fbf6a6331dae9c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCertificateFromCsr

## Description
One net-new root cause in the iot control plane.

Cause 1 of 1: CreateCertificateFromCsr's InvalidRequestException message for a well-formed PKCS#10 CSR whose public key is below the documented key-strength floor (an RSA key under 2048 bits, or a key on a curve other than NIST P-256/P-384/P-521) is emulator-authored prose that recites the rule, not the sentence AWS sends for that refusal class.

AWS: Smithy: CreateCertificateFromCsr (POST /certificates, success 200) takes the required member certificateSigningRequest (target CertificateSigningRequest, @length min 1 max 4096, pattern ^[\s\S]*$) and models InvalidRequestException (errorKind client, httpError 400, doc "The request is not valid."). The operation's documentation states the CSR's key must be an RSA key of at least 2048 bits or an ECC key on NIST P-256, P-384 or P-521, but the model does not state the refusal text. Recorded behavior only: for a CSR that parses but violates that key-strength constraint, AWS answers HTTP 400, error code InvalidRequestException, with the message exactly "CSR violates constraints". The entire message is FIXED text for this error class — AWS echoes no variable part into it: not the member name, not the submitted key type, not the observed key size, and not the 2048-bit limit or the permitted curve names. The status (400) and the code (InvalidRequestException) already match; only the message diverges.

Emulator: answers 400 InvalidRequestException with the message "Invalid certificate signing request: the public key must be an RSA key of at least 2048 bits or an ECC key from the NIST P-256, NIST P-384 or NIST P-521 curves" — text AWS never sends, which additionally names the input member and spells out the constraint and its limits. (The positive control in the same check passes: a 2048-bit RSA CSR mints a certificate, so this is a message-only divergence on the refusal branch, not a broken operation.)

Why: a client, provisioning tool or operator that matches, asserts on, localizes or logs AWS's refusal sentence to distinguish "key too weak" from other 400s sees a sentence AWS never emits, so the constraint-violation branch is unrecognized and surfaces as an unclassified 400; a device-provisioning pipeline validated against AWS takes a different path against the emulator even though status and code line up.

Fix: the CreateCertificateFromCsr handler's input validation — the refusal text it propagates from the CSR reader (crate::state::read_certificate_signing_request) for the key-strength branch must be AWS's fixed sentence for that error class instead of a description of the rule that was broken. This is a DIFFERENT branch from the already-open issue 20261125T013203Z-iot-createcertificatefromcsr-rejects-an-unreadable-csr-with-emulator.md, which covers the unparseable-CSR branch (AWS "Invalid CSR format"); that issue does not cover the key-strength refusal, whose AWS sentence is "CSR violates constraints".

Traces: behavioral check iot.create-certificate-from-csr-undersized-rsa-key-rejected. Component: services/service-com-amazonaws-iot-awsiotservice/src/ops/create_certificate_from_csr.rs lines 198-209 (the map_err that lifts refusal.message() into InvalidRequestException) and the key-strength refusal produced by read_certificate_signing_request in services/service-com-amazonaws-iot-awsiotservice/src/state.rs.

## Evidence
Failing check iot.create-certificate-from-csr-undersized-rsa-key-rejected [mismatch].
Step 1 (positive control): CreateCertificateFromCsr with a 2048-bit RSA CSR -> 200 with a certificateId (passes).
Step 2: CreateCertificateFromCsr with a 1024-bit RSA CSR.
  expected (Error.Code, Error.Message) == ('InvalidRequestException', 'CSR violates constraints')
  got      ('InvalidRequestException', 'Invalid certificate signing request: the public key must be an RSA key of at least 2048 bits or an ECC key from the NIST P-256, NIST P-384 or NIST P-521 curves')
AssertionError: expected ('InvalidRequestException', 'CSR violates constraints') for a 1024-bit RSA CSR, got ('InvalidRequestException', 'Invalid certificate signing request: ...').
HTTP status (400) and error code match; $.message is the only diverging field. AWS's message is 100% fixed text for this class — no request- or state-derived values.

## Steps to Reproduce
1. Generate a PEM PKCS#10 certificate signing request signed by a fresh 1024-bit RSA key (any subject CN).
2. Against the emulator's iot endpoint call CreateCertificateFromCsr (POST /certificates) with that PEM as certificateSigningRequest, e.g. `aws iot create-certificate-from-csr --certificate-signing-request file://weak.csr`.
3. Observe HTTP 400, x-amzn-errortype InvalidRequestException, message 'Invalid certificate signing request: the public key must be an RSA key of at least 2048 bits or an ECC key from the NIST P-256, NIST P-384 or NIST P-521 curves'.
4. AWS answers HTTP 400 InvalidRequestException with the message exactly 'CSR violates constraints'.
5. Confirm the same request shape at 2048 bits still returns 200 with a certificateId, then re-replay behavioral check iot.create-certificate-from-csr-undersized-rsa-key-rejected to confirm the fix.
