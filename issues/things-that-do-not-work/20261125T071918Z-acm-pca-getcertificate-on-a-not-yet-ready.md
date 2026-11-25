Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm-pca.get-certificate-not-ready-fails
Issue-Shape-Id: com.amazonaws.acmpca#ACMPrivateCA
Issue-Key: acm-pca.get-certificate-not-ready-fails|run#4|AssertionError|assert refusal == (
Issue-Commit: 7c1a32607181dcc252ace77444601da30e81bd5f

# acm-pca GetCertificate on a not-yet-ready CA returns ResourceNotFoundException instead of InvalidStateException

> _Note: this issue was identified against the code base at commit `7c1a32607181dcc252ace77444601da30e81bd5f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCertificate

## Description
Cause 1 of 1: GetCertificate does not gate on the certificate authority's state before looking the certificate up, so a CA that is not in a state in which it could have issued anything is reported as a missing certificate rather than as a CA in the wrong state.

AWS: Smithy (com-amazonaws-acmpca-acmprivateca) models GetCertificate with BOTH InvalidStateException and ResourceNotFoundException (each errorKind=client, httpError 400). InvalidStateException is documented as "The state of the private CA does not allow this action to occur." AWS evaluates the CA-state precondition FIRST: when GetCertificate names a CA that has not reached a state in which certificates can exist (e.g. CREATING / PENDING_CERTIFICATE, before ImportCertificateAuthorityCertificate makes it ACTIVE), the call is refused with HTTP 400, Error.Code "InvalidStateException" and Error.Message "The certificate authority <CertificateAuthorityArn> is not in the correct state to have issued certificates." In that message the fixed text is "The certificate authority " ... " is not in the correct state to have issued certificates." and the only variable part is the CA ARN taken verbatim from the request's CertificateAuthorityArn. ResourceNotFoundException is reserved for a CA that IS in a usable state but where the requested CertificateArn does not exist. Recorded behavior confirms the ordering: the same GetCertificate call against the same CA succeeds and returns a PEM body once that CA has been activated, so the earlier refusal is the CA-readiness gate and not a missing-certificate condition.

Emulator: the emulator skips the state gate and resolves the CertificateArn directly against its certificate store. For a CA that has issued nothing it answers Error.Code "ResourceNotFoundException" with Error.Message "Could not find specified certificate." — a different error code and a message with none of AWS's fixed text and no CA ARN interpolated.

Why: a client cannot distinguish "this CA is not ready yet" from "this certificate does not exist". The first is a transient lifecycle state a caller is expected to wait out (alongside RequestInProgressException), the second is terminal. Provisioning code that polls GetCertificate while a CA comes up, or that branches on InvalidStateException to retry and on ResourceNotFoundException to abort, aborts a CA bring-up that would have succeeded. Error handlers that match on the documented message text never fire, because the emulator's message shares no substring with AWS's.

Fix: belongs in the acm-pca GetCertificate handler's precondition/validation step — add the CA-state check ahead of the certificate lookup, so a CA whose status cannot yet have issued certificates raises InvalidStateException with AWS's message template (fixed text plus the request's CertificateAuthorityArn), and leave ResourceNotFoundException for an unknown CertificateArn on a CA that is in a usable state.

Traces: acm-pca.get-certificate-not-ready-fails (its assertion at check_acm_pca_get_certificate_not_ready_fails.py line 213 compares the captured (Error.Code, Error.Message) tuple against the pair above and reports ('ResourceNotFoundException', 'Could not find specified certificate.')).

## Evidence
Check acm-pca.get-certificate-not-ready-fails, verdict mismatch.

Request: GetCertificate(CertificateAuthorityArn=<arn of a freshly created CA that has issued nothing>, CertificateArn=<that arn>+"/certificate/<32 hex chars>").

Expected (AWS): HTTP 400, Error.Code = "InvalidStateException", Error.Message = "The certificate authority <CertificateAuthorityArn> is not in the correct state to have issued certificates." — fixed text as quoted, CA ARN taken from the request.

Actual (emulator): Error.Code = "ResourceNotFoundException", Error.Message = "Could not find specified certificate."

AssertionError from the replay:
  File "check_acm_pca_get_certificate_not_ready_fails.py", line 213, in run
    assert refusal == (
  AssertionError: ('ResourceNotFoundException', 'Could not find specified certificate.')

Restore beat in the same check passes: after _activate_root_ca drives the CA to ACTIVE, the identical GetCertificate on the CA's own certificate ARN returns a body beginning "-----BEGIN CERTIFICATE-----". So the refusal is the CA-readiness gate, not a blanket failure of credentials, wiring or the operation.

Model evidence: Smithy GetCertificate errors = [InvalidArnException, InvalidStateException, RequestFailedException, RequestInProgressException, ResourceNotFoundException]; InvalidStateException doc: "The state of the private CA does not allow this action to occur."

## Steps to Reproduce
1. CreateCertificateAuthority with CertificateAuthorityType="ROOT", UsageMode="SHORT_LIVED_CERTIFICATE", KeyAlgorithm="RSA_2048", SigningAlgorithm="SHA256WITHRSA" and any Subject.CommonName. Do NOT issue or import anything.
2. While that CA is still in a pre-ACTIVE state (it has issued nothing), call GetCertificate(CertificateAuthorityArn=<that CA arn>, CertificateArn=<that CA arn> + "/certificate/0123456789abcdef0123456789abcdef").
3. Observe the error: the emulator returns Error.Code "ResourceNotFoundException" / Error.Message "Could not find specified certificate."; AWS returns Error.Code "InvalidStateException" / Error.Message "The certificate authority <that CA arn> is not in the correct state to have issued certificates."
4. Control that must keep working: drive the CA to ACTIVE (GetCertificateAuthorityCsr -> IssueCertificate with the RootCACertificate/V1 template -> GetCertificate for the signed root -> ImportCertificateAuthorityCertificate), then call GetCertificate for that real certificate ARN and confirm it still returns a PEM certificate, and that an unknown CertificateArn on the now-ACTIVE CA still yields ResourceNotFoundException.
5. Re-replay acm-pca.get-certificate-not-ready-fails.
