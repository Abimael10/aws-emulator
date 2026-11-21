Issue-Source: trace-sweep-divergence
Issue-Trace: acm-pca-ca-not-ready-operation-guards
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: acm-pca-ca-not-ready-operation-guards
Issue-Shape-Id: com.amazonaws.acmpca#ACMPrivateCA
Issue-Key: acm-pca|GetCertificate|4xx->4xx|$.__type
Issue-Key: acm-pca|GetCertificate|4xx->4xx|$.message
Issue-Commit: 82319d48a946e26ab7d96dc7ca606472e537bd51

# acm-pca GetCertificate checks the CA's state before the certificate exists, answering InvalidStateException instead of ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `82319d48a946e26ab7d96dc7ca606472e537bd51`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCertificate

## Description
One cause.

Cause 1 of 1: GetCertificate evaluates the issuing CA's state guard before it resolves the requested certificate, so a request for a certificate that does not exist under the CA is rejected as a state error rather than a missing-resource error.

AWS: Smithy (acm-pca) — GetCertificate takes required CertificateAuthorityArn and CertificateArn (both target Arn, @length 5..200, pattern ^arn:[\w+=/,.@-]+:acm-pca:[\w+=/,.@-]*:[0-9]*:[\w+=,.@-]+(/[\w+=,.@-]+)*$) and models the client errors InvalidArnException, InvalidStateException, RequestFailedException, RequestInProgressException and ResourceNotFoundException, all HTTP 400. The recording (recorded behavior only for the precedence itself) shows that when the CertificateArn names no certificate issued by that CA, AWS answers HTTP 400 with __type ResourceNotFoundException and the message "Could not find specified certificate." — the message is entirely fixed text for this error class, with no variable part, and it is returned in preference to the CA-state guard. InvalidStateException is reserved for the case where the certificate reference is resolvable but the CA itself cannot serve issued certificates.

Emulator: for the same step the emulator answers HTTP 400 (status matches) but with __type "InvalidStateException" and the message "The certificate authority <certificate-authority-arn> is not in the correct state to have issued certificates." Its fixed text is "The certificate authority " ... " is not in the correct state to have issued certificates.", with the CA ARN taken verbatim from the request's CertificateAuthorityArn (the arn:aws:acm-pca:...:certificate-authority/<uuid> value in the recording is one run's value, not part of the requirement). The emulator therefore never reaches the certificate lookup: it short-circuits on the CA's status.

Why: a client that distinguishes "this certificate does not exist" from "this CA is not ready" takes the wrong branch. ResourceNotFoundException is a terminal, non-retryable answer meaning the certificate ARN is wrong or the issuance never happened; InvalidStateException tells the caller the CA is in a transient/incorrect state, which drives waiters, CA-activation retry loops and status polling that can never succeed because the certificate is simply absent. Error-code-keyed handling (botocore exception classes ResourceNotFoundException vs InvalidStateException) dispatches to the wrong recovery path, and the emulator's message additionally blames the CA for a request fault.

Fix: the acm-pca GetCertificate handler's guard ordering / validation — resolve the CertificateArn against the CA's issued-certificate state first and raise ResourceNotFoundException with the fixed message "Could not find specified certificate." when no such certificate exists; only apply the CA-state guard (InvalidStateException) once the certificate reference resolves.

Traces: acm-pca-ca-not-ready-operation-guards#task#8 (GetCertificate against a CA that is not in a certificate-serving state, for a certificate ARN that was never issued).

## Evidence
Fixture acm-pca-ca-not-ready-operation-guards, step task#8, operation GetCertificate (verdict: mismatch).
Expected status 400, actual status 400 — the status matches; the error identity does not.
$.__type: expected 'ResourceNotFoundException', got 'InvalidStateException'.
$.message: expected 'Could not find specified certificate.', got 'The certificate authority arn:aws:acm-pca:us-west-2:000000000000:certificate-authority/e83d46a6-1568-4423-95eb-40d27b4bceab is not in the correct state to have issued certificates.'
Fixed vs variable text: the expected message is wholly fixed text ("Could not find specified certificate."). The emulator's message is fixed text "The certificate authority " + <CertificateAuthorityArn from the request> + " is not in the correct state to have issued certificates."; the account id, region and CA uuid above are one run's values and are not requirements.
Divergence key: service=acm-pca|GetCertificate|status=400->400|keys=$.__type,$.message (1 affected step).

## Steps to Reproduce
1. Create an acm-pca certificate authority and leave it in a state that cannot serve issued certificates (e.g. PENDING_CERTIFICATE — do not import a CA certificate), as fixture acm-pca-ca-not-ready-operation-guards does.
2. Call acm-pca GetCertificate with CertificateAuthorityArn = that CA's ARN and CertificateArn = an ARN under that CA for a certificate that was never issued.
3. Observe the response: AWS/the fixture expects HTTP 400 with __type ResourceNotFoundException and message "Could not find specified certificate."; the emulator returns HTTP 400 with __type InvalidStateException and "The certificate authority <ca arn> is not in the correct state to have issued certificates."
4. Re-replay fixture acm-pca-ca-not-ready-operation-guards step task#8 to confirm the fix.
