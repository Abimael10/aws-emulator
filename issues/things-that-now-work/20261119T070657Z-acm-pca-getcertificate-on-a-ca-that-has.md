Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm-pca.get-certificate-not-ready-fails
Issue-Shape-Id: com.amazonaws.acmpca#ACMPrivateCA
Issue-Key: acm-pca.get-certificate-not-ready-fails|run#4|AssertionError|assert refusal == (
Issue-Commit: 8a8a1935decfd76385b56eadd04b9b4f60f5b4c5

# acm-pca GetCertificate on a CA that has not yet issued anything answers ResourceNotFoundException instead of InvalidStateException

> _Note: this issue was identified against the code base at commit `8a8a1935decfd76385b56eadd04b9b4f60f5b4c5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCertificate

## Description
Cause 1 of 1: GetCertificate does not gate on the private CA's readiness state before resolving the certificate ARN, so a CA that cannot yet have issued certificates (CREATING / PENDING_CERTIFICATE, i.e. no CA certificate imported) is reported as a missing certificate rather than as a CA in the wrong state.

AWS: GetCertificate models InvalidStateException (HTTP 400, "The state of the private CA does not allow this action to occur.") alongside ResourceNotFoundException — Smithy: GetCertificate errors = InvalidArnException, InvalidStateException, RequestFailedException, RequestInProgressException, ResourceNotFoundException, all httpError 400. The operation's documentation states a certificate is retrievable only once it is ISSUED, EXPIRED or REVOKED, and a CA that has not been activated has no issuance history at all. Recorded behavior: against a freshly created ROOT CA (not yet activated), GetCertificate with a syntactically valid certificate ARN under that CA is refused with error code InvalidStateException and the message "The certificate authority <CertificateAuthorityArn> is not in the correct state to have issued certificates." — the fixed text is "The certificate authority " ... " is not in the correct state to have issued certificates." and the single variable part is the CertificateAuthorityArn taken verbatim from the request. The CA-state refusal takes precedence over the per-certificate lookup. The same operation on the same CA succeeds (200, PEM body) once the CA has been driven to ACTIVE and asked for a certificate it really issued, so the refusal is tied to CA readiness and not to the ARN being unknown.

Emulator: the handler skips any CA-state check and goes straight to the certificate map, answering ResourceNotFoundException with the generic message "Could not find specified certificate." The check observed the pair ('ResourceNotFoundException', 'Could not find specified certificate.') where ('InvalidStateException', 'The certificate authority <arn> is not in the correct state to have issued certificates.') was required. The handler's comments make this explicit — it intentionally treats authority readiness as never a reason a certificate could not be resolved — which is the divergence, not an incidental omission.

Why: clients distinguish these two codes to decide whether to wait or to give up. A caller polling for issuance, or a provisioning workflow that branches on InvalidStateException to finish activating its CA before retrying, sees a terminal ResourceNotFoundException instead and aborts or reports a bogus "certificate missing" condition; conversely retry logic keyed on ResourceNotFoundException will spin against a CA that will never answer until it is activated. Negative-path tests that assert AWS's state refusal fail against the emulator.

Fix: the GetCertificate handler's pre-lookup validation in the acm-pca service crate — resolve the CA record first and, when its status is one in which it cannot have issued certificates (CREATING, PENDING_CERTIFICATE), return InvalidStateException with AWS's message built from the request's CertificateAuthorityArn, before consulting the CA's certificate map. The existing ResourceNotFoundException path stays correct for an unknown certificate under a CA that IS in an issuing state.

Traces: acm-pca.get-certificate-not-ready-fails. Relevant source: services/service-com-amazonaws-acmpca-acmprivateca/src/ops/get_certificate.rs lines 200-218 (CA record fetched at 200, certificate map consulted at 214 with no status gate in between).

## Evidence
Check acm-pca.get-certificate-not-ready-fails, assertion at check_acm_pca_get_certificate_not_ready_fails.py:213.

Request: GetCertificate{CertificateAuthorityArn=<arn of a ROOT CA created moments earlier, UsageMode SHORT_LIVED_CERTIFICATE, not yet activated>, CertificateArn=<that same CA arn> + "/certificate/<32 hex chars>"}.

Expected (AWS): HTTP 400, Error.Code = "InvalidStateException", Error.Message = "The certificate authority <CertificateAuthorityArn> is not in the correct state to have issued certificates." (fixed text with the request's CA ARN interpolated).

Actual (emulator): HTTP 400, Error.Code = "ResourceNotFoundException", Error.Message = "Could not find specified certificate."

AssertionError reported: ('ResourceNotFoundException', 'Could not find specified certificate.')

Restore beat in the same check passes: after GetCertificateAuthorityCsr -> IssueCertificate(RootCACertificate/V1) -> ImportCertificateAuthorityCertificate drives the CA to ACTIVE, GetCertificate on the CA's own certificate ARN returns 200 with a "-----BEGIN CERTIFICATE-----" PEM body, so the earlier divergence is confined to the not-ready CA state.

## Steps to Reproduce
1. CreateCertificateAuthority(CertificateAuthorityType="ROOT", UsageMode="SHORT_LIVED_CERTIFICATE", CertificateAuthorityConfiguration={KeyAlgorithm="RSA_2048", SigningAlgorithm="SHA256WITHRSA", Subject={CommonName=<any>}}) and capture CertificateAuthorityArn. Do NOT activate it.
2. Call GetCertificate(CertificateAuthorityArn=<that arn>, CertificateArn=<that arn> + "/certificate/0123456789abcdef0123456789abcdef").
3. Observe the returned Error.Code/Error.Message: emulator gives ResourceNotFoundException / "Could not find specified certificate."; AWS gives InvalidStateException / "The certificate authority <that arn> is not in the correct state to have issued certificates."
4. Activate the CA (GetCertificateAuthorityCsr -> IssueCertificate with arn:aws:acm-pca:::template/RootCACertificate/V1 -> ImportCertificateAuthorityCertificate, poll to Status=ACTIVE), then call GetCertificate for the root certificate ARN: this succeeds on both, confirming the divergence is the missing CA-state gate.
