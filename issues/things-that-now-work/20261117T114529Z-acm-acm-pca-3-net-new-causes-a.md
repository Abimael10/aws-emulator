Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__acm-pca.private-ca-certificate-export-chain-cross-checked-wrong-arn-rejected acm__acm-pca.request-certificate-private-ca-type-and-export acm__acm-pca.request-certificate-short-lived-ca-usage-mode-rejected
Issue-Key: acm|ExportCertificate|ValidationException
Issue-Key: acm__acm-pca.request-certificate-private-ca-type-and-export|run#9|AssertionError|assert cert["Type"] == "PRIVATE", (
Issue-Key: acm__acm-pca.request-certificate-short-lived-ca-usage-mode-rejected|run#8|AssertionError|assert short_cert["Status"] == "FAILED", (
Issue-Commit: 14940b74adde738ffb545252b71aede81788f071

# acm__acm-pca: 3 net-new causes — a certificate issued through a private CA reports Type AMAZON_ISSUED, ExportCertificate refuses it as "not an export enabled certificate", and a SHORT_LIVED_CERTIFICATE CA request settles ISSUED instead of FAILED

> _Note: this issue was identified against the code base at commit `14940b74adde738ffb545252b71aede81788f071`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RequestCertificate

## Description
Three net-new root causes in ACM's private-CA (ACM Private CA) issuance path. All three are reached by RequestCertificate with a CertificateAuthorityArn naming an ACTIVE ROOT private CA; the emulator treats such a request as if it were an ordinary public, Amazon-issued request.

Cause 1 of 3: DescribeCertificate reports Type AMAZON_ISSUED for a certificate requested through a private CA.
AWS: Smithy (acm) — RequestCertificate.CertificateAuthorityArn targets PcaArn (pattern ^arn:[\w+=/,.@-]+:acm-pca:...$) and names the ACM Private CA that issues the certificate; CertificateDetail.Type targets the CertificateType enum whose values are IMPORTED | AMAZON_ISSUED | PRIVATE, and CertificateDetail.CertificateAuthorityArn is documented as "The Amazon Resource Name (ARN) of the private certificate authority (CA) that issued the certificate". A certificate ACM issues through a private CA is therefore Type PRIVATE, not AMAZON_ISSUED (AMAZON_ISSUED is reserved for certificates ACM's own public CA issues).
Emulator: DescribeCertificate on the ISSUED certificate returns Type 'AMAZON_ISSUED'.
Why: a client cannot tell a private certificate from a public one, so it takes the public code path — it will not call ExportCertificate, will expect managed public renewal, and will mis-render the certificate's provenance; anything keyed on Type == "PRIVATE" never fires.
Fix: the ACM RequestCertificate handler / certificate state — a request carrying CertificateAuthorityArn must record the issuing CA and settle the certificate as Type PRIVATE (and echo CertificateAuthorityArn on DescribeCertificate).
Traces: acm__acm-pca.request-certificate-private-ca-type-and-export (also asserted as the tail of acm__acm-pca.request-certificate-short-lived-ca-usage-mode-rejected).

Cause 2 of 3: ExportCertificate refuses a private-CA certificate with 400 ValidationException instead of exporting it.
AWS: Smithy (acm) — ExportCertificate takes CertificateArn + Passphrase and returns Certificate, CertificateChain and the passphrase-encrypted PrivateKey; its modeled errors are InvalidArnException, RequestInProgressException, ResourceNotFoundException, ThrottlingException and ValidationException (all HTTP 400). Exporting is exactly what a PRIVATE certificate supports: the exported chain's leading certificate is byte-identical to the issuing CA's certificate from acm-pca GetCertificateAuthorityCertificate, that chain certificate's public key verifies the leaf's signature, and the exported PrivateKey's public key matches the leaf's SubjectPublicKeyInfo. Refusal is reserved for a certificate that is not exportable (public AMAZON_ISSUED / IMPORTED).
Emulator: 400 ValidationException with Message "Certificate ARN: <certificate arn> is not an export enabled certificate" — fixed text "Certificate ARN: " ... " is not an export enabled certificate", the ARN taken from the request. No certificate, chain or key is ever served.
Why: a client that provisions a private certificate for a non-AWS endpoint (or any offline consumer) can never obtain its key material; the whole private-CA export workflow is unavailable, and the refusal class (ValidationException) tells the client its request was malformed when it was not.
Fix: the ACM ExportCertificate handler and its exportability predicate — a certificate whose issuing authority is an ACM Private CA must be exportable, and the serializer must emit the PEM certificate, the CA-led chain, and the passphrase-encrypted private key for the leaf's own key pair.
Traces: acm__acm-pca.private-ca-certificate-export-chain-cross-checked-wrong-arn-rejected (the same check's unknown-ARN leg, which must answer ResourceNotFoundException "Could not find certificate <arn>.", is never reached because the export above is refused first), acm__acm-pca.request-certificate-private-ca-type-and-export.

Cause 3 of 3: RequestCertificate against a SHORT_LIVED_CERTIFICATE-usage-mode private CA settles ISSUED instead of FAILED.
AWS: recorded behavior only — Smithy (acm) models FailureReason as an enum that does not list the recorded value, and CertificateDetail.FailureReason is documented as existing "only when the certificate status is FAILED". The recording shows ACM accepting the RequestCertificate call and then settling the certificate at Status FAILED with FailureReason PCA_NOT_SUPPORTED_VIA_ACM, because ACM cannot issue through a CA created with UsageMode SHORT_LIVED_CERTIFICATE; the identical request against a GENERAL_PURPOSE CA settles ISSUED.
Emulator: the certificate requested against the SHORT_LIVED_CERTIFICATE CA settles Status 'ISSUED' (and carries no FailureReason), i.e. the CA's UsageMode is never consulted.
Why: a client gets a certificate AWS would never issue, so a configuration error (pointing ACM at a short-lived-certificate CA) is silently rewarded with a usable certificate; code that branches on FAILED + PCA_NOT_SUPPORTED_VIA_ACM is unreachable and the failure surfaces later, out of context.
Fix: the ACM RequestCertificate issuance path — when resolving CertificateAuthorityArn, read the CA's UsageMode and settle the certificate FAILED with FailureReason PCA_NOT_SUPPORTED_VIA_ACM for SHORT_LIVED_CERTIFICATE, leaving GENERAL_PURPOSE on the issuing path.
Traces: acm__acm-pca.request-certificate-short-lived-ca-usage-mode-rejected.

## Evidence
Cause 1 — expected: DescribeCertificate.Certificate.Type == "PRIVATE" for a certificate requested with CertificateAuthorityArn. Actual: AssertionError "expected Type PRIVATE for a certificate requested with CertificateAuthorityArn, got 'AMAZON_ISSUED'" (check_acm__acm_pca_request_certificate_private_ca_type_and_export.py line 192).

Cause 2 — expected: ExportCertificate(CertificateArn=<private cert>, Passphrase=...) returns 200 with Certificate / CertificateChain (leading cert byte-identical to acm-pca GetCertificateAuthorityCertificate) / PrivateKey matching the leaf's public key. Actual: botocore.errorfactory.ValidationException: An error occurred (ValidationException) when calling the ExportCertificate operation: Certificate ARN: arn:aws:acm:<region>:<account>:certificate/<id> is not an export enabled certificate. (HTTP 400, Error.Code ValidationException.)

Cause 3 — expected: Status FAILED with FailureReason PCA_NOT_SUPPORTED_VIA_ACM for a request against a SHORT_LIVED_CERTIFICATE-mode CA, while the GENERAL_PURPOSE control reaches ISSUED/PRIVATE. Actual: AssertionError "expected a certificate requested against a SHORT_LIVED_CERTIFICATE CA to settle FAILED, got 'ISSUED'" (check_acm__acm_pca_request_certificate_short_lived_ca_usage_mode_rejected.py line 196).

## Steps to Reproduce
1. acm-pca CreateCertificateAuthority (CertificateAuthorityType=ROOT, UsageMode=GENERAL_PURPOSE), GetCertificateAuthorityCsr, IssueCertificate with the RootCACertificate/V1 template, GetCertificate, ImportCertificateAuthorityCertificate; poll DescribeCertificateAuthority to ACTIVE.
2. acm RequestCertificate(DomainName=<x>.example.com, CertificateAuthorityArn=<that CA>); poll DescribeCertificate to ISSUED.
3. Read Certificate.Type — the emulator reports AMAZON_ISSUED where AWS reports PRIVATE (cause 1).
4. Call acm ExportCertificate(CertificateArn=<that certificate>, Passphrase=b"...") — the emulator answers 400 ValidationException "... is not an export enabled certificate" instead of serving the certificate, the CA-led chain and the encrypted private key (cause 2).
5. Repeat step 1 with UsageMode=SHORT_LIVED_CERTIFICATE, then step 2 against that CA and poll to a terminal status — the emulator settles ISSUED where AWS settles FAILED with FailureReason PCA_NOT_SUPPORTED_VIA_ACM (cause 3).
